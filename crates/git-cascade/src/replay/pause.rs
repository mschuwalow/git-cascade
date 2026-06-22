use super::state::ReplayPauseMode;
use crate::model::{BranchName, CommitId, Strategy};
use crate::plan::{Node, Plan, PlanCommit};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct ReplayPauseStrategy {
    commit_pauses: BTreeSet<CommitId>,
    branch_end_pauses: BTreeSet<BranchName>,
}

impl ReplayPauseStrategy {
    pub(super) fn new(
        mode: ReplayPauseMode,
        replay_strategy: Strategy,
        plan: &Plan,
        extra_commits: &BTreeMap<BranchName, Vec<PlanCommit>>,
    ) -> Self {
        let mut commit_pauses = BTreeSet::new();
        let mut branch_end_pauses = BTreeSet::new();

        for node in &plan.nodes {
            let commits = replay_commits_from_extra(node, extra_commits);
            let pauses = match mode {
                ReplayPauseMode::Never => BTreeSet::new(),
                ReplayPauseMode::EveryCommit => every_commit_pauses(replay_strategy, &commits),
                ReplayPauseMode::Checkpoints => {
                    checkpoint_pause_commits(replay_strategy, plan, node, &commits)
                }
            };
            commit_pauses.extend(pauses);

            if pauses_at_branch_end(mode, replay_strategy, &commits) {
                branch_end_pauses.insert(node.branch.clone());
            }
        }

        Self {
            commit_pauses,
            branch_end_pauses,
        }
    }

    pub(super) fn pauses_at_commit(&self, commit: &CommitId) -> bool {
        self.commit_pauses.contains(commit)
    }

    pub(super) fn pauses_at_branch_end(&self, branch: &BranchName) -> bool {
        self.branch_end_pauses.contains(branch)
    }
}

fn replay_commits_from_extra(
    node: &Node,
    extra_commits: &BTreeMap<BranchName, Vec<PlanCommit>>,
) -> Vec<PlanCommit> {
    let mut commits = node.commits().to_vec();
    if let Some(extra) = extra_commits.get(&node.branch) {
        commits.extend(extra.iter().cloned());
    }
    commits
}

fn every_commit_pauses(strategy: Strategy, commits: &[PlanCommit]) -> BTreeSet<CommitId> {
    let pause_count = match strategy {
        Strategy::Squash => commits.len().saturating_sub(1),
        Strategy::PreserveForkPoints
        | Strategy::MoveToPlannedTips
        | Strategy::MoveToCurrentTips => commits.len(),
    };
    commits
        .iter()
        .take(pause_count)
        .map(|commit| commit.oid.clone())
        .collect()
}

fn pauses_at_branch_end(mode: ReplayPauseMode, strategy: Strategy, commits: &[PlanCommit]) -> bool {
    match mode {
        ReplayPauseMode::Never => false,
        ReplayPauseMode::EveryCommit => match strategy {
            Strategy::Squash => !commits.is_empty(),
            Strategy::PreserveForkPoints
            | Strategy::MoveToPlannedTips
            | Strategy::MoveToCurrentTips => false,
        },
        ReplayPauseMode::Checkpoints => true,
    }
}

fn checkpoint_pause_commits(
    strategy: Strategy,
    plan: &Plan,
    node: &Node,
    commits: &[PlanCommit],
) -> BTreeSet<CommitId> {
    match strategy {
        Strategy::PreserveForkPoints => preserve_fork_point_pause_commits(plan, node, commits),
        Strategy::MoveToPlannedTips => planned_tip_pause_commits(node, commits),
        Strategy::MoveToCurrentTips | Strategy::Squash => BTreeSet::new(),
    }
}

fn preserve_fork_point_pause_commits(
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

fn planned_tip_pause_commits(node: &Node, commits: &[PlanCommit]) -> BTreeSet<CommitId> {
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
