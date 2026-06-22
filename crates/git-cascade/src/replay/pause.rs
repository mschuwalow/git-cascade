use super::backend::ReplayBackend;
use super::state::ReplayPauseMode;
use super::state::{PausedState, Phase, ReplayState};
use super::state_writer::StateWriter;
use super::strategy::ReplayBranchStrategy;
use crate::model::{BranchName, CommitId, GitRef};
use crate::plan::{Node, Plan, PlanCommit};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(super) struct ReplayPauseStrategy {
    mode: ReplayPauseMode,
}

pub(super) enum BranchEnd {
    Complete { ref_update: BranchRefUpdate },
    Pause { prepare_worktree: bool },
}

pub(super) enum BranchRefUpdate {
    Skip,
    Write,
}

impl ReplayPauseStrategy {
    pub(super) fn new(mode: ReplayPauseMode) -> Self {
        Self { mode }
    }

    pub(super) fn pause_commits(
        &self,
        branch_strategy: &ReplayBranchStrategy,
        plan: &Plan,
        node: &Node,
        commits: &[PlanCommit],
    ) -> BTreeSet<CommitId> {
        match self.mode {
            ReplayPauseMode::Never => BTreeSet::new(),
            ReplayPauseMode::EveryCommit => commits
                .iter()
                .take(branch_strategy.every_commit_pause_count(commits))
                .map(|commit| commit.oid.clone())
                .collect(),
            ReplayPauseMode::Checkpoints => {
                branch_strategy.checkpoint_pause_commits(plan, node, commits)
            }
        }
    }

    pub(super) fn branch_end(
        &self,
        branch_strategy: &ReplayBranchStrategy,
        commits: &[PlanCommit],
        unchanged_tip: bool,
    ) -> BranchEnd {
        match self.mode {
            ReplayPauseMode::Never => complete_branch_end(unchanged_tip),
            ReplayPauseMode::EveryCommit => {
                branch_strategy.every_commit_branch_end(commits, unchanged_tip)
            }
            ReplayPauseMode::Checkpoints => BranchEnd::Pause {
                prepare_worktree: unchanged_tip,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_branch<B, W>(
        &self,
        branch_strategy: &ReplayBranchStrategy,
        backend: &mut B,
        state_writer: &mut W,
        state: &mut ReplayState,
        plan: &Plan,
        mappings: &BTreeMap<CommitId, CommitId>,
        temp_tips: &mut HashMap<BranchName, CommitId>,
        branch: &BranchName,
        node: &Node,
        commits: &[PlanCommit],
        branch_index: usize,
        total_branches: usize,
        rewritten_tip: &CommitId,
    ) -> Result<bool>
    where
        B: ReplayBackend,
        W: StateWriter,
    {
        let branch_end = self.branch_end(branch_strategy, commits, rewritten_tip == &node.tip);
        branch_end.apply(
            backend,
            state_writer,
            state,
            plan,
            mappings,
            temp_tips,
            branch,
            node,
            commits,
            branch_index,
            total_branches,
            rewritten_tip,
        )
    }
}

pub(super) fn complete_branch_end(unchanged_tip: bool) -> BranchEnd {
    BranchEnd::Complete {
        ref_update: if unchanged_tip {
            BranchRefUpdate::Skip
        } else {
            BranchRefUpdate::Write
        },
    }
}

impl BranchEnd {
    #[allow(clippy::too_many_arguments)]
    fn apply<B, W>(
        self,
        backend: &mut B,
        state_writer: &mut W,
        state: &mut ReplayState,
        plan: &Plan,
        mappings: &BTreeMap<CommitId, CommitId>,
        temp_tips: &mut HashMap<BranchName, CommitId>,
        branch: &BranchName,
        node: &Node,
        commits: &[PlanCommit],
        branch_index: usize,
        total_branches: usize,
        rewritten_tip: &CommitId,
    ) -> Result<bool>
    where
        B: ReplayBackend,
        W: StateWriter,
    {
        match self {
            Self::Complete { ref_update } => {
                let (temp_ref, branch_tip) = ref_update.write(
                    backend,
                    plan,
                    node,
                    branch_index,
                    total_branches,
                    rewritten_tip,
                )?;
                record_temp_ref(state, temp_tips, &node.branch, temp_ref, branch_tip);
                remove_pending_branch(state, branch)?;
                state.phase = Phase::Replay { current: None };
                write_state(state_writer, state, mappings)?;
                Ok(false)
            }
            Self::Pause { prepare_worktree } => {
                if prepare_worktree {
                    backend.prepare_branch(
                        state,
                        branch_index,
                        total_branches,
                        node,
                        rewritten_tip,
                    )?;
                }
                let (temp_ref, branch_tip) = backend.write_temp_ref(
                    plan,
                    node,
                    branch_index,
                    total_branches,
                    rewritten_tip,
                )?;
                record_temp_ref(
                    state,
                    temp_tips,
                    &node.branch,
                    temp_ref.clone(),
                    branch_tip.clone(),
                );
                remove_pending_branch(state, branch)?;
                state.phase = Phase::Paused {
                    paused: PausedState::BranchEnd {
                        branch: node.branch.clone(),
                        rewritten_tip: branch_tip,
                        temp_ref,
                        mapped_commit: commits
                            .last()
                            .map(|commit| commit.oid.clone())
                            .unwrap_or_else(|| node.base.clone()),
                        worktree: state.worktree.path().to_owned(),
                    },
                };
                write_state(state_writer, state, mappings)?;
                Ok(true)
            }
        }
    }
}

impl BranchRefUpdate {
    fn write<B>(
        self,
        backend: &mut B,
        plan: &Plan,
        node: &Node,
        branch_index: usize,
        total_branches: usize,
        rewritten_tip: &CommitId,
    ) -> Result<(GitRef, CommitId)>
    where
        B: ReplayBackend,
    {
        match self {
            Self::Skip => {
                backend.skip_replay(plan, node, branch_index, total_branches, rewritten_tip)
            }
            Self::Write => {
                backend.write_temp_ref(plan, node, branch_index, total_branches, rewritten_tip)
            }
        }
    }
}

fn record_temp_ref(
    state: &mut ReplayState,
    temp_tips: &mut HashMap<BranchName, CommitId>,
    branch: &BranchName,
    temp_ref: GitRef,
    branch_tip: CommitId,
) {
    temp_tips.insert(branch.clone(), branch_tip);
    if !state.completed_temp_refs.contains(&temp_ref) {
        state.completed_temp_refs.push(temp_ref);
    }
}

fn remove_pending_branch(state: &mut ReplayState, branch: &BranchName) -> Result<()> {
    if state.pending_branches.first() != Some(branch) {
        return Err(Error::InvalidPlan(format!(
            "completed branch `{branch}` is not first in pending state"
        )));
    }
    state.pending_branches.remove(0);
    Ok(())
}

fn write_state<W>(
    state_writer: &mut W,
    state: &mut ReplayState,
    mappings: &BTreeMap<CommitId, CommitId>,
) -> Result<()>
where
    W: StateWriter,
{
    state.mappings = mappings.clone();
    state_writer.write_state(state)
}
