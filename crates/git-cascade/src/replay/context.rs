use super::backend::{CherryPickOutcome, ReplayBackend, RequiredAncestor, temp_ref};
use super::state::{BranchReplayState, PauseReason, PausedKind, PausedState, Phase, ReplayState};
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

enum CommitReplay {
    Continued(CommitId),
    Stopped,
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
                Phase::Replay => {
                    self.prepare_next_branch_replay()?;
                }
                Phase::ContinueReplay { replay } => {
                    self.continue_branch_replay(replay.clone())?;
                }
                Phase::FinalizeBranch {
                    branch,
                    temp_ref,
                    mapped_commit,
                    mapped_tip,
                    branch_tip,
                } => {
                    self.finalize_branch(
                        branch.clone(),
                        temp_ref.clone(),
                        mapped_commit.clone(),
                        mapped_tip.clone(),
                        branch_tip.clone(),
                    )?;
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
                Phase::Conflict { replay, message } => {
                    let commit = self.current_replay_commit(replay)?;
                    return Ok(ReplayOutcome::Conflict {
                        branch: replay.branch.clone(),
                        commit,
                        worktree: self.state.worktree.path().to_owned(),
                        message: message.clone(),
                    });
                }
                Phase::ContinueAfterConflict { replay } => {
                    self.resolve_conflict(replay.clone())?;
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
            Phase::Conflict { replay, .. } => {
                self.state.phase = Phase::ContinueAfterConflict {
                    replay: replay.clone(),
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

    fn prepare_next_branch_replay(&mut self) -> Result<()> {
        let Some(branch) = self.state.pending_branches.first().cloned() else {
            self.state.phase = Phase::FinalUpdate;
            self.write_state()?;
            return Ok(());
        };

        let node = self.node(branch.as_str())?.clone();
        let branch_index = self.branch_index();
        let replay = self.start_branch_replay(&node, branch_index)?;
        self.state.phase = Phase::ContinueReplay { replay };
        self.write_state()
    }

    fn continue_branch_replay(&mut self, mut replay: BranchReplayState) -> Result<()> {
        let node = self.node(replay.branch.as_str())?.clone();
        let branch_index = self.branch_index();
        let commits = replay_commits_from_extra(&node, &self.state.extra_commits);

        self.backend.start_replay(
            branch_index,
            self.total_branches(),
            &node,
            commits.len(),
            replay.commit_index,
            replay.last_replayed_commit.is_some(),
        )?;
        for (commit_index, commit) in commits.iter().enumerate().skip(replay.commit_index) {
            match self.replay_commit(
                &node,
                commit,
                commit_index,
                commits.len(),
                &replay.last_rewritten,
                branch_index,
            )? {
                CommitReplay::Continued(last_rewritten) => replay.last_rewritten = last_rewritten,
                CommitReplay::Stopped => return Ok(()),
            }
        }

        self.finish_branch(&node, &commits, branch_index)
    }

    fn replay_after_commit(
        &self,
        node: &Node,
        commit_index: usize,
        last_replayed_commit: CommitId,
        last_rewritten: CommitId,
    ) -> BranchReplayState {
        BranchReplayState {
            branch: node.branch.clone(),
            commit_index: commit_index + 1,
            last_replayed_commit: Some(last_replayed_commit),
            last_rewritten,
        }
    }

    fn start_branch_replay(
        &mut self,
        node: &Node,
        branch_index: usize,
    ) -> Result<BranchReplayState> {
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

        Ok(BranchReplayState {
            branch: node.branch.clone(),
            commit_index: 0,
            last_replayed_commit: None,
            last_rewritten: base,
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
    ) -> Result<CommitReplay> {
        let Some(rewritten_commit) =
            self.rewrite_commit(node, commit, commit_index, total_commits, last_rewritten)?
        else {
            return Ok(CommitReplay::Stopped);
        };
        self.state
            .mappings
            .insert(commit.oid.clone(), rewritten_commit.clone());
        if let Some(pause_reasons) = self.state.pause_plan.commit_pause_reasons(&commit.oid) {
            let pause_reasons = pause_reasons.clone();
            self.pause_at_commit(
                node,
                branch_index,
                &rewritten_commit,
                pause_reasons,
                self.replay_after_commit(
                    node,
                    commit_index,
                    commit.oid.clone(),
                    rewritten_commit.clone(),
                ),
            )?;
            return Ok(CommitReplay::Stopped);
        }
        Ok(CommitReplay::Continued(rewritten_commit))
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
                self.state.phase = Phase::Conflict {
                    replay: self.replay_after_commit(
                        node,
                        commit_index,
                        commit.oid.clone(),
                        last_rewritten.clone(),
                    ),
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
        rewritten_tip: &CommitId,
        reasons: BTreeSet<PauseReason>,
        replay: BranchReplayState,
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
            kind: PausedKind::MidBranch { replay },
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
    ) -> Result<()> {
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
            let mapped_commit = commits
                .last()
                .map(|commit| commit.oid.clone())
                .unwrap_or_else(|| node.base.clone());
            self.complete_branch(node, mapped_commit, branch_index, &rewritten_tip)
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
        mapped_commit: CommitId,
        branch_index: usize,
        rewritten_tip: &CommitId,
    ) -> Result<()> {
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
        self.state.phase = Phase::FinalizeBranch {
            branch: node.branch.clone(),
            temp_ref,
            mapped_commit,
            mapped_tip: rewritten_tip.clone(),
            branch_tip,
        };
        self.write_state()?;
        Ok(())
    }

    fn pause_branch_end(
        &mut self,
        node: &Node,
        mapped_commit: CommitId,
        branch_index: usize,
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
        Ok(())
    }

    fn resolve_conflict(&mut self, mut replay: BranchReplayState) -> Result<()> {
        let commit = self.current_replay_commit(&replay)?;
        let rewritten_commit = self.backend.continue_cherry_pick(
            &self.state,
            self.state.worktree.path(),
            &replay.branch,
            &commit,
        )?;
        self.state.mappings.insert(commit, rewritten_commit.clone());
        replay.last_rewritten = rewritten_commit;
        self.state.phase = Phase::ContinueReplay { replay };
        self.write_state()?;
        Ok(())
    }

    fn resume_paused_branch(&mut self, paused: PausedState) -> Result<()> {
        let required_ancestors = self.resume_requirements(&paused)?;
        let rewritten_tip =
            self.backend
                .resume_paused_branch(&self.state, &paused, &required_ancestors)?;
        match paused.kind {
            PausedKind::BranchEnd {
                temp_ref,
                mapped_commit,
            } => {
                self.state.phase = Phase::FinalizeBranch {
                    branch: paused.branch,
                    temp_ref,
                    mapped_commit,
                    mapped_tip: rewritten_tip.clone(),
                    branch_tip: rewritten_tip,
                };
            }
            PausedKind::MidBranch { mut replay } => {
                let commit = self.current_replay_commit(&replay)?;
                self.state
                    .mappings
                    .insert(commit.clone(), rewritten_tip.clone());
                replay.last_rewritten = rewritten_tip;
                self.state.phase = Phase::ContinueReplay { replay };
            }
        }
        self.write_state()?;
        Ok(())
    }

    fn finalize_branch(
        &mut self,
        branch: BranchName,
        temp_ref: GitRef,
        mapped_commit: CommitId,
        mapped_tip: CommitId,
        branch_tip: CommitId,
    ) -> Result<()> {
        self.record_temp_ref(&branch, temp_ref, branch_tip);
        self.state.mappings.insert(mapped_commit, mapped_tip);
        self.remove_pending_branch(&branch)?;
        self.state.phase = Phase::Replay;
        self.write_state()
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

    fn current_replay_commit(&self, replay: &BranchReplayState) -> Result<CommitId> {
        replay.last_replayed_commit.clone().ok_or_else(|| {
            Error::InvalidPlan(format!(
                "replay state for branch `{}` has no current commit",
                replay.branch
            ))
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
