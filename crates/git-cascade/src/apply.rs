use crate::git::Git;
use crate::plan::{
    branches_in_topological_order, validate_branch_refs, validate_merge_parents_for_apply,
    validate_plan, BranchRef, Node, Plan, PlanCommit, PlanName,
};
use crate::replay_backend::{
    CherryPickOutcome, DryRunReplayBackend, GitReplayBackend, ReplayBackend,
};
use crate::state::{
    initial_apply_state, ApplyState, CurrentState, InitialApplyStateInput, PausedState, Phase,
    ReplayMode, RestoreState, StateFile, Strategy, WorktreeState,
};
use crate::state_writer::{LockedStateWriter, NoopStateWriter, StateWriter};
use crate::storage::Storage;
use crate::test_hooks;
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub plan_name: PlanName,
    pub new_tip_input: String,
    pub strategy: Strategy,
    pub in_place: bool,
    pub pause_at_checkpoints: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    Complete,
    Conflict {
        current: CurrentState,
        message: String,
    },
    Paused {
        paused: PausedState,
    },
}

struct ReplayContext<'state> {
    plan: Plan,
    state_writer: &'state mut dyn StateWriter,
    backend: &'state mut dyn ReplayBackend,
    state: ApplyState,
    nodes: HashMap<String, usize>,
    temp_tips: HashMap<String, String>,
    selected_bases: HashMap<String, String>,
    total_branches: usize,
}

pub fn dry_run(git: &Git, storage: &Storage, plan: &Plan, options: ApplyOptions) -> Result<String> {
    validate_plan(git, plan)?;
    let branch_refs = validate_branch_refs(git, plan)?;
    let new_tip = git.resolve_commit(&options.new_tip_input)?;
    validate_merge_parents_for_apply(git, plan, &branch_refs, &new_tip)?;
    let ordered = branches_in_topological_order(plan)?;
    let worktree = if options.in_place {
        git.worktree_root()?
    } else {
        storage.worktrees_dir().join(plan.plan_id.to_string())
    };
    let worktree_state = if options.in_place {
        WorktreeState::InPlace {
            path: worktree.display().to_string(),
            restore: restore_state(git)?,
        }
    } else {
        WorktreeState::Temporary {
            path: worktree.display().to_string(),
        }
    };
    let (branch_tips, extra_commits) = branch_tips_and_extra_commits(branch_refs);
    let mappings = BTreeMap::new();
    let state = initial_apply_state(InitialApplyStateInput {
        plan_name: &options.plan_name,
        plan_id: &plan.plan_id,
        new_tip: &new_tip,
        strategy: options.strategy,
        replay_mode: replay_mode(&options),
        pending_branches: ordered,
        branch_tips,
        extra_commits,
        mappings,
        worktree: worktree_state,
    })?;
    let mut state_writer = NoopStateWriter;
    let mut backend = DryRunReplayBackend::new(git, storage, plan, &state)?;
    run_replay_context(plan.clone(), &mut state_writer, &mut backend, state, false)?;

    Ok(backend.finish())
}

pub fn execute(
    git: &Git,
    storage: &Storage,
    plan: &Plan,
    options: ApplyOptions,
) -> Result<ApplyOutcome> {
    validate_plan(git, plan)?;
    let branch_refs = validate_branch_refs(git, plan)?;
    let new_tip = git.resolve_commit(&options.new_tip_input)?;
    validate_merge_parents_for_apply(git, plan, &branch_refs, &new_tip)?;
    let ordered = branches_in_topological_order(plan)?;
    let (worktree_state, worktree) = if options.in_place {
        let worktree = git.worktree_root()?;
        git.ensure_clean_worktree()?;
        ensure_target_branches_not_checked_out_except(git, &ordered, &worktree)?;
        (
            WorktreeState::InPlace {
                path: worktree.display().to_string(),
                restore: restore_state(git)?,
            },
            worktree,
        )
    } else {
        let worktree = storage.worktrees_dir().join(plan.plan_id.to_string());
        ensure_target_branches_not_checked_out(git, &ordered)?;
        (
            WorktreeState::Temporary {
                path: worktree.display().to_string(),
            },
            worktree,
        )
    };
    let (branch_tips, extra_commits) = branch_tips_and_extra_commits(branch_refs);
    let mappings = BTreeMap::new();
    let state = initial_apply_state(InitialApplyStateInput {
        plan_name: &options.plan_name,
        plan_id: &plan.plan_id,
        new_tip: &new_tip,
        strategy: options.strategy,
        replay_mode: replay_mode(&options),
        pending_branches: ordered,
        branch_tips,
        extra_commits,
        mappings,
        worktree: worktree_state.clone(),
    })?;
    let state_file = StateFile::create(storage, &state)?;

    if worktree_state.is_temporary() {
        storage.ensure_worktrees_dir()?;
        cleanup_stale_worktree(git, &worktree)?;
    }
    let mut state_writer = LockedStateWriter::new(state_file);
    let mut backend = GitReplayBackend::new(git, storage);
    run_replay_context(plan.clone(), &mut state_writer, &mut backend, state, true)
}

