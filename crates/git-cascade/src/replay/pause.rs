use super::replay_commits_from_extra;
use super::state::ReplayPauseMode;
use super::strategy as branch_strategy;
use crate::model::Strategy;
use crate::model::{BranchName, CommitId};
use crate::plan::{Plan, PlanCommit};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PausePlan {
    pauses: BTreeSet<CommitId>,
    branch_end_pauses: BTreeSet<BranchName>,
}

impl PausePlan {
    pub(super) fn for_plan(
        mode: ReplayPauseMode,
        strategy: Strategy,
        plan: &Plan,
        extra_commits: &BTreeMap<BranchName, Vec<PlanCommit>>,
    ) -> Self {
        let mut replay_commits = BTreeSet::new();
        let mut checkpoint_commits = BTreeSet::new();
        let mut branch_end_branches = BTreeSet::new();
        let mut branch_end_finalization_branches = BTreeSet::new();

        for node in &plan.nodes {
            let commits = replay_commits_from_extra(node, extra_commits);
            replay_commits.extend(commits.iter().map(|commit| commit.oid.clone()));
            checkpoint_commits.extend(branch_strategy::checkpoint_commits(
                strategy, plan, node, &commits,
            ));

            branch_end_branches.insert(node.branch.clone());
            if branch_strategy::finalizes_branch_at_end(strategy, &commits) {
                branch_end_finalization_branches.insert(node.branch.clone());
            }
        }

        Self::new(
            mode,
            replay_commits,
            checkpoint_commits,
            branch_end_branches,
            branch_end_finalization_branches,
        )
    }

    fn new(
        mode: ReplayPauseMode,
        replay_commits: BTreeSet<CommitId>,
        checkpoint_commits: BTreeSet<CommitId>,
        branch_end_branches: BTreeSet<BranchName>,
        branch_end_finalization_branches: BTreeSet<BranchName>,
    ) -> Self {
        match mode {
            ReplayPauseMode::Never => Self {
                pauses: BTreeSet::new(),
                branch_end_pauses: BTreeSet::new(),
            },
            ReplayPauseMode::EveryCommit => Self {
                pauses: replay_commits,
                branch_end_pauses: branch_end_finalization_branches,
            },
            ReplayPauseMode::Checkpoints => Self {
                pauses: checkpoint_commits,
                branch_end_pauses: branch_end_branches,
            },
        }
    }

    pub(super) fn pauses_at_commit(&self, commit: &CommitId) -> bool {
        self.pauses.contains(commit)
    }

    pub(super) fn pauses_at_branch_end(&self, branch: &BranchName) -> bool {
        self.branch_end_pauses.contains(branch)
    }
}
