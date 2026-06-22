use super::backend::ReplayBackend;
use super::state::ReplayState;
use crate::model::Strategy;
use crate::model::{BranchName, CommitId};
use crate::plan::{Node, PlanCommit};
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap};

pub(super) struct ReplayBranchStrategy {
    strategy: Strategy,
}

pub(super) fn dry_run_temp_ref_tracks_rewritten_tip(strategy: Strategy) -> bool {
    matches!(strategy, Strategy::Squash)
}

impl ReplayBranchStrategy {
    pub(super) fn new(strategy: Strategy) -> Self {
        Self { strategy }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn rewrite_branch_tip<B>(
        &self,
        backend: &mut B,
        state: &ReplayState,
        branch_replay_base: &CommitId,
        total_branches: usize,
        mappings: &mut BTreeMap<CommitId, CommitId>,
        node: &Node,
        commits: &[PlanCommit],
        branch_index: usize,
        rewritten_tip: CommitId,
    ) -> Result<CommitId>
    where
        B: ReplayBackend,
    {
        match self.strategy {
            Strategy::Squash if commits.len() > 1 => {
                let first_commit = commits
                    .first()
                    .expect("non-empty commits has a first commit")
                    .oid
                    .clone();
                let rewritten_tip = backend.squash_branch(
                    state,
                    node,
                    branch_index,
                    total_branches,
                    branch_replay_base,
                    &first_commit,
                    &rewritten_tip,
                )?;
                if let Some(last_commit) = commits.last() {
                    mappings.insert(last_commit.oid.clone(), rewritten_tip.clone());
                }
                Ok(rewritten_tip)
            }
            Strategy::PreserveForkPoints
            | Strategy::MoveToPlannedTips
            | Strategy::MoveToCurrentTips
            | Strategy::Squash => Ok(rewritten_tip),
        }
    }

    pub(super) fn actual_child_base(
        &self,
        parent: &Node,
        child: &Node,
        selected_bases: &HashMap<BranchName, CommitId>,
        mappings: &BTreeMap<CommitId, CommitId>,
        temp_tips: &HashMap<BranchName, CommitId>,
    ) -> Result<CommitId> {
        match self.strategy {
            Strategy::PreserveForkPoints => {
                preserve_fork_point_child_base(parent, child, selected_bases, mappings)
            }
            Strategy::MoveToPlannedTips => planned_parent_tip(parent, mappings),
            Strategy::MoveToCurrentTips | Strategy::Squash => current_parent_tip(parent, temp_tips),
        }
    }

    pub(super) fn required_child_replay_base(
        &self,
        parent: &Node,
        child: &Node,
        selected_bases: &HashMap<BranchName, CommitId>,
        mappings: &BTreeMap<CommitId, CommitId>,
    ) -> Result<Option<(CommitId, String)>> {
        match self.strategy {
            Strategy::PreserveForkPoints => {
                preserve_fork_point_child_base(parent, child, selected_bases, mappings)
                    .map(|base| Some(child_replay_base_requirement(child, base)))
            }
            Strategy::MoveToPlannedTips => planned_parent_tip(parent, mappings)
                .map(|base| Some(child_replay_base_requirement(child, base))),
            Strategy::MoveToCurrentTips | Strategy::Squash => Ok(None),
        }
    }
}

fn preserve_fork_point_child_base(
    parent: &Node,
    child: &Node,
    selected_bases: &HashMap<BranchName, CommitId>,
    mappings: &BTreeMap<CommitId, CommitId>,
) -> Result<CommitId> {
    if child.base() == parent.base() {
        return selected_bases.get(&parent.branch).cloned().ok_or_else(|| {
            Error::InvalidPlan(format!("parent `{}` has no selected base", parent.branch))
        });
    }

    mappings.get(&child.base).cloned().ok_or_else(|| {
        Error::InvalidPlan(format!(
            "base `{}` for branch `{}` was not mapped",
            child.base(),
            child.branch
        ))
    })
}

fn planned_parent_tip(parent: &Node, mappings: &BTreeMap<CommitId, CommitId>) -> Result<CommitId> {
    mappings.get(&parent.tip).cloned().ok_or_else(|| {
        Error::InvalidPlan(format!(
            "parent `{}` has no rewritten planned tip",
            parent.branch
        ))
    })
}

fn current_parent_tip(
    parent: &Node,
    temp_tips: &HashMap<BranchName, CommitId>,
) -> Result<CommitId> {
    temp_tips.get(&parent.branch).cloned().ok_or_else(|| {
        Error::InvalidPlan(format!("parent `{}` has no rewritten tip", parent.branch))
    })
}

fn child_replay_base_requirement(child: &Node, base: CommitId) -> (CommitId, String) {
    (
        base,
        format!("replay base for child branch `{}`", child.branch),
    )
}