pub fn continue_apply(git: &Git, storage: &Storage) -> Result<ApplyOutcome> {
    let mut state_file = StateFile::open(storage)?
        .ok_or_else(|| Error::InvalidInvocation("no active cascade operation".to_owned()))?;
    let mut state = state_file.read_state()?;

    let mut state_writer = LockedStateWriter::new(state_file);
    let mut backend = GitReplayBackend::new(git, storage);
    if matches!(state.phase, Phase::Deleting { .. }) {
        run_deleting_state(&mut state_writer, &mut backend, &mut state)?;
        Ok(ApplyOutcome::Complete)
    } else {
        let plan_name = state.plan_name.clone();
        let plan = Plan::from_yaml(&storage.read_plan(&plan_name)?)?;
        // Branch refs are not re-checked here: they may legitimately already
        // point at rewritten tips when resuming a final update.
        validate_plan(git, &plan)?;
        run_replay_context(plan, &mut state_writer, &mut backend, state, true)
    }
}

pub fn abort(git: &Git, storage: &Storage) -> Result<()> {
    let Some(mut state_file) = StateFile::open(storage)? else {
        return Err(Error::InvalidInvocation(
            "no active cascade operation".to_owned(),
        ));
    };
    let mut state = state_file.read_state()?;

    if !matches!(state.phase, Phase::Deleting { .. }) {
        state.phase = Phase::Deleting { delete_plan: false };
        state_file.write_state(&mut state)?;
    }

    let mut state_writer = LockedStateWriter::new(state_file);
    let mut backend = GitReplayBackend::new(git, storage);
    run_deleting_state(&mut state_writer, &mut backend, &mut state)
}

fn restore_state(git: &Git) -> Result<RestoreState> {
    let head = git.head_oid()?;
    Ok(if let Some(name) = git.current_branch()? {
        RestoreState::Branch { name, head }
    } else {
        RestoreState::Detached { head }
    })
}

fn run_replay_context(
    plan: Plan,
    state_writer: &mut dyn StateWriter,
    backend: &mut dyn ReplayBackend,
    state: ApplyState,
    stop_on_pause: bool,
) -> Result<ApplyOutcome> {
    ReplayContext::new(plan, state_writer, backend, state)?.run(stop_on_pause)
}

fn replay_mode(options: &ApplyOptions) -> ReplayMode {
    if options.pause_at_checkpoints {
        ReplayMode::PauseAtCheckpoints
    } else {
        ReplayMode::RunToCompletion
    }
}

fn run_deleting_state(
    state_writer: &mut dyn StateWriter,
    backend: &mut dyn ReplayBackend,
    state: &mut ApplyState,
) -> Result<()> {
    let delete_plan = match &state.phase {
        Phase::Deleting { delete_plan } => *delete_plan,
        _ => {
            return Err(Error::InvalidInvocation(
                "active apply state is not in deleting phase".to_owned(),
            ));
        }
    };
    if delete_plan {
        backend.delete_applied_plan(state)?;
    }
    backend.cleanup_deleting_state(state)?;
    state_writer.remove_state()
}

