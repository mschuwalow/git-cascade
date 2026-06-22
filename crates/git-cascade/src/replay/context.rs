use super::ReplayOutcome;
use super::backend::{CherryPickOutcome, ReplayBackend, RequiredAncestor};
use super::state::{CurrentState, PausedState, Phase, ReplayPauseMode, ReplayState};
use super::state_writer::StateWriter;
use crate::model::Strategy;
use crate::model::{BranchName, CommitId, GitRef};
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
    nodes: HashMap<BranchName, usize>,
    temp_tips: HashMap<BranchName, CommitId>,
    selected_bases: HashMap<BranchName, CommitId>,
    mappings: BTreeMap<CommitId, CommitId>,
    branch_strategy: Box<dyn ReplayBranchStrategy>,
    pause_strategy: Box<dyn ReplayPauseStrategy>,
}

enum CommitReplay {
    Continue,
    Stop,
}

trait ReplayBranchStrategy {
    fn checkpoint_pause_commits(
        &self,
        plan: &Plan,
        node: &Node,
        commits: &[PlanCommit],
    ) -> BTreeSet<CommitId>;

    fn every_commit_pause_count(&self, commits: &[PlanCommit]) -> usize {
        commits.len()
    }

    fn every_commit_branch_end(&self, _commits: &[PlanCommit], unchanged_tip: bool) -> BranchEnd {
        complete_branch_end(unchanged_tip)
    }

    fn branch_tip_rewrite<'commit>(
        &self,
        _commits: &'commit [PlanCommit],
    ) -> BranchTipRewrite<'commit> {
        BranchTipRewrite::Keep
    }

    fn actual_child_base(
        &self,
        parent: &Node,
        child: &Node,
        selected_bases: &HashMap<BranchName, CommitId>,
        mappings: &BTreeMap<CommitId, CommitId>,
        temp_tips: &HashMap<BranchName, CommitId>,
    ) -> Result<CommitId>;

    fn required_child_replay_base(
        &self,
        parent: &Node,
        child: &Node,
        selected_bases: &HashMap<BranchName, CommitId>,
        mappings: &BTreeMap<CommitId, CommitId>,
    ) -> Result<Option<(CommitId, String)>>;
}

trait ReplayPauseStrategy {
    fn pause_commits(
        &self,
        branch_strategy: &dyn ReplayBranchStrategy,
        plan: &Plan,
        node: &Node,
        commits: &[PlanCommit],
    ) -> BTreeSet<CommitId>;

    fn branch_end(
        &self,
        branch_strategy: &dyn ReplayBranchStrategy,
        commits: &[PlanCommit],
        unchanged_tip: bool,
    ) -> BranchEnd;
}

enum BranchEnd {
    Complete { ref_update: BranchRefUpdate },
    Pause { prepare_worktree: bool },
}

enum BranchRefUpdate {
    Skip,
    Write,
}

enum BranchTipRewrite<'commit> {
    Keep,
    Squash { first_commit: &'commit PlanCommit },
}

impl<'commit> BranchTipRewrite<'commit> {
    fn apply<B, W>(
        self,
        replay: &mut ReplayContext<'_, '_, B, W>,
        node: &Node,
        commits: &[PlanCommit],
        branch_index: usize,
        rewritten_tip: CommitId,
    ) -> Result<CommitId>
    where
        B: ReplayBackend,
        W: StateWriter,
    {
        match self {
            Self::Keep => Ok(rewritten_tip),
            Self::Squash { first_commit } => {
                let base = replay.branch_replay_base(node)?.clone();
                let rewritten_tip = replay.backend.squash_branch(
                    &replay.state,
                    node,
                    branch_index,
                    replay.total_branches(),
                    &base,
                    &first_commit.oid,
                    &rewritten_tip,
                )?;
                if let Some(last_commit) = commits.last() {
                    replay
                        .mappings
                        .insert(last_commit.oid.clone(), rewritten_tip.clone());
                }
                Ok(rewritten_tip)
            }
        }
    }
}

