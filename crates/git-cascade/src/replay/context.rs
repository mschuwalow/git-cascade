use super::ReplayOutcome;
use super::backend::{CherryPickOutcome, ReplayBackend};
use super::state::{CurrentState, PausedState, Phase, ReplayState};
use super::state_writer::StateWriter;
use crate::model::Strategy;
use crate::plan::{Node, Plan, PlanCommit};
use crate::test_hooks;
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(super) struct ReplayContext<'plan, 'state> {
    plan: &'plan Plan,
    state_writer: &'state mut dyn StateWriter,
    backend: &'state mut dyn ReplayBackend,
    state: ReplayState,
    nodes: HashMap<String, usize>,
    temp_tips: HashMap<String, String>,
    selected_bases: HashMap<String, String>,
}

enum CommitReplay {
    Continue(String),
    Stop,
}

impl<'plan, 'state> ReplayContext<'plan, 'state> {
    pub(super) fn new(
        plan: &'plan Plan,
        state_writer: &'state mut dyn StateWriter,
        backend: &'state mut dyn ReplayBackend,
        state: ReplayState,
    ) -> Result<Self> {
        let nodes = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.branch.clone(), index))
            .collect::<HashMap<_, _>>();
        let temp_tips = backend.temp_tips(&state.completed_temp_refs)?;
        let selected_bases = selected_bases_from_mappings(plan, &state.mappings);

        Ok(Self {
            plan,
            state_writer,
            backend,
            state,
            nodes,
            temp_tips,
            selected_bases,
        })
    }

    pub(super) fn run(&mut self) -> Result<ReplayOutcome> {
        self.backend.start(&self.state)?;
        loop {
            match &self.state.phase {
                Phase::Replay { .. } => {
                    self.replay_pending_branches()?;
                }
                Phase::FinalUpdate => {
                    self.backend.final_update(self.plan, &self.state)?;
                    test_hooks::run("after-final-update")?;
                    self.state.phase = Phase::Deleting { delete_plan: true };
                    self.write_state()?;
                    test_hooks::run("after-deleting-state-written")?;
                }
                Phase::Conflict { current, message } => {
                    return Ok(ReplayOutcome::Conflict {
                        current: current.clone(),
                        message: message.clone(),
                    });
                }
                Phase::ContinueAfterConflict { current } => {
                    self.resolve_conflict(current.clone())?;
                    self.write_state()?;
                }
                Phase::Paused { paused } => {
                    return Ok(ReplayOutcome::Paused {
                        paused: paused.clone(),
                    });
                }
                Phase::ContinueAfterPause { paused } => {
                    self.resume_paused_branch(paused.clone())?
                }
                Phase::Deleting { .. } => return Ok(ReplayOutcome::Complete),
            }
        }
    }

    pub(super) fn into_state(self) -> ReplayState {
        self.state
    }

    pub(super) fn continue_after_pause_or_conflict(&mut self) {
        match &self.state.phase {
            Phase::Conflict { current, .. } => {
                self.state.phase = Phase::ContinueAfterConflict {
                    current: current.clone(),
                };
            }
            Phase::Paused { paused } => {
                self.state.phase = Phase::ContinueAfterPause {
                    paused: paused.clone(),
                };
            }
            _ => {}
        };
    }

    fn replay_pending_branches(&mut self) -> Result<()> {
        if self.total_branches() == 0 {
            self.backend.no_branches()?;
        }

        while let Some(branch) = self.state.pending_branches.first().cloned() {
            if self.replay_branch(branch)? {
                return Ok(());
            }
        }

        self.state.phase = Phase::FinalUpdate;
        self.write_state()?;
        Ok(())
    }

    fn replay_branch(&mut self, branch: String) -> Result<bool> {
        let node = self.node(&branch)?.clone();
        let branch_index = self.branch_index();
        let commits = replay_commits_from_extra(&node, &self.state.extra_commits);
        let replay_current = self.replay_current();
        let was_resuming = replay_current.is_some();
        let child_base_pause_commits = if self.state.replay_mode.pauses_at_checkpoints() {
            child_base_pause_commits(self.plan, &node, self.state.strategy, &commits)
        } else {
            BTreeSet::new()
        };

        let (start_commit_index, mut last_rewritten) = if let Some(current) = replay_current {
            let start = self.resume_start_commit_index(&node, &current, &commits)?;
            self.state.phase = Phase::Replay { current: None };
            let head = commits
                .get(start.wrapping_sub(1))
                .and_then(|commit| self.state.mappings.get(&commit.oid))
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!(
                        "branch `{}` has no rewritten commit to resume from",
                        node.branch
                    ))
                })?;
            (start, head)
        } else {
            let base = self.actual_replay_base(&node)?;
            self.selected_bases
                .insert(node.branch.clone(), base.clone());
            self.state
                .mappings
                .insert(node.base().to_owned(), base.clone());

            if base == node.base() {
                self.skip_replay_at_existing_base(&node, &branch, &commits)?;
                self.state.phase = Phase::Replay { current: None };
                self.write_state()?;
                return Ok(false);
            }

            self.backend.prepare_branch(
                &self.state,
                branch_index,
                self.total_branches(),
                &node,
                &base,
            )?;
            (0, base)
        };

        self.backend.start_replay(
            branch_index,
            self.total_branches(),
            &node,
            commits.len(),
            start_commit_index,
            was_resuming,
        )?;
        for (commit_index, commit) in commits.iter().enumerate().skip(start_commit_index) {
            match self.replay_commit(
                &node,
                commit,
                commit_index,
                commits.len(),
                &last_rewritten,
                &child_base_pause_commits,
            )? {
                CommitReplay::Continue(rewritten_commit) => last_rewritten = rewritten_commit,
                CommitReplay::Stop => return Ok(true),
            }
        }

        self.finish_branch(&node, &branch, &commits, branch_index)
    }

    fn replay_commit(
        &mut self,
        node: &Node,
        commit: &PlanCommit,
        commit_index: usize,
        total_commits: usize,
        last_rewritten: &str,
        child_base_pause_commits: &BTreeSet<String>,
    ) -> Result<CommitReplay> {
        if commit.is_merge() {
            // The merged history is contained in the new base; flatten.
            self.backend
                .flatten_merge(node, &commit.oid, commit_index, total_commits)?;
            self.state
                .mappings
                .insert(commit.oid.clone(), last_rewritten.to_owned());
            if child_base_pause_commits.contains(&commit.oid) {
                self.pause_at_child_base(node, &commit.oid, last_rewritten)?;
                return Ok(CommitReplay::Stop);
            }
            return Ok(CommitReplay::Continue(last_rewritten.to_owned()));
        }

        let rewritten_commit = match self.backend.cherry_pick(
            &self.state,
            node,
            &commit.oid,
            commit_index,
            total_commits,
        )? {
            CherryPickOutcome::Applied(rewritten_commit) => rewritten_commit,
            CherryPickOutcome::Conflict { message } => {
                let current = CurrentState {
                    branch: node.branch.clone(),
                    commit: commit.oid.clone(),
                    worktree: self.state.worktree.path().to_owned(),
                };
                self.state.phase = Phase::Conflict {
                    current: current.clone(),
                    message,
                };
                self.write_state()?;
                return Ok(CommitReplay::Stop);
            }
        };
        self.state
            .mappings
            .insert(commit.oid.clone(), rewritten_commit.clone());
        if child_base_pause_commits.contains(&commit.oid) {
            self.pause_at_child_base(node, &commit.oid, &rewritten_commit)?;
            return Ok(CommitReplay::Stop);
        }
        Ok(CommitReplay::Continue(rewritten_commit))
    }

    fn pause_at_child_base(
        &mut self,
        node: &Node,
        commit: &str,
        rewritten_tip: &str,
    ) -> Result<()> {
        let paused = PausedState::ChildBase {
            branch: node.branch.clone(),
            commit: commit.to_owned(),
            rewritten_tip: rewritten_tip.to_owned(),
            worktree: self.state.worktree.path().to_owned(),
        };
        self.state.phase = Phase::Paused { paused };
        self.write_state()
    }

    fn finish_branch(
        &mut self,
        node: &Node,
        branch: &str,
        commits: &[PlanCommit],
        branch_index: usize,
    ) -> Result<bool> {
        let rewritten_tip = if let Some(commit) = commits.last() {
            self.state
                .mappings
                .get(&commit.oid)
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!(
                        "commit `{}` for branch `{}` has no rewritten mapping",
                        commit.oid, node.branch
                    ))
                })?
        } else {
            self.selected_bases
                .get(&node.branch)
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!("branch `{}` has no selected base", node.branch))
                })?
        };
        let (temp_ref, branch_tip) = self.backend.write_temp_ref(
            self.plan,
            node,
            branch_index,
            self.total_branches(),
            &rewritten_tip,
        )?;
        self.record_temp_ref(&node.branch, temp_ref.clone(), branch_tip.clone());
        self.remove_pending_branch(branch)?;
        if self.state.replay_mode.pauses_at_checkpoints() {
            let paused = PausedState::BranchEnd {
                branch: node.branch.clone(),
                rewritten_tip: branch_tip,
                temp_ref,
                mapped_commit: commits
                    .last()
                    .map(|commit| commit.oid.clone())
                    .unwrap_or_else(|| node.base().to_owned()),
                worktree: self.state.worktree.path().to_owned(),
            };
            self.state.phase = Phase::Paused { paused };
            self.write_state()?;
            return Ok(true);
        }
        self.state.phase = Phase::Replay { current: None };
        self.write_state()?;
        Ok(false)
    }

    fn resolve_conflict(&mut self, current: CurrentState) -> Result<()> {
        let rewritten_commit = self.backend.continue_cherry_pick(&self.state, &current)?;
        self.state
            .mappings
            .insert(current.commit.clone(), rewritten_commit);
        self.state.phase = Phase::Replay {
            current: Some(current),
        };
        self.write_state()?;
        Ok(())
    }

    fn resume_paused_branch(&mut self, paused: PausedState) -> Result<()> {
        if !self
            .plan
            .nodes
            .iter()
            .any(|node| node.branch == paused.branch())
        {
            return Err(Error::InvalidPlan(format!(
                "paused branch `{}` is not in the active plan",
                paused.branch()
            )));
        }

        let rewritten_tip = self.backend.resume_paused_branch(&self.state, &paused)?;
        match paused {
            PausedState::BranchEnd {
                branch,
                mapped_commit,
                temp_ref,
                ..
            } => {
                self.record_temp_ref(&branch, temp_ref, rewritten_tip.clone());
                self.state.mappings.insert(mapped_commit, rewritten_tip);
                self.state.phase = Phase::Replay { current: None };
            }
            PausedState::ChildBase {
                branch,
                commit,
                worktree,
                ..
            } => {
                self.state.mappings.insert(commit.clone(), rewritten_tip);
                self.state.phase = Phase::Replay {
                    current: Some(CurrentState {
                        branch,
                        commit,
                        worktree,
                    }),
                };
            }
        }
        self.write_state()?;
        Ok(())
    }

    fn skip_replay_at_existing_base(
        &mut self,
        node: &Node,
        branch: &str,
        commits: &[PlanCommit],
    ) -> Result<()> {
        for commit in commits {
            self.state
                .mappings
                .insert(commit.oid.clone(), commit.oid.clone());
        }
        let current_tip = commits
            .last()
            .map(|commit| commit.oid.clone())
            .ok_or_else(|| {
                Error::InvalidPlan(format!("branch `{}` has no commits", node.branch))
            })?;
        let (temp_ref, branch_tip) = self.backend.skip_replay(
            self.plan,
            node,
            self.branch_index(),
            self.total_branches(),
            &current_tip,
        )?;
        self.record_temp_ref(&node.branch, temp_ref, branch_tip);
        self.remove_pending_branch(branch)?;
        Ok(())
    }

    fn resume_start_commit_index(
        &self,
        node: &Node,
        current: &CurrentState,
        commits: &[PlanCommit],
    ) -> Result<usize> {
        if current.branch != node.branch {
            return Err(Error::InvalidPlan(format!(
                "current branch `{}` is not the next pending branch `{}`",
                current.branch, node.branch
            )));
        }
        if !self.state.mappings.contains_key(&current.commit) {
            return Err(Error::InvalidPlan(format!(
                "current commit `{}` for branch `{}` has no rewritten mapping",
                current.commit, current.branch
            )));
        }

        commits
            .iter()
            .position(|commit| commit.oid == current.commit)
            .map(|index| index + 1)
            .ok_or_else(|| {
                Error::InvalidPlan(format!(
                    "current commit `{}` is not part of branch `{}`",
                    current.commit, current.branch
                ))
            })
    }

    fn remove_pending_branch(&mut self, branch: &str) -> Result<()> {
        if self.state.pending_branches.first().map(String::as_str) != Some(branch) {
            return Err(Error::InvalidPlan(format!(
                "completed branch `{branch}` is not first in pending state"
            )));
        }
        self.state.pending_branches.remove(0);
        Ok(())
    }

    fn actual_replay_base(&self, node: &Node) -> Result<String> {
        if node.is_root() {
            return Ok(self.state.new_tip.clone());
        }

        let parent_branch = node.parent().ok_or_else(|| {
            Error::InvalidPlan(format!("root node `{}` has no branch parent", node.branch))
        })?;
        let parent = self.node(parent_branch)?;

        if self.state.strategy == Strategy::MoveToPlannedTips {
            return self
                .state
                .mappings
                .get(&parent.tip)
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!(
                        "parent `{}` has no rewritten planned tip",
                        parent.branch
                    ))
                });
        }

        if self.state.strategy == Strategy::MoveToCurrentTips {
            return self.temp_tips.get(&parent.branch).cloned().ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no rewritten tip", parent.branch))
            });
        }

        let base = node.base();
        if base == parent.base() {
            return self
                .selected_bases
                .get(&parent.branch)
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!("parent `{}` has no selected base", parent.branch))
                });
        }

        self.state.mappings.get(base).cloned().ok_or_else(|| {
            Error::InvalidPlan(format!(
                "base `{}` for branch `{}` was not mapped",
                base, node.branch
            ))
        })
    }

    fn record_temp_ref(&mut self, branch: &str, temp_ref: String, branch_tip: String) {
        self.temp_tips.insert(branch.to_owned(), branch_tip);
        if !self.state.completed_temp_refs.contains(&temp_ref) {
            self.state.completed_temp_refs.push(temp_ref);
        }
    }

    fn node(&self, branch: &str) -> Result<&Node> {
        let index = self
            .nodes
            .get(branch)
            .ok_or_else(|| Error::InvalidPlan(format!("unknown branch `{branch}`")))?;
        self.plan
            .nodes
            .get(*index)
            .ok_or_else(|| Error::InvalidPlan(format!("unknown branch `{branch}`")))
    }

    fn replay_current(&self) -> Option<CurrentState> {
        match &self.state.phase {
            Phase::Replay { current } => current.clone(),
            _ => None,
        }
    }

    fn branch_index(&self) -> usize {
        self.state.completed_temp_refs.len() + 1
    }

    fn total_branches(&self) -> usize {
        self.plan.nodes.len()
    }

    fn write_state(&mut self) -> Result<()> {
        self.state_writer.write_state(&mut self.state)
    }
}