impl<'state> ReplayContext<'state> {
    fn new(
        plan: Plan,
        state_writer: &'state mut dyn StateWriter,
        backend: &'state mut dyn ReplayBackend,
        state: ApplyState,
    ) -> Result<Self> {
        let nodes = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.branch.clone(), index))
            .collect::<HashMap<_, _>>();
        let temp_tips = backend.temp_tips(&state.completed_temp_refs)?;
        let selected_bases = selected_bases_from_mappings(&plan, &state.mappings);
        let total_branches = plan.nodes.len();

        Ok(Self {
            plan,
            state_writer,
            backend,
            state,
            nodes,
            temp_tips,
            selected_bases,
            total_branches,
        })
    }

    fn run(&mut self, stop_on_pause: bool) -> Result<ApplyOutcome> {
        self.backend.start(&self.state)?;
        let mut resume_initial_pause = matches!(self.state.phase, Phase::Paused { .. });
        loop {
            match self.state.phase.clone() {
                Phase::Replay { .. } => match self.replay_pending_branches()? {
                    ApplyOutcome::Complete => {
                        self.state.phase = Phase::FinalUpdate;
                        self.write_state()?;
                    }
                    outcome @ (ApplyOutcome::Paused { .. } | ApplyOutcome::Conflict { .. }) => {
                        return Ok(outcome);
                    }
                },
                Phase::FinalUpdate => {
                    self.backend.final_update(&self.plan, &self.state)?;
                    test_hooks::run("after-final-update")?;
                    self.state.phase = Phase::Deleting { delete_plan: true };
                    self.write_state()?;
                    test_hooks::run("after-deleting-state-written")?;
                }
                Phase::Conflict { current } => {
                    self.resolve_conflict(current)?;
                    self.write_state()?;
                }
                Phase::Paused { paused } => {
                    if stop_on_pause && !resume_initial_pause {
                        return Ok(ApplyOutcome::Paused { paused });
                    }
                    resume_initial_pause = false;
                    self.resume_paused_branch(paused)?;
                }
                Phase::Deleting { .. } => {
                    run_deleting_state(self.state_writer, self.backend, &mut self.state)?;
                    return Ok(ApplyOutcome::Complete);
                }
            }
        }
    }

    fn replay_pending_branches(&mut self) -> Result<ApplyOutcome> {
        if self.total_branches == 0 {
            self.backend.no_branches()?;
        }

        while let Some(branch) = self.state.pending_branches.first().cloned() {
            let node = self.node(&branch)?.clone();
            let branch_index = self.branch_index();
            let commits = replay_commits_from_extra(&node, &self.state.extra_commits);
            let replay_current = self.replay_current();
            let was_resuming = replay_current.is_some();
            let child_base_pause_commits = if self.state.replay_mode.pauses_at_checkpoints() {
                child_base_pause_commits(&self.plan, &node, self.state.strategy, &commits)
            } else {
                BTreeSet::new()
            };

            let (start_commit_index, mut last_rewritten) = if let Some(current) = replay_current {
                let start = self.resume_start_commit_index(&node, &current, &commits)?;
                self.state.phase = Phase::Replay { current: None };
                let head = commits
                    .get(start.wrapping_sub(1))
                    .and_then(|commit| self.state.mappings.get(&commit.oid))
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
                self.state
                    .mappings
                    .insert(node.base().to_owned(), base.clone());

                if base == node.base() {
                    self.skip_replay_at_existing_base(&node, &branch, &commits)?;
                    continue;
                }

                self.backend.prepare_branch(
                    &self.state,
                    branch_index,
                    self.total_branches,
                    &node,
                    &base,
                )?;
                (0, base)
            };

            self.backend.start_replay(
                branch_index,
                self.total_branches,
                &node,
                commits.len(),
                start_commit_index,
                was_resuming,
            )?;
            for (commit_index, commit) in commits.iter().enumerate().skip(start_commit_index) {
                if commit.is_merge() {
                    // The merged history is contained in the new base; flatten.
                    self.backend
                        .flatten_merge(&node, &commit.oid, commit_index, commits.len())?;
                    self.state
                        .mappings
                        .insert(commit.oid.clone(), last_rewritten.clone());
                    if child_base_pause_commits.contains(&commit.oid) {
                        let paused =
                            self.checkpoint_paused_child_base(&node, &commit.oid, &last_rewritten)?;
                        return Ok(ApplyOutcome::Paused { paused });
                    }
                    continue;
                }

                let rewritten_commit = match self.backend.cherry_pick(
                    &self.state,
                    &node,
                    &commit.oid,
                    commit_index,
                    commits.len(),
                )? {
                    CherryPickOutcome::Applied(rewritten_commit) => rewritten_commit,
                    CherryPickOutcome::Conflict { message } => {
                        let current = CurrentState {
                            branch: node.branch.clone(),
                            commit: commit.oid.clone(),
                            worktree: self.state.worktree.path().to_owned(),
                        };
                        self.state.phase = Phase::Conflict {
                            current: current.clone(),
                        };
                        self.write_state()?;
                        return Ok(ApplyOutcome::Conflict { current, message });
                    }
                };
                self.state
                    .mappings
                    .insert(commit.oid.clone(), rewritten_commit.clone());
                last_rewritten = rewritten_commit;
                if child_base_pause_commits.contains(&commit.oid) {
                    let paused =
                        self.checkpoint_paused_child_base(&node, &commit.oid, &last_rewritten)?;
                    return Ok(ApplyOutcome::Paused { paused });
                }
            }

            let rewritten_tip = if let Some(commit) = commits.last() {
                self.state
                    .mappings
                    .get(&commit.oid)
                    .cloned()
                    .ok_or_else(|| {
                        Error::InvalidPlan(format!(
                            "commit `{}` for branch `{}` has no rewritten mapping",
                            commit.oid, node.branch
                        ))
                    })?
            } else {
                self.selected_bases
                    .get(&node.branch)
                    .cloned()
                    .ok_or_else(|| {
                        Error::InvalidPlan(format!("branch `{}` has no selected base", node.branch))
                    })?
            };
            let (temp_ref, branch_tip) = self.backend.write_temp_ref(
                &self.plan,
                &node,
                branch_index,
                self.total_branches,
                &rewritten_tip,
            )?;
            self.record_temp_ref(&node.branch, temp_ref.clone(), branch_tip.clone());
            self.remove_pending_branch(&branch)?;
            let pause =
                self.state
                    .replay_mode
                    .pauses_at_checkpoints()
                    .then(|| PausedState::BranchEnd {
                        branch: node.branch.clone(),
                        rewritten_tip: branch_tip,
                        temp_ref,
                        mapped_commit: commits
                            .last()
                            .map(|commit| commit.oid.clone())
                            .unwrap_or_else(|| node.base().to_owned()),
                        worktree: self.state.worktree.path().to_owned(),
                    });
            self.checkpoint_completed_branch(pause.clone())?;
            if let Some(paused) = pause {
                return Ok(ApplyOutcome::Paused { paused });
            }
        }

        self.state.phase = Phase::Replay { current: None };
        Ok(ApplyOutcome::Complete)
    }

    fn resolve_conflict(&mut self, current: CurrentState) -> Result<()> {
        let rewritten_commit = self.backend.continue_cherry_pick(&self.state, &current)?;
        self.state
            .mappings
            .insert(current.commit.clone(), rewritten_commit);
        self.state.phase = Phase::Replay {
            current: Some(current),
        };
        Ok(())
    }

    fn resume_paused_branch(&mut self, paused: PausedState) -> Result<()> {
        if !self
            .plan
            .nodes
            .iter()
            .any(|node| node.branch == paused.branch())
        {
            return Err(Error::InvalidPlan(format!(
                "paused branch `{}` is not in the active plan",
                paused.branch()
            )));
        }

        let rewritten_tip = self.backend.resume_paused_branch(&self.state, &paused)?;
        match paused {
            PausedState::BranchEnd {
                branch,
                mapped_commit,
                temp_ref,
                ..
            } => {
                self.record_temp_ref(&branch, temp_ref, rewritten_tip.clone());
                self.state.mappings.insert(mapped_commit, rewritten_tip);
                self.state.phase = Phase::Replay { current: None };
            }
            PausedState::ChildBase {
                branch,
                commit,
                worktree,
                ..
            } => {
                self.state.mappings.insert(commit.clone(), rewritten_tip);
                self.state.phase = Phase::Replay {
                    current: Some(CurrentState {
                        branch,
                        commit,
                        worktree,
                    }),
                };
            }
        }
        self.write_state()
    }

    fn skip_replay_at_existing_base(
        &mut self,
        node: &Node,
        branch: &str,
        commits: &[PlanCommit],
    ) -> Result<()> {
        for commit in commits {
            self.state
                .mappings
                .insert(commit.oid.clone(), commit.oid.clone());
        }
        let current_tip = commits
            .last()
            .map(|commit| commit.oid.clone())
            .ok_or_else(|| {
                Error::InvalidPlan(format!("branch `{}` has no commits", node.branch))
            })?;
        let (temp_ref, branch_tip) = self.backend.skip_replay(
            &self.plan,
            node,
            self.branch_index(),
            self.total_branches,
            &current_tip,
        )?;
        self.record_temp_ref(&node.branch, temp_ref, branch_tip);
        self.remove_pending_branch(branch)?;
        self.checkpoint_completed_branch(None)
    }

    fn checkpoint_paused_child_base(
        &mut self,
        node: &Node,
        commit: &str,
        rewritten_tip: &str,
    ) -> Result<PausedState> {
        let paused = PausedState::ChildBase {
            branch: node.branch.clone(),
            commit: commit.to_owned(),
            rewritten_tip: rewritten_tip.to_owned(),
            worktree: self.state.worktree.path().to_owned(),
        };
        self.state.phase = Phase::Paused {
            paused: paused.clone(),
        };
        self.write_state()?;
        Ok(paused)
    }

    /// Persists progress after a branch finished so a crashed apply can resume
    /// from the next pending branch.
    fn checkpoint_completed_branch(&mut self, pause: Option<PausedState>) -> Result<()> {
        self.state.phase = if let Some(paused) = pause {
            Phase::Paused { paused }
        } else {
            Phase::Replay { current: None }
        };
        self.write_state()
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
        if !self.state.mappings.contains_key(&current.commit) {
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

    fn remove_pending_branch(&mut self, branch: &str) -> Result<()> {
        if self.state.pending_branches.first().map(String::as_str) != Some(branch) {
            return Err(Error::InvalidPlan(format!(
                "completed branch `{branch}` is not first in pending state"
            )));
        }
        self.state.pending_branches.remove(0);
        Ok(())
    }

    fn actual_replay_base(&self, node: &Node) -> Result<String> {
        if node.is_root() {
            return Ok(self.state.new_tip.clone());
        }

        let parent_branch = node.parent().ok_or_else(|| {
            Error::InvalidPlan(format!("root node `{}` has no branch parent", node.branch))
        })?;
        let parent = self.node(parent_branch)?;

        if self.state.strategy == Strategy::MoveToPlannedTips {
            return self
                .state
                .mappings
                .get(&parent.tip)
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!(
                        "parent `{}` has no rewritten planned tip",
                        parent.branch
                    ))
                });
        }

        if self.state.strategy == Strategy::MoveToCurrentTips {
            return self.temp_tips.get(&parent.branch).cloned().ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no rewritten tip", parent.branch))
            });
        }

        let base = node.base();
        if base == parent.base() {
            return self
                .selected_bases
                .get(&parent.branch)
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!("parent `{}` has no selected base", parent.branch))
                });
        }

        self.state.mappings.get(base).cloned().ok_or_else(|| {
            Error::InvalidPlan(format!(
                "base `{}` for branch `{}` was not mapped",
                base, node.branch
            ))
        })
    }

    fn record_temp_ref(&mut self, branch: &str, temp_ref: String, branch_tip: String) {
        self.temp_tips.insert(branch.to_owned(), branch_tip);
        if !self.state.completed_temp_refs.contains(&temp_ref) {
            self.state.completed_temp_refs.push(temp_ref);
        }
    }

    fn node(&self, branch: &str) -> Result<&Node> {
        let index = self
            .nodes
            .get(branch)
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

    fn write_state(&mut self) -> Result<()> {
        self.state_writer.write_state(&mut self.state)
    }
}