impl BranchEnd {
    fn apply<B, W>(
        self,
        replay: &mut ReplayContext<'_, '_, B, W>,
        node: &Node,
        branch: &BranchName,
        commits: &[PlanCommit],
        branch_index: usize,
        rewritten_tip: &CommitId,
    ) -> Result<bool>
    where
        B: ReplayBackend,
        W: StateWriter,
    {
        match self {
            Self::Complete { ref_update } => {
                let (temp_ref, branch_tip) =
                    ref_update.write(replay, node, branch_index, rewritten_tip)?;
                replay.record_temp_ref(&node.branch, temp_ref, branch_tip);
                replay.remove_pending_branch(branch)?;
                replay.state.phase = Phase::Replay { current: None };
                replay.write_state()?;
                Ok(false)
            }
            Self::Pause { prepare_worktree } => {
                if prepare_worktree {
                    replay.backend.prepare_branch(
                        &replay.state,
                        branch_index,
                        replay.total_branches(),
                        node,
                        rewritten_tip,
                    )?;
                }
                let (temp_ref, branch_tip) = replay.backend.write_temp_ref(
                    replay.plan,
                    node,
                    branch_index,
                    replay.total_branches(),
                    rewritten_tip,
                )?;
                replay.record_temp_ref(&node.branch, temp_ref.clone(), branch_tip.clone());
                replay.remove_pending_branch(branch)?;
                replay.state.phase = Phase::Paused {
                    paused: PausedState::BranchEnd {
                        branch: node.branch.clone(),
                        rewritten_tip: branch_tip,
                        temp_ref,
                        mapped_commit: commits
                            .last()
                            .map(|commit| commit.oid.clone())
                            .unwrap_or_else(|| node.base.clone()),
                        worktree: replay.state.worktree.path().to_owned(),
                    },
                };
                replay.write_state()?;
                Ok(true)
            }
        }
    }
}

impl BranchRefUpdate {
    fn write<B, W>(
        self,
        replay: &mut ReplayContext<'_, '_, B, W>,
        node: &Node,
        branch_index: usize,
        rewritten_tip: &CommitId,
    ) -> Result<(GitRef, CommitId)>
    where
        B: ReplayBackend,
        W: StateWriter,
    {
        match self {
            Self::Skip => replay.backend.skip_replay(
                replay.plan,
                node,
                branch_index,
                replay.total_branches(),
                rewritten_tip,
            ),
            Self::Write => replay.backend.write_temp_ref(
                replay.plan,
                node,
                branch_index,
                replay.total_branches(),
                rewritten_tip,
            ),
        }
    }
}

struct PreserveForkPoints;
struct MoveToPlannedTips;
struct MoveToCurrentTips;
struct SquashBranches;

