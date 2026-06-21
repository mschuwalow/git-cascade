use crate::git::Git;
mod context;
pub mod state;

use crate::plan::{
    BranchRef, Plan, PlanCommit, PlanName, branches_in_topological_order, validate_branch_refs,
    validate_merge_parents_for_apply, validate_plan,
};
use crate::replay_backend::{DryRunReplayBackend, GitReplayBackend, ReplayBackend};
use crate::state_writer::{LockedStateWriter, NoopStateWriter, StateWriter};
use crate::storage::Storage;
use crate::{Error, Result};
use context::ReplayContext;
pub use state::{
    CurrentState, PausedState, Phase, ReplayMode, ReplayState, RestoreState, Strategy,
    WorktreeState,
};
use state::{InitialReplayStateInput, StateFile, initial_replay_state};
use std::collections::BTreeMap;
use std::fs;

#[derive(Debug, Clone)]
pub struct ReplayOptions {
    pub plan_name: PlanName,
    pub new_tip_input: String,
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
            match replay.run()? {
                ReplayOutcome::Complete => break,
                ReplayOutcome::Paused { .. } => replay.continue_after_pause(),
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
    let mut backend = GitReplayBackend::new(git, storage);
    ReplayContext::new(plan, &mut state_writer, &mut backend, state)?.run()
}

pub fn continue_apply(git: &Git, storage: &Storage) -> Result<ReplayOutcome> {
    let mut state_file = StateFile::open(storage)?
        .ok_or_else(|| Error::InvalidInvocation("no active cascade operation".to_owned()))?;
    let mut state = state_file.read_state()?;

    let mut state_writer = LockedStateWriter::new(state_file);
    let mut backend = GitReplayBackend::new(git, storage);
    if matches!(state.phase, Phase::Deleting { .. }) {
        run_deleting_state(&mut state_writer, &mut backend, &mut state)?;
        Ok(ReplayOutcome::Complete)
    } else {
        let plan_name = state.plan_name.clone();
        let plan = Plan::from_yaml(&storage.read_plan(&plan_name)?)?;
        // Branch refs are not re-checked here: they may legitimately already
        // point at rewritten tips when resuming a final update.
        validate_plan(git, &plan)?;
        let mut context = ReplayContext::new(&plan, &mut state_writer, &mut backend, state)?;
        context.continue_after_pause();
        context.run()
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

fn replay_mode(options: &ReplayOptions) -> ReplayMode {
    if options.pause_at_checkpoints {
        ReplayMode::PauseAtCheckpoints
    } else {
        ReplayMode::RunToCompletion
    }
}

fn run_deleting_state(
    state_writer: &mut dyn StateWriter,
    backend: &mut dyn ReplayBackend,
    state: &mut ReplayState,
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