fn replay_commits_from_extra(
    node: &Node,
    extra_commits: &BTreeMap<String, Vec<PlanCommit>>,
) -> Vec<PlanCommit> {
    let mut commits = node.commits().to_vec();
    if let Some(extra) = extra_commits.get(&node.branch) {
        commits.extend(extra.iter().cloned());
    }
    commits
}

fn child_base_pause_commits(
    plan: &Plan,
    node: &Node,
    strategy: Strategy,
    commits: &[PlanCommit],
) -> BTreeSet<String> {
    let Some(last_commit) = commits.last() else {
        return BTreeSet::new();
    };
    let has_child = plan
        .nodes
        .iter()
        .any(|child| child.parent() == Some(node.branch.as_str()));
    if !has_child {
        return BTreeSet::new();
    }

    let commit_oids = commits
        .iter()
        .map(|commit| commit.oid.as_str())
        .collect::<BTreeSet<_>>();
    match strategy {
        Strategy::MoveToCurrentTips => BTreeSet::new(),
        Strategy::MoveToPlannedTips => {
            if node.tip != last_commit.oid && commit_oids.contains(node.tip.as_str()) {
                BTreeSet::from([node.tip.clone()])
            } else {
                BTreeSet::new()
            }
        }
        Strategy::PreserveForkPoints => plan
            .nodes
            .iter()
            .filter(|child| child.parent() == Some(node.branch.as_str()))
            .map(Node::base)
            .filter(|base| *base != node.base())
            .filter(|base| *base != last_commit.oid)
            .filter(|base| commit_oids.contains(*base))
            .map(str::to_owned)
            .collect(),
    }
}

fn selected_bases_from_mappings(
    plan: &Plan,
    mappings: &BTreeMap<String, String>,
) -> HashMap<String, String> {
    plan.nodes
        .iter()
        .filter_map(|node| {
            mappings
                .get(node.base())
                .map(|base| (node.branch.clone(), base.clone()))
        })
        .collect()
}