struct NeverPause;
struct PauseEveryCommit;
struct PauseAtCheckpoints;

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
        let nodes = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.branch.clone(), index))
            .collect::<HashMap<_, _>>();
        let temp_tips = backend.temp_tips(&state.completed_temp_refs)?;
        let selected_bases = selected_bases_from_mappings(plan, &state.mappings);
        let mappings = state.mappings.clone();
        let branch_strategy = branch_strategy(state.strategy);
        let pause_strategy = pause_strategy(state.replay_mode);

        Ok(Self {
            plan,
            state_writer,
            backend,
            state,
            nodes,
            temp_tips,
            selected_bases,
            mappings,
            branch_strategy,
            pause_strategy,
        })
    }

    pub(super) fn run(&mut self) -> Result<ReplayOutcome> {
        self.backend.start(&self.state)?;
        loop {
            match &self.state.phase {
                Phase::Replay { .. } => {
                    self.replay_pending_branches()?;
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
                Phase::Conflict { current, message } => {
                    return Ok(ReplayOutcome::Conflict {
                        current: current.clone(),
                        message: message.clone(),
                    });
                }
                Phase::ContinueAfterConflict { current } => {
                    self.resolve_conflict(current.clone())?;
                    self.write_state()?;
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
            Phase::Conflict { current, .. } => {
                self.state.phase = Phase::ContinueAfterConflict {
                    current: current.clone(),
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

    fn replay_pending_branches(&mut self) -> Result<()> {
        if self.total_branches() == 0 {
            self.backend.no_branches()?;
        }

        while let Some(branch) = self.state.pending_branches.first().cloned() {
            if self.replay_branch(branch)? {
                return Ok(());
            }
        }

        self.state.phase = Phase::FinalUpdate;
        self.write_state()?;
        Ok(())
    }

    fn replay_branch(&mut self, branch: BranchName) -> Result<bool> {
        let node = self.node(branch.as_str())?.clone();
        let branch_index = self.branch_index();
        let commits = replay_commits_from_extra(&node, &self.state.extra_commits);
        let replay_current = self.replay_current();
        let was_resuming = replay_current.is_some();
        let child_base_pause_commits = self.pause_strategy.pause_commits(
            self.branch_strategy.as_ref(),
            self.plan,
            &node,
            &commits,
        );

        let (start_commit_index, mut last_rewritten) = if let Some(current) = replay_current {
            let start = self.resume_start_commit_index(&node, &current, &commits)?;
            self.state.phase = Phase::Replay { current: None };
            let head = commits
                .get(start.wrapping_sub(1))
                .and_then(|commit| self.mappings.get(&commit.oid))
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!(
                        "branch `{}` has no rewritten commit to resume from",
                        node.branch
                    ))
                })?;
            (start, head)
        } else {
            let base = self.actual_replay_base(&node)?;
            self.selected_bases
                .insert(node.branch.clone(), base.clone());
            self.mappings.insert(node.base.clone(), base.clone());

            if base != node.base {
                self.backend.prepare_branch(
                    &self.state,
                    branch_index,
                    self.total_branches(),
                    &node,
                    &base,
                )?;
            }
            (0, base)
        };

        self.backend.start_replay(
            branch_index,
            self.total_branches(),
            &node,
            commits.len(),
            start_commit_index,
            was_resuming,
        )?;
        for (commit_index, commit) in commits.iter().enumerate().skip(start_commit_index) {
            match self.replay_commit(
                &node,
                commit,
                commit_index,
                commits.len(),
                &last_rewritten,
                &child_base_pause_commits,
                branch_index,
            )? {
                CommitReplay::Continue => last_rewritten = self.mapped_commit(&commit.oid)?.clone(),
                CommitReplay::Stop => return Ok(true),
            }
        }

        self.finish_branch(&node, &branch, &commits, branch_index)
    }

    #[allow(clippy::too_many_arguments)]
    fn replay_commit(
        &mut self,
        node: &Node,
        commit: &PlanCommit,
        commit_index: usize,
        total_commits: usize,
        last_rewritten: &CommitId,
        child_base_pause_commits: &BTreeSet<CommitId>,
        branch_index: usize,
    ) -> Result<CommitReplay> {
        let Some(rewritten_commit) =
            self.rewrite_commit(node, commit, commit_index, total_commits, last_rewritten)?
        else {
            return Ok(CommitReplay::Stop);
        };
        self.mappings
            .insert(commit.oid.clone(), rewritten_commit.clone());
        if child_base_pause_commits.contains(&commit.oid) {
            if self.can_keep_existing_commit(commit, last_rewritten) {
                self.backend.prepare_branch(
                    &self.state,
                    branch_index,
                    self.total_branches(),
                    node,
                    &rewritten_commit,
                )?;
            }
            self.pause_at_child_base(node, &commit.oid, &rewritten_commit)?;
            return Ok(CommitReplay::Stop);
        }
        Ok(CommitReplay::Continue)
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
                let current = CurrentState {
                    branch: node.branch.clone(),
                    commit: commit.oid.clone(),
                    worktree: self.state.worktree.path().to_owned(),
                };
                self.state.phase = Phase::Conflict {
                    current: current.clone(),
                    message,
                };
                self.write_state()?;
                Ok(None)
            }
        }
    }

    fn pause_at_child_base(
        &mut self,
        node: &Node,
        commit: &CommitId,
        rewritten_tip: &CommitId,
    ) -> Result<()> {
        let paused = PausedState::ChildBase {
            branch: node.branch.clone(),
            commit: commit.clone(),
            rewritten_tip: rewritten_tip.clone(),
            worktree: self.state.worktree.path().to_owned(),
        };
        self.state.phase = Phase::Paused { paused };
        self.write_state()
    }

    fn finish_branch(
        &mut self,
        node: &Node,
        branch: &BranchName,
        commits: &[PlanCommit],
        branch_index: usize,
    ) -> Result<bool> {
        let rewritten_tip = if let Some(commit) = commits.last() {
            self.mapped_commit(&commit.oid)?.clone()
        } else {
            self.selected_bases
                .get(&node.branch)
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!("branch `{}` has no selected base", node.branch))
                })?
        };

        let rewritten_tip = self.branch_strategy.branch_tip_rewrite(commits).apply(
            self,
            node,
            commits,
            branch_index,
            rewritten_tip,
        )?;

        let branch_end = self.pause_strategy.branch_end(
            self.branch_strategy.as_ref(),
            commits,
            rewritten_tip == node.tip,
        );
        branch_end.apply(self, node, branch, commits, branch_index, &rewritten_tip)
    }

    fn resolve_conflict(&mut self, current: CurrentState) -> Result<()> {
        let rewritten_commit = self.backend.continue_cherry_pick(&self.state, &current)?;
        self.mappings
            .insert(current.commit.clone(), rewritten_commit);
        self.state.phase = Phase::Replay {
            current: Some(current),
        };
        self.write_state()?;
        Ok(())
    }

    fn resume_paused_branch(&mut self, paused: PausedState) -> Result<()> {
        if !self
            .plan
            .nodes
            .iter()
            .any(|node| node.branch.as_str() == paused.branch())
        {
            return Err(Error::InvalidPlan(format!(
                "paused branch `{}` is not in the active plan",
                paused.branch()
            )));
        }

        let required_ancestors = self.resume_requirements(&paused)?;
        let rewritten_tip =
            self.backend
                .resume_paused_branch(&self.state, &paused, &required_ancestors)?;
        match paused {
            PausedState::BranchEnd {
                branch,
                mapped_commit,
                temp_ref,
                ..
            } => {
                self.record_temp_ref(&branch, temp_ref, rewritten_tip.clone());
                self.mappings.insert(mapped_commit, rewritten_tip);
                self.state.phase = Phase::Replay { current: None };
            }
            PausedState::ChildBase {
                branch,
                commit,
                worktree,
                ..
            } => {
                self.mappings.insert(commit.clone(), rewritten_tip);
                self.state.phase = Phase::Replay {
                    current: Some(CurrentState {
                        branch,
                        commit,
                        worktree,
                    }),
                };
            }
        }
        self.write_state()?;
        Ok(())
    }

    fn resume_requirements(&self, paused: &PausedState) -> Result<Vec<RequiredAncestor>> {
        let node = self.node(paused.branch())?;
        match paused {
            PausedState::ChildBase { rewritten_tip, .. } => {
                let mut required = BTreeMap::<CommitId, String>::new();
                required.insert(
                    self.branch_replay_base(node)?.clone(),
                    format!("replay base for branch `{}`", node.branch),
                );
                required.insert(
                    rewritten_tip.clone(),
                    format!("rewritten child-base checkpoint for `{}`", node.branch),
                );
                Ok(required
                    .into_iter()
                    .map(|(commit, reason)| RequiredAncestor { commit, reason })
                    .collect())
            }
            PausedState::BranchEnd { .. } => self.branch_end_resume_requirements(node),
        }
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
        self.selected_bases.get(&node.branch).ok_or_else(|| {
            Error::InvalidPlan(format!("branch `{}` has no selected base", node.branch))
        })
    }

    fn required_child_replay_base(
        &self,
        parent: &Node,
        child: &Node,
    ) -> Result<Option<(CommitId, String)>> {
        self.branch_strategy.required_child_replay_base(
            parent,
            child,
            &self.selected_bases,
            &self.mappings,
        )
    }

    fn resume_start_commit_index(
        &self,
        node: &Node,
        current: &CurrentState,
        commits: &[PlanCommit],
    ) -> Result<usize> {
        if current.branch != node.branch {
            return Err(Error::InvalidPlan(format!(
                "current branch `{}` is not the next pending branch `{}`",
                current.branch, node.branch
            )));
        }
        if !self.mappings.contains_key(&current.commit) {
            return Err(Error::InvalidPlan(format!(
                "current commit `{}` for branch `{}` has no rewritten mapping",
                current.commit, current.branch
            )));
        }

        commits
            .iter()
            .position(|commit| commit.oid == current.commit)
            .map(|index| index + 1)
            .ok_or_else(|| {
                Error::InvalidPlan(format!(
                    "current commit `{}` is not part of branch `{}`",
                    current.commit, current.branch
                ))
            })
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

    fn actual_replay_base(&self, node: &Node) -> Result<CommitId> {
        if node.is_root() {
            return Ok(self.state.new_tip.clone());
        }

        let parent_branch = node.parent().ok_or_else(|| {
            Error::InvalidPlan(format!("root node `{}` has no branch parent", node.branch))
        })?;
        let parent = self.node(parent_branch)?;

        self.branch_strategy.actual_child_base(
            parent,
            node,
            &self.selected_bases,
            &self.mappings,
            &self.temp_tips,
        )
    }

    fn record_temp_ref(&mut self, branch: &BranchName, temp_ref: GitRef, branch_tip: CommitId) {
        self.temp_tips.insert(branch.clone(), branch_tip);
        if !self.state.completed_temp_refs.contains(&temp_ref) {
            self.state.completed_temp_refs.push(temp_ref);
        }
    }

    fn node(&self, branch: &str) -> Result<&Node> {
        let index = self
            .nodes
            .get(&BranchName::from_git_unchecked(branch))
            .ok_or_else(|| Error::InvalidPlan(format!("unknown branch `{branch}`")))?;
        self.plan
            .nodes
            .get(*index)
            .ok_or_else(|| Error::InvalidPlan(format!("unknown branch `{branch}`")))
    }

    fn replay_current(&self) -> Option<CurrentState> {
        match &self.state.phase {
            Phase::Replay { current } => current.clone(),
            _ => None,
        }
    }

    fn branch_index(&self) -> usize {
        self.state.completed_temp_refs.len() + 1
    }

    fn total_branches(&self) -> usize {
        self.plan.nodes.len()
    }

    fn write_state(&mut self) -> Result<()> {
        self.state.mappings = self.mappings.clone();
        self.state_writer.write_state(&mut self.state)
    }

    fn mapped_commit(&self, commit: &CommitId) -> Result<&CommitId> {
        self.mappings.get(commit).ok_or_else(|| {
            Error::InvalidPlan(format!("commit `{commit}` has no rewritten mapping"))
        })
    }

    fn can_keep_existing_commit(&self, commit: &PlanCommit, last_rewritten: &CommitId) -> bool {
        commit.parents.first() == Some(last_rewritten)
    }
}

