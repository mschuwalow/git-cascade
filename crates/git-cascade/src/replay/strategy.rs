use crate::model::Strategy;
use crate::model::{BranchName, CommitId};
use crate::plan::{Node, Plan, PlanCommit};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub(super) fn checkpoint_commits(
    strategy: Strategy,
    plan: &Plan,
    node: &Node,
    commits: &[PlanCommit],
) -> BTreeSet<CommitId> {
    match strategy {
        Strategy::PreserveForkPoints => preserve_fork_point_checkpoints(plan, node, commits),
        Strategy::MoveToPlannedTips => planned_tip_checkpoint(node, commits),
        Strategy::MoveToCurrentTips => BTreeSet::new(),
    }
}

pub(super) fn actual_child_base(
    strategy: Strategy,
    parent: &Node,
    child: &Node,
    mappings: &BTreeMap<CommitId, CommitId>,
    temp_tips: &HashMap<BranchName, CommitId>,
) -> Result<CommitId> {
    match strategy {
        Strategy::PreserveForkPoints => preserve_fork_point_child_base(parent, child, mappings),
        Strategy::MoveToPlannedTips => planned_parent_tip(parent, mappings),
        Strategy::MoveToCurrentTips => current_parent_tip(parent, temp_tips),
    }
}

pub(super) fn required_child_replay_base(
    strategy: Strategy,
    parent: &Node,
    child: &Node,
    mappings: &BTreeMap<CommitId, CommitId>,
) -> Result<Option<(CommitId, String)>> {
    match strategy {
        Strategy::PreserveForkPoints => preserve_fork_point_child_base(parent, child, mappings)
            .map(|base| Some(child_replay_base_requirement(child, base))),
        Strategy::MoveToPlannedTips => planned_parent_tip(parent, mappings)
            .map(|base| Some(child_replay_base_requirement(child, base))),
        Strategy::MoveToCurrentTips => Ok(None),
    }
}

fn preserve_fork_point_checkpoints(
    plan: &Plan,
    node: &Node,
    commits: &[PlanCommit],
) -> BTreeSet<CommitId> {
    let Some(last_commit) = commits.last() else {
        return BTreeSet::new();
    };

    child_replay_bases(plan, node, commits)
        .filter(|base| *base != node.base())
        .filter(|base| *base != last_commit.oid.as_str())
        .map(CommitId::new)
        .collect()
}

fn planned_tip_checkpoint(node: &Node, commits: &[PlanCommit]) -> BTreeSet<CommitId> {
    let Some(last_commit) = commits.last() else {
        return BTreeSet::new();
    };
    let commit_oids = commits
        .iter()
        .map(|commit| commit.oid.as_str())
        .collect::<BTreeSet<_>>();
    if node.tip != last_commit.oid && commit_oids.contains(node.tip.as_str()) {
        BTreeSet::from([node.tip.clone()])
    } else {
        BTreeSet::new()
    }
}

fn child_replay_bases<'plan>(
    plan: &'plan Plan,
    node: &Node,
    commits: &[PlanCommit],
) -> impl Iterator<Item = &'plan str> {
    let Some(last_commit) = commits.last() else {
        return Vec::new().into_iter();
    };
    let has_child = plan
        .nodes
        .iter()
        .any(|child| child.parent() == Some(node.branch.as_str()));
    if !has_child {
        return Vec::new().into_iter();
    }

    let commit_oids = commits
        .iter()
        .map(|commit| commit.oid.as_str())
        .collect::<BTreeSet<_>>();
    let bases = plan
        .nodes
        .iter()
        .filter(|child| child.parent() == Some(node.branch.as_str()))
        .map(Node::base)
        .filter(move |base| *base != last_commit.oid.as_str())
        .filter(move |base| commit_oids.contains(*base))
        .collect::<Vec<_>>();
    bases.into_iter()
}

fn preserve_fork_point_child_base(
    parent: &Node,
    child: &Node,
    mappings: &BTreeMap<CommitId, CommitId>,
) -> Result<CommitId> {
    if child.base() == parent.base() {
        return mappings.get(&parent.base).cloned().ok_or_else(|| {
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
