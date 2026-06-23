use super::backend::{CherryPickOutcome, ReplayBackend, RequiredAncestor, temp_ref};
use super::state::{CurrentState, PauseReason, PausedKind, PausedState, Phase, ReplayState};
use super::state_writer::StateWriter;
use super::strategy;
use super::{ReplayOutcome, replay_commits_from_extra};
use crate::model::{BranchName, CommitId, GitRef, Strategy};
use crate::plan::{Node, Plan, PlanCommit};
use crate::test_hooks;
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(super) struct ReplayContext<'plan, 'state, B, W>
where
    B: ReplayBackend,
    W: StateWriter,
{
    plan: &'plan Plan,
    state_writer: &'state mut W,
    backend: &'state mut B,
    state: ReplayState,
    temp_tips: HashMap<BranchName, CommitId>,
}

struct BranchReplayStart {
    commit_index: usize,
    last_rewritten: CommitId,
    was_resuming: bool,
}

impl<'plan, 'state, B, W> ReplayContext<'plan, 'state, B, W>
where
    B: ReplayBackend,
    W: StateWriter,
{
    pub(super) fn new(
        plan: &'plan Plan,
        state_writer: &'state mut W,
        backend: &'state mut B,
        state: ReplayState,
    ) -> Result<Self> {
        let temp_tips = backend.temp_tips(&state.completed_temp_refs)?;

        Ok(Self {
            plan,
            state_writer,
            backend,
            state,
            temp_tips,
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
                    self.state.phase = Phase::RestoreCheckout {
                        delete_plan: true,
                        force_checkout: false,
                    };
                    self.write_state()?;
                }
                Phase::RestoreCheckout {
                    delete_plan,
                    force_checkout,
                } => {
                    let delete_plan = *delete_plan;
                    let force_checkout = *force_checkout;
                    self.backend.restore_checkout(&self.state, force_checkout)?;
                    self.state.phase = Phase::Deleting { delete_plan };
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

    fn replay_branch(&mut self, branch: BranchName) -> Result<bool> {
        let node = self.node(branch.as_str())?.clone();
        let branch_index = self.branch_index();
        let commits = replay_commits_from_extra(&node, &self.state.extra_commits);
        let mut start = self.prepare_branch_replay(&node, &commits, branch_index)?;

        self.backend.start_replay(
            branch_index,
            self.total_branches(),
            &node,
            commits.len(),
            start.commit_index,
            start.was_resuming,
        )?;
        for (commit_index, commit) in commits.iter().enumerate().skip(start.commit_index) {
            let Some(last_rewritten) = self.replay_commit(
                &node,
                commit,
                commit_index,
                commits.len(),
                &start.last_rewritten,
                branch_index,
            )?
            else {
                return Ok(true);
            };
            start.last_rewritten = last_rewritten;
        }

        self.finish_branch(&node, &commits, branch_index)
    }

    fn prepare_branch_replay(
        &mut self,
        node: &Node,
        commits: &[PlanCommit],
        branch_index: usize,
    ) -> Result<BranchReplayStart> {
        if let Some(current) = self.take_replay_current() {
            let commit_index = self.resume_start_commit_index(node, &current, commits)?;
            return Ok(BranchReplayStart {
                commit_index,
                last_rewritten: self.resume_last_rewritten(node, commits, commit_index)?,
                was_resuming: true,
            });
        }

        let base = self.actual_replay_base(node)?;
        self.state.mappings.insert(node.base.clone(), base.clone());
        if base != node.base {
            self.backend.prepare_branch(
                &self.state,
                branch_index,
                self.total_branches(),
                node,
                &base,
            )?;
        }

        Ok(BranchReplayStart {
            commit_index: 0,
            last_rewritten: base,
            was_resuming: false,
        })
    }

    fn resume_last_rewritten(
        &self,
        node: &Node,
        commits: &[PlanCommit],
        start_commit_index: usize,
    ) -> Result<CommitId> {
        commits
            .get(start_commit_index.wrapping_sub(1))
            .and_then(|commit| self.state.mappings.get(&commit.oid))
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!(
                    "branch `{}` has no rewritten commit to resume from",
                    node.branch
                ))
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn replay_commit(
        &mut self,
        node: &Node,
        commit: &PlanCommit,
        commit_index: usize,
        total_commits: usize,
        last_rewritten: &CommitId,
        branch_index: usize,
    ) -> Result<Option<CommitId>> {
        let Some(rewritten_commit) =
            self.rewrite_commit(node, commit, commit_index, total_commits, last_rewritten)?
        else {
            return Ok(None);
        };
        self.state
            .mappings
            .insert(commit.oid.clone(), rewritten_commit.clone());
        if let Some(pause_reasons) = self.state.pause_plan.commit_pause_reasons(&commit.oid) {
            let pause_reasons = pause_reasons.clone();
            self.pause_at_commit(
                node,
                branch_index,
                &commit.oid,
                &rewritten_commit,
                pause_reasons,
            )?;
            return Ok(None);
        }
        Ok(Some(rewritten_commit))
    }

    fn rewrite_commit(
        &mut self,
        node: &Node,
        commit: &PlanCommit,
        commit_index: usize,
        total_commits: usize,
        last_rewritten: &CommitId,
    ) -> Result<Option<CommitId>> {
        if self.can_keep_existing_commit(commit, last_rewritten) {
            return Ok(Some(commit.oid.clone()));
        }

        if commit.is_merge() {
            // The merged history is contained in the new base; flatten.
            self.backend
                .flatten_merge(node, &commit.oid, commit_index, total_commits)?;
            return Ok(Some(last_rewritten.clone()));
        }

        match self.backend.cherry_pick(
            &self.state,
            node,
            &commit.oid,
            commit_index,
            total_commits,
        )? {
            CherryPickOutcome::Applied(rewritten_commit) => Ok(Some(rewritten_commit)),
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
                Ok(None)
            }
        }
    }

    fn pause_at_commit(
        &mut self,
        node: &Node,
        branch_index: usize,
        commit: &CommitId,
        rewritten_tip: &CommitId,
        reasons: BTreeSet<PauseReason>,
    ) -> Result<()> {
        self.backend.prepare_branch(
            &self.state,
            branch_index,
            self.total_branches(),
            node,
            rewritten_tip,
        )?;
        let worktree = self.state.worktree.path().to_owned();
        let paused = PausedState {
            branch: node.branch.clone(),
            rewritten_tip: rewritten_tip.clone(),
            worktree,
            reasons,
            kind: PausedKind::MidBranch {
                commit: commit.clone(),
            },
        };
        self.state.phase = Phase::Paused { paused };
        self.write_state()
    }

    fn paused_branch_end_state(
        &self,
        node: &Node,
        mapped_commit: CommitId,
        branch_tip: CommitId,
        temp_ref: GitRef,
        reasons: BTreeSet<PauseReason>,
    ) -> PausedState {
        PausedState {
            branch: node.branch.clone(),
            rewritten_tip: branch_tip,
            worktree: self.state.worktree.path().to_owned(),
            reasons,
            kind: PausedKind::BranchEnd {
                temp_ref,
                mapped_commit,
            },
        }
    }

    fn finish_branch(
        &mut self,
        node: &Node,
        commits: &[PlanCommit],
        branch_index: usize,
    ) -> Result<bool> {
        let rewritten_tip = if let Some(commit) = commits.last() {
            self.mapped_commit(&commit.oid)?.clone()
        } else {
            self.branch_replay_base(node)?.clone()
        };

        let branch_replay_base = self.branch_replay_base(node)?.clone();
        let rewritten_tip = self.finalize_branch_tip(
            node,
            commits,
            branch_index,
            &branch_replay_base,
            rewritten_tip.clone(),
        )?;
        if let Some(last_commit) = commits.last() {
            self.state
                .mappings
                .insert(last_commit.oid.clone(), rewritten_tip.clone());
        }

        if let Some(reasons) = self.branch_end_pause_reasons(node) {
            let mapped_commit = commits
                .last()
                .map(|commit| commit.oid.clone())
                .unwrap_or_else(|| node.base.clone());
            self.pause_branch_end(node, mapped_commit, branch_index, &rewritten_tip, reasons)
        } else {
            self.complete_branch(node, branch_index, &rewritten_tip)
        }
    }

    fn finalize_branch_tip(
        &mut self,
        node: &Node,
        commits: &[PlanCommit],
        branch_index: usize,
        branch_replay_base: &CommitId,
        rewritten_tip: CommitId,
    ) -> Result<CommitId> {
        match self.state.strategy {
            Strategy::Squash if commits.len() > 1 => {
                let first_commit = commits
                    .first()
                    .expect("non-empty commits has a first commit")
                    .oid
                    .clone();
                self.backend.squash_branch(
                    &self.state,
                    node,
                    branch_index,
                    self.total_branches(),
                    branch_replay_base,
                    &first_commit,
                    &rewritten_tip,
                )
            }
            Strategy::PreserveForkPoints
            | Strategy::MoveToPlannedTips
            | Strategy::MoveToCurrentTips
            | Strategy::Squash => Ok(rewritten_tip),
        }
    }

    fn branch_end_pause_reasons(&self, node: &Node) -> Option<BTreeSet<PauseReason>> {
        self.state
            .pause_plan
            .branch_end_pause_reasons(&node.branch)
            .cloned()
    }

    fn complete_branch(
        &mut self,
        node: &Node,
        branch_index: usize,
        rewritten_tip: &CommitId,
    ) -> Result<bool> {
        let total_branches = self.total_branches();
        let (temp_ref, branch_tip) = if rewritten_tip == &node.tip {
            self.backend.skip_replay(
                self.plan,
                node,
                branch_index,
                total_branches,
                rewritten_tip,
            )?
        } else {
            self.backend.write_temp_ref(
                self.plan,
                node,
                branch_index,
                total_branches,
                rewritten_tip,
            )?
        };
        self.record_temp_ref(&node.branch, temp_ref, branch_tip);
        self.remove_pending_branch(&node.branch)?;
        self.state.phase = Phase::Replay { current: None };
        self.write_state()?;
        Ok(false)
    }

    fn pause_branch_end(
        &mut self,
        node: &Node,
        mapped_commit: CommitId,
        branch_index: usize,
        rewritten_tip: &CommitId,
        reasons: BTreeSet<PauseReason>,
    ) -> Result<bool> {
        self.backend.prepare_branch(
            &self.state,
            branch_index,
            self.total_branches(),
            node,
            rewritten_tip,
        )?;
        self.state.phase = Phase::Paused {
            paused: self.paused_branch_end_state(
                node,
                mapped_commit,
                rewritten_tip.clone(),
                temp_ref(self.plan, node.branch.as_str()),
                reasons,
            ),
        };
        self.write_state()?;
        Ok(true)
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
            .any(|node| node.branch.as_str() == paused.branch())
        {
            return Err(Error::InvalidPlan(format!(
                "paused branch `{}` is not in the active plan",
                paused.branch()
            )));
        }

        let required_ancestors = self.resume_requirements(&paused)?;
        let rewritten_tip =
            self.backend
                .resume_paused_branch(&self.state, &paused, &required_ancestors)?;
        match paused.kind {
            PausedKind::BranchEnd {
                temp_ref,
                mapped_commit,
            } => {
                let branch = paused.branch;
                self.record_temp_ref(&branch, temp_ref, rewritten_tip.clone());
                self.state.mappings.insert(mapped_commit, rewritten_tip);
                self.remove_pending_branch(&branch)?;
                self.state.phase = Phase::Replay { current: None };
            }
            PausedKind::MidBranch { commit } => {
                self.state.mappings.insert(commit.clone(), rewritten_tip);
                self.state.phase = Phase::Replay {
                    current: Some(CurrentState {
                        branch: paused.branch,
                        commit,
                        worktree: paused.worktree,
                    }),
                };
            }
        }
        self.write_state()?;
        Ok(())
    }

    fn resume_requirements(&self, paused: &PausedState) -> Result<Vec<RequiredAncestor>> {
        let node = self.node(paused.branch())?;
        if paused.is_branch_end() {
            return self.branch_end_resume_requirements(node);
        }

        let mut required = BTreeMap::<CommitId, String>::new();
        required.insert(
            self.branch_replay_base(node)?.clone(),
            format!("replay base for branch `{}`", node.branch),
        );
        required.insert(
            paused.rewritten_tip.clone(),
            format!("rewritten commit pause for `{}`", node.branch),
        );
        Ok(required
            .into_iter()
            .map(|(commit, reason)| RequiredAncestor { commit, reason })
            .collect())
    }

    fn branch_end_resume_requirements(&self, node: &Node) -> Result<Vec<RequiredAncestor>> {
        let mut required = BTreeMap::<CommitId, String>::new();
        required.insert(
            self.branch_replay_base(node)?.clone(),
            format!("replay base for branch `{}`", node.branch),
        );

        for child in self
            .plan
            .nodes
            .iter()
            .filter(|child| child.parent() == Some(node.branch.as_str()))
        {
            let Some((commit, reason)) = self.required_child_replay_base(node, child)? else {
                continue;
            };
            required.entry(commit).or_insert(reason);
        }

        Ok(required
            .into_iter()
            .map(|(commit, reason)| RequiredAncestor { commit, reason })
            .collect())
    }

    fn branch_replay_base(&self, node: &Node) -> Result<&CommitId> {
        self.state.mappings.get(&node.base).ok_or_else(|| {
            Error::InvalidPlan(format!("branch `{}` has no selected base", node.branch))
        })
    }

    fn required_child_replay_base(
        &self,
        parent: &Node,
        child: &Node,
    ) -> Result<Option<(CommitId, String)>> {
        strategy::required_child_replay_base(
            self.state.strategy,
            parent,
            child,
            &self.state.mappings,
        )
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

    fn actual_replay_base(&self, node: &Node) -> Result<CommitId> {
        if node.is_root() {
            return Ok(self.state.new_tip.clone());
        }

        let parent_branch = node.parent().ok_or_else(|| {
            Error::InvalidPlan(format!("root node `{}` has no branch parent", node.branch))
        })?;
        let parent = self.node(parent_branch)?;

        strategy::actual_child_base(
            self.state.strategy,
            parent,
            node,
            &self.state.mappings,
            &self.temp_tips,
        )
    }

    fn record_temp_ref(&mut self, branch: &BranchName, temp_ref: GitRef, branch_tip: CommitId) {
        self.temp_tips.insert(branch.clone(), branch_tip);
        if !self.state.completed_temp_refs.contains(&temp_ref) {
            self.state.completed_temp_refs.push(temp_ref);
        }
    }

    fn remove_pending_branch(&mut self, branch: &BranchName) -> Result<()> {
        if self.state.pending_branches.first() != Some(branch) {
            return Err(Error::InvalidPlan(format!(
                "completed branch `{branch}` is not first in pending state"
            )));
        }
        self.state.pending_branches.remove(0);
        Ok(())
    }

    fn node(&self, branch: &str) -> Result<&Node> {
        self.plan
            .nodes
            .iter()
            .find(|node| node.branch.as_str() == branch)
            .ok_or_else(|| Error::InvalidPlan(format!("unknown branch `{branch}`")))
    }

    fn take_replay_current(&mut self) -> Option<CurrentState> {
        match &mut self.state.phase {
            Phase::Replay { current } => current.take(),
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

    fn mapped_commit(&self, commit: &CommitId) -> Result<&CommitId> {
        self.state.mappings.get(commit).ok_or_else(|| {
            Error::InvalidPlan(format!("commit `{commit}` has no rewritten mapping"))
        })
    }

    fn can_keep_existing_commit(&self, commit: &PlanCommit, last_rewritten: &CommitId) -> bool {
        commit.parents.first() == Some(last_rewritten)
    }
}