fn branch_strategy(strategy: Strategy) -> Box<dyn ReplayBranchStrategy> {
    match strategy {
        Strategy::PreserveForkPoints => Box::new(PreserveForkPoints),
        Strategy::MoveToPlannedTips => Box::new(MoveToPlannedTips),
        Strategy::MoveToCurrentTips => Box::new(MoveToCurrentTips),
        Strategy::Squash => Box::new(SquashBranches),
    }
}

fn pause_strategy(mode: ReplayPauseMode) -> Box<dyn ReplayPauseStrategy> {
    match mode {
        ReplayPauseMode::Never => Box::new(NeverPause),
        ReplayPauseMode::EveryCommit => Box::new(PauseEveryCommit),
        ReplayPauseMode::Checkpoints => Box::new(PauseAtCheckpoints),
    }
}

impl ReplayBranchStrategy for PreserveForkPoints {
    fn checkpoint_pause_commits(
        &self,
        plan: &Plan,
        node: &Node,
        commits: &[PlanCommit],
    ) -> BTreeSet<CommitId> {
        let Some(last_commit) = commits.last() else {
            return BTreeSet::new();
        };
        let bases = child_replay_bases(plan, node, commits);
        bases
            .filter(|base| *base != node.base())
            .filter(|base| *base != last_commit.oid.as_str())
            .map(CommitId::new)
            .collect()
    }

