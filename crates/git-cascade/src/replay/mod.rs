mod backend;
mod cleanup;
mod context;
pub mod state;
mod state_writer;

use crate::git::Git;
use crate::model::Strategy;
use crate::model::{BranchName, CommitId, GitRef};
use crate::plan::{
    BranchRef, Plan, PlanCommit, PlanName, branches_in_topological_order, validate_branch_refs,
    validate_merge_parents_for_apply, validate_plan,
};
use crate::storage::Storage;
use crate::{Error, Result};
use backend::{DryRunReplayBackend, GitReplayBackend, ReplayBackend};
use cleanup::run_deleting_phase;
use context::ReplayContext;
pub use state::{
    CurrentState, PausedState, Phase, ReplayMode, ReplayState, RestoreState, WorktreeState,
};
use state::{InitialReplayStateInput, StateFile, initial_replay_state};
use state_writer::{LockedStateWriter, NoopStateWriter, StateWriter};
use std::collections::BTreeMap;
use std::fs;

#[derive(Debug, Clone)]
pub struct ReplayOptions {
    pub plan_name: PlanName,
    pub new_tip_input: GitRef,
    pub strategy: Strategy,
    pub in_place: bool,
    pub pause_at_checkpoints: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayOutcome {
    Complete,
    Conflict {
        current: CurrentState,
        message: String,
    },
    Paused {
        paused: PausedState,
    },
}

pub fn dry_run(
    git: &Git,
    storage: &Storage,
    plan: &Plan,
    options: ReplayOptions,
) -> Result<String> {
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
    let state = initial_replay_state(InitialReplayStateInput {
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
    {
        let mut replay = ReplayContext::new(plan, &mut state_writer, &mut backend, state)?;
        loop {
            // drive replay to completions, as there are no manual resolutions in dry run
            match replay.run()? {
                ReplayOutcome::Complete => break,
                ReplayOutcome::Paused { .. } => replay.continue_after_pause_or_conflict(),
                ReplayOutcome::Conflict { .. } => break,
            }
        }
    }

    Ok(backend.finish())
}

pub fn execute(
    git: &Git,
    storage: &Storage,
    plan: &Plan,
    options: ReplayOptions,
) -> Result<ReplayOutcome> {
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
    let state = initial_replay_state(InitialReplayStateInput {
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
    let mut backend = GitReplayBackend::new(git);
    let mut context = ReplayContext::new(plan, &mut state_writer, &mut backend, state)?;
    let outcome = context.run()?;

    if matches!(outcome, ReplayOutcome::Complete) {
        let mut state = context.into_state();
        run_deleting_phase(git, storage, &mut state_writer, &mut state)?;
    };

    Ok(outcome)
}

pub fn continue_replay(git: &Git, storage: &Storage) -> Result<ReplayOutcome> {
    let mut state_file = StateFile::open(storage)?
        .ok_or_else(|| Error::InvalidInvocation("no active cascade operation".to_owned()))?;
    let mut state = state_file.read_state()?;

    let mut state_writer = LockedStateWriter::new(state_file);
    let mut backend = GitReplayBackend::new(git);
    if matches!(state.phase, Phase::Deleting { .. }) {
        run_deleting_phase(git, storage, &mut state_writer, &mut state)?;
        Ok(ReplayOutcome::Complete)
    } else if matches!(state.phase, Phase::RestoreCheckout { .. }) {
        run_restore_checkout_phase(git, storage, &mut state_writer, &mut backend, &mut state)
    } else {
        let plan_name = state.plan_name.clone();
        let plan = Plan::from_yaml(&storage.read_plan(&plan_name)?)?;
        // Branch refs are not re-checked here: they may legitimately already
        // point at rewritten tips when resuming a final update.
        validate_plan(git, &plan)?;
        let mut context = ReplayContext::new(&plan, &mut state_writer, &mut backend, state)?;
        context.continue_after_pause_or_conflict();
        let outcome = context.run()?;

        if matches!(outcome, ReplayOutcome::Complete) {
            let mut state = context.into_state();
            run_deleting_phase(git, storage, &mut state_writer, &mut state)?;
        };

        Ok(outcome)
    }
}

pub fn abort(git: &Git, storage: &Storage) -> Result<()> {
    let Some(mut state_file) = StateFile::open(storage)? else {
        return Err(Error::InvalidInvocation(
            "no active cascade operation".to_owned(),
        ));
    };
    let mut state = state_file.read_state()?;

    let mut state_writer = LockedStateWriter::new(state_file);
    if matches!(state.phase, Phase::Deleting { .. }) {
        return run_deleting_phase(git, storage, &mut state_writer, &mut state);
    }

    let mut backend = GitReplayBackend::new(git);
    if matches!(state.phase, Phase::RestoreCheckout { .. }) {
        run_restore_checkout_phase(git, storage, &mut state_writer, &mut backend, &mut state)?;
        return Ok(());
    }

    let plan_name = state.plan_name.clone();
    let plan = Plan::from_yaml(&storage.read_plan(&plan_name)?)?;

    if matches!(state.phase, Phase::FinalUpdate) {
        let mut context = ReplayContext::new(&plan, &mut state_writer, &mut backend, state)?;
        if matches!(context.run()?, ReplayOutcome::Complete) {
            let mut state = context.into_state();
            run_deleting_phase(git, storage, &mut state_writer, &mut state)?;
            return Ok(());
        }
        return Err(Error::InvalidInvocation(
            "abort cannot stop an apply that is completing final updates".to_owned(),
        ));
    }

    state.phase = Phase::RestoreCheckout {
        delete_plan: false,
        force_checkout: true,
    };
    state_writer.write_state(&mut state)?;

    let mut context = ReplayContext::new(&plan, &mut state_writer, &mut backend, state)?;
    match context.run()? {
        ReplayOutcome::Complete => {
            let mut state = context.into_state();
            run_deleting_phase(git, storage, &mut state_writer, &mut state)
        }
        ReplayOutcome::Conflict { .. } | ReplayOutcome::Paused { .. } => Err(
            Error::InvalidInvocation("abort cleanup stopped before deleting phase".to_owned()),
        ),
    }
}

fn run_restore_checkout_phase(
    git: &Git,
    storage: &Storage,
    state_writer: &mut dyn StateWriter,
    backend: &mut GitReplayBackend<'_>,
    state: &mut ReplayState,
) -> Result<ReplayOutcome> {
    let Phase::RestoreCheckout {
        delete_plan,
        force_checkout,
    } = &state.phase
    else {
        return Err(Error::InvalidInvocation(
            "active apply state is not in restore-checkout phase".to_owned(),
        ));
    };
    let delete_plan = *delete_plan;
    let force_checkout = *force_checkout;
    backend.restore_checkout(state, force_checkout)?;
    state.phase = Phase::Deleting { delete_plan };
    state_writer.write_state(state)?;
    run_deleting_phase(git, storage, state_writer, state)?;
    Ok(ReplayOutcome::Complete)
}

fn restore_state(git: &Git) -> Result<RestoreState> {
    let head = git.head_oid()?;
    Ok(if let Some(name) = git.current_branch()? {
        RestoreState::Branch { name, head }
    } else {
        RestoreState::Detached { head }
    })
}

fn replay_mode(options: &ReplayOptions) -> ReplayMode {
    if options.pause_at_checkpoints {
        ReplayMode::PauseAtCheckpoints
    } else {
        ReplayMode::RunToCompletion
    }
}

fn ensure_target_branches_not_checked_out(git: &Git, branches: &[BranchName]) -> Result<()> {
    let checked_out = git.checked_out_branches()?;
    ensure_branches_not_checked_out(branches, &checked_out)
}

fn ensure_target_branches_not_checked_out_except(
    git: &Git,
    branches: &[BranchName],
    excluded_path: &std::path::Path,
) -> Result<()> {
    let checked_out = git.checked_out_branches_except(excluded_path)?;
    ensure_branches_not_checked_out(branches, &checked_out)
}

fn ensure_branches_not_checked_out(
    branches: &[BranchName],
    checked_out: &[BranchName],
) -> Result<()> {
    let blocked = branches
        .iter()
        .filter(|branch| checked_out.contains(branch))
        .map(BranchName::as_str)
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
    branch_refs: BTreeMap<BranchName, BranchRef>,
) -> (
    BTreeMap<BranchName, CommitId>,
    BTreeMap<BranchName, Vec<PlanCommit>>,
) {
    let mut branch_tips = BTreeMap::new();
    let mut extra_commits = BTreeMap::new();
    for (branch, branch_ref) in branch_refs {
        branch_tips.insert(branch.clone(), branch_ref.expected_tip);
        extra_commits.insert(branch, branch_ref.extra_commits);
    }

    (branch_tips, extra_commits)
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
