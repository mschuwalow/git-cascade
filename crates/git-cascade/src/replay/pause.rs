use super::replay_commits_from_extra;
use super::state::{PauseReason, ReplayPauseLocation};
use super::strategy as branch_strategy;
use crate::model::Strategy;
use crate::model::{BranchName, CommitId};
use crate::plan::{Plan, PlanCommit};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct PausePlan {
    commit_pauses: BTreeMap<CommitId, BTreeSet<PauseReason>>,
    branch_end_pauses: BTreeMap<BranchName, BTreeSet<PauseReason>>,
}

impl PausePlan {
    pub(super) fn for_plan(
        pause_at: &BTreeSet<ReplayPauseLocation>,
        strategy: Strategy,
        plan: &Plan,
        extra_commits: &BTreeMap<BranchName, Vec<PlanCommit>>,
    ) -> Self {
        let mut pause_plan = Self {
            commit_pauses: BTreeMap::new(),
            branch_end_pauses: BTreeMap::new(),
        };

        if pause_at.is_empty() {
            return pause_plan;
        }

        for node in &plan.nodes {
            let commits = replay_commits_from_extra(node, extra_commits);
            if pause_at.contains(&ReplayPauseLocation::Commits) {
                for commit in &commits {
                    pause_plan.add_commit_reason(commit.oid.clone(), PauseReason::Commit);
                }
            }
            if pause_at.contains(&ReplayPauseLocation::ChildBases) {
                for commit in branch_strategy::checkpoint_commits(strategy, plan, node, &commits) {
                    pause_plan.add_commit_reason(commit, PauseReason::ChildBase);
                }
            }
            if pause_at.contains(&ReplayPauseLocation::BranchEnds) {
                pause_plan.add_branch_end_reason(node.branch.clone());
            }
        }

        pause_plan
    }

    pub(super) fn commit_pause_reasons(&self, commit: &CommitId) -> Option<&BTreeSet<PauseReason>> {
        self.commit_pauses.get(commit)
    }

    pub(super) fn branch_end_pause_reasons(
        &self,
        branch: &BranchName,
    ) -> Option<&BTreeSet<PauseReason>> {
        self.branch_end_pauses.get(branch)
    }

    fn add_commit_reason(&mut self, commit: CommitId, reason: PauseReason) {
        self.commit_pauses.entry(commit).or_default().insert(reason);
    }

    fn add_branch_end_reason(&mut self, branch: BranchName) {
        self.branch_end_pauses
            .entry(branch)
            .or_default()
            .insert(PauseReason::BranchEnd);
    }
}