fn ensure_target_branches_not_checked_out(git: &Git, branches: &[String]) -> Result<()> {
    let checked_out = git.checked_out_branches()?;
    ensure_branches_not_checked_out(branches, &checked_out)
}

fn ensure_target_branches_not_checked_out_except(
    git: &Git,
    branches: &[String],
    excluded_path: &std::path::Path,
) -> Result<()> {
    let checked_out = git.checked_out_branches_except(excluded_path)?;
    ensure_branches_not_checked_out(branches, &checked_out)
}

fn ensure_branches_not_checked_out(branches: &[String], checked_out: &[String]) -> Result<()> {
    let blocked = branches
        .iter()
        .filter(|branch| checked_out.contains(branch))
        .cloned()
        .collect::<Vec<_>>();
    if blocked.is_empty() {
        return Ok(());
    }

    Err(Error::InvalidInvocation(format!(
        "cannot apply while target branch(es) are checked out in a worktree: {}. Switch those worktrees to another branch or a detached HEAD before running apply.",
        blocked.join(", ")
    )))
}

fn branch_tips_and_extra_commits(
    branch_refs: BTreeMap<String, BranchRef>,
) -> (BTreeMap<String, String>, BTreeMap<String, Vec<PlanCommit>>) {
    let mut branch_tips = BTreeMap::new();
    let mut extra_commits = BTreeMap::new();
    for (branch, branch_ref) in branch_refs {
        branch_tips.insert(branch.clone(), branch_ref.expected_tip);
        extra_commits.insert(branch, branch_ref.extra_commits);
    }

    (branch_tips, extra_commits)
}