    fn actual_child_base(
        &self,
        parent: &Node,
        child: &Node,
        selected_bases: &HashMap<BranchName, CommitId>,
        mappings: &BTreeMap<CommitId, CommitId>,
        _temp_tips: &HashMap<BranchName, CommitId>,
    ) -> Result<CommitId> {
        preserve_fork_point_child_base(parent, child, selected_bases, mappings)
    }

    fn required_child_replay_base(
        &self,
        parent: &Node,
        child: &Node,
        selected_bases: &HashMap<BranchName, CommitId>,
        mappings: &BTreeMap<CommitId, CommitId>,
    ) -> Result<Option<(CommitId, String)>> {
        preserve_fork_point_child_base(parent, child, selected_bases, mappings)
            .map(|base| Some(child_replay_base_requirement(child, base)))
    }
}

impl ReplayBranchStrategy for MoveToPlannedTips {
    fn checkpoint_pause_commits(
        &self,
        _plan: &Plan,
        node: &Node,
        commits: &[PlanCommit],
    ) -> BTreeSet<CommitId> {
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

    fn actual_child_base(
        &self,
        parent: &Node,
        _child: &Node,
        _selected_bases: &HashMap<BranchName, CommitId>,
        mappings: &BTreeMap<CommitId, CommitId>,
        _temp_tips: &HashMap<BranchName, CommitId>,
    ) -> Result<CommitId> {
        planned_parent_tip(parent, mappings)
    }

    fn required_child_replay_base(
        &self,
        parent: &Node,
        child: &Node,
        _selected_bases: &HashMap<BranchName, CommitId>,
        mappings: &BTreeMap<CommitId, CommitId>,
    ) -> Result<Option<(CommitId, String)>> {
        planned_parent_tip(parent, mappings)
            .map(|base| Some(child_replay_base_requirement(child, base)))
    }
}

impl ReplayBranchStrategy for MoveToCurrentTips {
    fn checkpoint_pause_commits(
        &self,
        _plan: &Plan,
        _node: &Node,
        _commits: &[PlanCommit],
    ) -> BTreeSet<CommitId> {
        BTreeSet::new()
    }

