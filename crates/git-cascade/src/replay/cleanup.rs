use super::state::{Phase, ReplayState};
use super::state_writer::StateWriter;
use crate::git::Git;
use crate::storage::Storage;
use crate::{Error, Result};
use std::collections::BTreeSet;
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
    delete_temp_refs(git, state)?;
    remove_temporary_worktree(git, state, &worktree)
}

fn delete_temp_refs(git: &Git, state: &ReplayState) -> Result<()> {
    let mut refs = BTreeSet::new();
    refs.extend(
        state
            .completed_temp_refs
            .iter()
            .map(|temp_ref| temp_ref.as_str().to_owned()),
    );
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

    let _ = git.worktree_remove_force(worktree);
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
