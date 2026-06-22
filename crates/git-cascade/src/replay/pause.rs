use super::replay_commits_from_extra;
use super::state::ReplayPauseMode;
use super::strategy as branch_strategy;
use crate::model::Strategy;
use crate::model::{BranchName, CommitId};
use crate::plan::{Plan, PlanCommit};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct PausePlan {
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
        let mut pause_plan = Self {
            pauses: BTreeSet::new(),
            branch_end_pauses: BTreeSet::new(),
        };

        if mode == ReplayPauseMode::Never {
            return pause_plan;
        }

        for node in &plan.nodes {
            let commits = replay_commits_from_extra(node, extra_commits);
            match mode {
                ReplayPauseMode::Never => unreachable!("handled before collecting pause points"),
                ReplayPauseMode::EveryCommit => {
                    pause_plan
                        .pauses
                        .extend(commits.iter().map(|commit| commit.oid.clone()));
                    if matches!(strategy, Strategy::Squash) && !commits.is_empty() {
                        pause_plan.branch_end_pauses.insert(node.branch.clone());
                    }
                }
                ReplayPauseMode::Checkpoints => {
                    pause_plan
                        .pauses
                        .extend(branch_strategy::checkpoint_commits(
                            strategy, plan, node, &commits,
                        ));
                    pause_plan.branch_end_pauses.insert(node.branch.clone());
                }
            }
        }

        pause_plan
    }

    pub(super) fn pauses_at_commit(&self, commit: &CommitId) -> bool {
        self.pauses.contains(commit)
    }

    pub(super) fn pauses_at_branch_end(&self, branch: &BranchName) -> bool {
        self.branch_end_pauses.contains(branch)
    }
}