    fn actual_child_base(
        &self,
        parent: &Node,
        _child: &Node,
        _selected_bases: &HashMap<BranchName, CommitId>,
        _mappings: &BTreeMap<CommitId, CommitId>,
        temp_tips: &HashMap<BranchName, CommitId>,
    ) -> Result<CommitId> {
        current_parent_tip(parent, temp_tips)
    }

    fn required_child_replay_base(
        &self,
        _parent: &Node,
        _child: &Node,
        _selected_bases: &HashMap<BranchName, CommitId>,
        _mappings: &BTreeMap<CommitId, CommitId>,
    ) -> Result<Option<(CommitId, String)>> {
        Ok(None)
    }
}

impl ReplayBranchStrategy for SquashBranches {
    fn checkpoint_pause_commits(
        &self,
        _plan: &Plan,
        _node: &Node,
        _commits: &[PlanCommit],
    ) -> BTreeSet<CommitId> {
        BTreeSet::new()
    }

    fn every_commit_pause_count(&self, commits: &[PlanCommit]) -> usize {
        commits.len().saturating_sub(1)
    }

    fn every_commit_branch_end(&self, commits: &[PlanCommit], unchanged_tip: bool) -> BranchEnd {
        if commits.is_empty() {
            complete_branch_end(unchanged_tip)
        } else {
            BranchEnd::Pause {
                prepare_worktree: unchanged_tip,
            }
        }
    }