fn replay_commits_from_extra(
    node: &Node,
    extra_commits: &BTreeMap<String, Vec<PlanCommit>>,
) -> Vec<PlanCommit> {
    let mut commits = node.commits().to_vec();
    if let Some(extra) = extra_commits.get(&node.branch) {
        commits.extend(extra.iter().cloned());
    }
    commits
}

fn child_base_pause_commits(
    plan: &Plan,
    node: &Node,
    strategy: Strategy,
    commits: &[PlanCommit],
) -> BTreeSet<String> {
    let Some(last_commit) = commits.last() else {
        return BTreeSet::new();
    };
    let has_child = plan
        .nodes
        .iter()
        .any(|child| child.parent() == Some(node.branch.as_str()));
    if !has_child {
        return BTreeSet::new();
    }

    let commit_oids = commits
        .iter()
        .map(|commit| commit.oid.as_str())
        .collect::<BTreeSet<_>>();
    match strategy {
        Strategy::MoveToCurrentTips => BTreeSet::new(),
        Strategy::MoveToPlannedTips => {
            if node.tip != last_commit.oid && commit_oids.contains(node.tip.as_str()) {
                BTreeSet::from([node.tip.clone()])
            } else {
                BTreeSet::new()
            }
        }
        Strategy::PreserveForkPoints => plan
            .nodes
            .iter()
            .filter(|child| child.parent() == Some(node.branch.as_str()))
            .map(Node::base)
            .filter(|base| *base != node.base())
            .filter(|base| *base != last_commit.oid)
            .filter(|base| commit_oids.contains(*base))
            .map(str::to_owned)
            .collect(),
    }
}

fn selected_bases_from_mappings(
    plan: &Plan,
    mappings: &BTreeMap<String, String>,
) -> HashMap<String, String> {
    plan.nodes
        .iter()
        .filter_map(|node| {
            mappings
                .get(node.base())
                .map(|base| (node.branch.clone(), base.clone()))
        })
        .collect()
}

fn cleanup_stale_worktree(git: &Git, worktree: &std::path::Path) -> Result<()> {
    if !worktree.exists() {
        return Ok(());
    }

    let _ = git.worktree_remove_force(worktree);
    if worktree.exists() {
        fs::remove_dir_all(worktree).map_err(|source| Error::IoWithPath {
            path: worktree.to_owned(),
            source,
        })?;
    }

    Ok(())
}
