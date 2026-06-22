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
        let pause_at_branch_end = match self.mode {
            ReplayPauseMode::Never => false,
            ReplayPauseMode::EveryCommit => {
                branch_strategy.pauses_at_branch_end_after_every_commit(commits)
            }
            ReplayPauseMode::Checkpoints => true,
        };

        if pause_at_branch_end {
            pause_branch_end(
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
        } else {
            complete_branch(
                backend,
                state_writer,
                state,
                plan,
                mappings,
                temp_tips,
                branch,
                node,
                branch_index,
                total_branches,
                rewritten_tip,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_branch<B, W>(
    backend: &mut B,
    state_writer: &mut W,
    state: &mut ReplayState,
    plan: &Plan,
    mappings: &BTreeMap<CommitId, CommitId>,
    temp_tips: &mut HashMap<BranchName, CommitId>,
    branch: &BranchName,
    node: &Node,
    branch_index: usize,
    total_branches: usize,
    rewritten_tip: &CommitId,
) -> Result<bool>
where
    B: ReplayBackend,
    W: StateWriter,
{
    let (temp_ref, branch_tip) = if rewritten_tip == &node.tip {
        backend.skip_replay(plan, node, branch_index, total_branches, rewritten_tip)?
    } else {
        backend.write_temp_ref(plan, node, branch_index, total_branches, rewritten_tip)?
    };
    record_temp_ref(state, temp_tips, &node.branch, temp_ref, branch_tip);
    remove_pending_branch(state, branch)?;
    state.phase = Phase::Replay { current: None };
    write_state(state_writer, state, mappings)?;
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn pause_branch_end<B, W>(
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
    if rewritten_tip == &node.tip {
        backend.prepare_branch(state, branch_index, total_branches, node, rewritten_tip)?;
    }
    let (temp_ref, branch_tip) =
        backend.write_temp_ref(plan, node, branch_index, total_branches, rewritten_tip)?;
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