    fn branch_tip_rewrite<'commit>(
        &self,
        commits: &'commit [PlanCommit],
    ) -> BranchTipRewrite<'commit> {
        if commits.len() > 1 {
            BranchTipRewrite::Squash {
                first_commit: commits
                    .first()
                    .expect("non-empty commits has a first commit"),
            }
        } else {
            BranchTipRewrite::Keep
        }
    }

    fn actual_child_base(
        &self,
        parent: &Node,
        _child: &Node,
        _selected_bases: &HashMap<BranchName, CommitId>,
        _mappings: &BTreeMap<CommitId, CommitId>,
        temp_tips: &HashMap<BranchName, CommitId>,
    ) -> Result<CommitId> {
        current_parent_tip(parent, temp_tips)
    }

    fn required_child_replay_base(
        &self,
        _parent: &Node,
        _child: &Node,
        _selected_bases: &HashMap<BranchName, CommitId>,
        _mappings: &BTreeMap<CommitId, CommitId>,
    ) -> Result<Option<(CommitId, String)>> {
        Ok(None)
    }
}

impl ReplayPauseStrategy for NeverPause {
    fn pause_commits(
        &self,
        _branch_strategy: &dyn ReplayBranchStrategy,
        _plan: &Plan,
        _node: &Node,
        _commits: &[PlanCommit],
    ) -> BTreeSet<CommitId> {
        BTreeSet::new()
    }

    fn branch_end(
        &self,
        _branch_strategy: &dyn ReplayBranchStrategy,
        _commits: &[PlanCommit],
        unchanged_tip: bool,
    ) -> BranchEnd {
        complete_branch_end(unchanged_tip)
    }
}

impl ReplayPauseStrategy for PauseEveryCommit {
    fn pause_commits(
        &self,
        branch_strategy: &dyn ReplayBranchStrategy,
        _plan: &Plan,
        _node: &Node,
        commits: &[PlanCommit],
    ) -> BTreeSet<CommitId> {
        commits
            .iter()
            .take(branch_strategy.every_commit_pause_count(commits))
            .map(|commit| commit.oid.clone())
            .collect()
    }

    fn branch_end(
        &self,
        branch_strategy: &dyn ReplayBranchStrategy,
        commits: &[PlanCommit],
        unchanged_tip: bool,
    ) -> BranchEnd {
        branch_strategy.every_commit_branch_end(commits, unchanged_tip)
    }
}

impl ReplayPauseStrategy for PauseAtCheckpoints {
    fn pause_commits(
        &self,
        branch_strategy: &dyn ReplayBranchStrategy,
        plan: &Plan,
        node: &Node,
        commits: &[PlanCommit],
    ) -> BTreeSet<CommitId> {
        branch_strategy.checkpoint_pause_commits(plan, node, commits)
    }

    fn branch_end(
        &self,
        _branch_strategy: &dyn ReplayBranchStrategy,
        _commits: &[PlanCommit],
        unchanged_tip: bool,
    ) -> BranchEnd {
        BranchEnd::Pause {
            prepare_worktree: unchanged_tip,
        }
    }
}

fn complete_branch_end(unchanged_tip: bool) -> BranchEnd {
    BranchEnd::Complete {
        ref_update: if unchanged_tip {
            BranchRefUpdate::Skip
        } else {
            BranchRefUpdate::Write
        },
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

fn selected_bases_from_mappings(
    plan: &Plan,
    mappings: &BTreeMap<CommitId, CommitId>,
) -> HashMap<BranchName, CommitId> {
    plan.nodes
        .iter()
        .filter_map(|node| {
            mappings
                .get(&node.base)
                .map(|base| (node.branch.clone(), base.clone()))
        })
        .collect()
}
