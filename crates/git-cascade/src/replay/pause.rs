use super::replay_commits_from_extra;
use super::state::{PauseReason, ReplayPauseMode};
use super::strategy as branch_strategy;
use crate::model::Strategy;
use crate::model::{BranchName, CommitId};
use crate::plan::{Plan, PlanCommit};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct PausePlan {
    commit_pauses: BTreeMap<CommitId, BTreeSet<PauseReason>>,
    branch_end_commit_pauses: BTreeSet<CommitId>,
    branch_end_pauses: BTreeMap<BranchName, BTreeSet<PauseReason>>,
}

impl PausePlan {
    pub(super) fn for_plan(
        mode: ReplayPauseMode,
        strategy: Strategy,
        plan: &Plan,
        extra_commits: &BTreeMap<BranchName, Vec<PlanCommit>>,
    ) -> Self {
        let mut pause_plan = Self {
            commit_pauses: BTreeMap::new(),
            branch_end_commit_pauses: BTreeSet::new(),
            branch_end_pauses: BTreeMap::new(),
        };

        if mode == ReplayPauseMode::Never {
            return pause_plan;
        }

        for node in &plan.nodes {
            let commits = replay_commits_from_extra(node, extra_commits);
            match mode {
                ReplayPauseMode::Never => unreachable!("handled before collecting pause points"),
                ReplayPauseMode::EveryCommit => {
                    for commit in &commits {
                        pause_plan.add_commit_reason(commit.oid.clone(), PauseReason::Commit);
                    }
                    if matches!(strategy, Strategy::Squash) && commits.len() > 1 {
                        pause_plan
                            .add_branch_end_reason(node.branch.clone(), PauseReason::BranchEnd);
                        pause_plan.add_branch_end_reason(node.branch.clone(), PauseReason::Commit);
                    } else if let Some(last_commit) = commits.last() {
                        pause_plan
                            .add_commit_reason(last_commit.oid.clone(), PauseReason::BranchEnd);
                        pause_plan.add_branch_end_commit_pause(last_commit.oid.clone());
                    }
                }
                ReplayPauseMode::Checkpoints => {
                    for commit in
                        branch_strategy::checkpoint_commits(strategy, plan, node, &commits)
                    {
                        pause_plan.add_commit_reason(commit, PauseReason::ChildBase);
                    }
                    if matches!(strategy, Strategy::Squash) && commits.len() > 1 {
                        pause_plan
                            .add_branch_end_reason(node.branch.clone(), PauseReason::BranchEnd);
                    } else if let Some(last_commit) = commits.last() {
                        pause_plan
                            .add_commit_reason(last_commit.oid.clone(), PauseReason::BranchEnd);
                        pause_plan.add_branch_end_commit_pause(last_commit.oid.clone());
                    }
                }
            }
        }

        pause_plan
    }

    pub(super) fn commit_pause_reasons(&self, commit: &CommitId) -> Option<&BTreeSet<PauseReason>> {
        self.commit_pauses.get(commit)
    }

    pub(super) fn is_branch_end_commit_pause(&self, commit: &CommitId) -> bool {
        self.branch_end_commit_pauses.contains(commit)
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

    fn add_branch_end_commit_pause(&mut self, commit: CommitId) {
        self.branch_end_commit_pauses.insert(commit);
    }

    fn add_branch_end_reason(&mut self, branch: BranchName, reason: PauseReason) {
        self.branch_end_pauses
            .entry(branch)
            .or_default()
            .insert(reason);
    }
}
