use super::state::{Phase, ReplayState, RestoreState, WorktreeState};
use super::state_writer::StateWriter;
use crate::git::Git;
use crate::storage::Storage;
use crate::{Error, Result};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

pub(super) fn run_deleting_phase(
    git: &Git,
    storage: &Storage,
    state_writer: &mut dyn StateWriter,
    state: &mut ReplayState,
) -> Result<()> {
    let delete_plan = deleting_delete_plan(state)?;
    delete_applied_plan(storage, state, delete_plan)?;
    cleanup_replay_artifacts(git, storage, state)?;
    state_writer.remove_state()
}

pub(super) fn dry_run_deleting_output(state: &ReplayState) -> String {
    let WorktreeState::InPlace { path, restore } = &state.worktree else {
        return String::new();
    };

    let mut output = String::new();
    writeln!(output).unwrap();
    writeln!(output, "# restore checkout").unwrap();
    match restore {
        RestoreState::Branch { name, .. } => {
            writeln!(output, "git -C {} switch {name}", path).unwrap();
        }
        RestoreState::Detached { head } => {
            writeln!(output, "git -C {} switch --detach {head}", path).unwrap();
        }
    }
    output
}

fn deleting_delete_plan(state: &ReplayState) -> Result<bool> {
    match &state.phase {
        Phase::Deleting { delete_plan } => Ok(*delete_plan),
        _ => Err(Error::InvalidInvocation(
            "active apply state is not in deleting phase".to_owned(),
        )),
    }
}

fn delete_applied_plan(storage: &Storage, state: &ReplayState, delete_plan: bool) -> Result<()> {
    if delete_plan {
        storage.delete_plan_if_exists(state.plan_name.clone())?;
    }
    Ok(())
}

fn cleanup_replay_artifacts(git: &Git, storage: &Storage, state: &ReplayState) -> Result<()> {
    eprintln!("Cleaning temporary cascade state");
    let worktree = worktree_path(storage, state);
    abort_cherry_pick_and_restore_checkout(&worktree, &state.worktree)?;
    delete_temp_refs(git, state)?;
    remove_temporary_worktree(git, state, &worktree)
}

fn abort_cherry_pick_and_restore_checkout(
    worktree: &std::path::Path,
    worktree_state: &WorktreeState,
) -> Result<()> {
    if !worktree.exists() {
        return Ok(());
    }

    let worktree_git = Git::new(worktree);
    worktree_git.try_cherry_pick_abort()?;
    if let WorktreeState::InPlace { restore, .. } = worktree_state {
        restore_checkout(&worktree_git, restore)?;
    }
    Ok(())
}

fn restore_checkout(git: &Git, restore: &RestoreState) -> Result<()> {
    match restore {
        RestoreState::Branch { name, .. } => git.switch_branch(name),
        RestoreState::Detached { head } => git.switch_detached(head),
    }
}

fn delete_temp_refs(git: &Git, state: &ReplayState) -> Result<()> {
    let mut refs = BTreeSet::new();
    refs.extend(state.completed_temp_refs.iter().cloned());
    refs.extend(git.refs_under(&format!("refs/cascade/tmp/{}", state.plan_id))?);
    for temp_ref in refs {
        git.delete_ref(&temp_ref)?;
    }
    Ok(())
}

fn remove_temporary_worktree(
    git: &Git,
    state: &ReplayState,
    worktree: &std::path::Path,
) -> Result<()> {
    if !state.worktree.is_temporary() || !worktree.exists() {
        return Ok(());
    }

    git.worktree_remove_force(worktree)?;
    if worktree.exists() {
        fs::remove_dir_all(worktree).map_err(|source| Error::IoWithPath {
            path: worktree.to_owned(),
            source,
        })?;
    }
    Ok(())
}

fn worktree_path(storage: &Storage, state: &ReplayState) -> PathBuf {
    if state.worktree.path().is_empty() {
        storage.worktrees_dir().join(state.plan_id.to_string())
    } else {
        PathBuf::from(state.worktree.path())
    }
}
