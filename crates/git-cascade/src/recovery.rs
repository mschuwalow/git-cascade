use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use crate::git::Git;
use crate::state::{ApplyState, read_state, remove_state, require_state};
use crate::storage::Storage;
use crate::{Error, Result};

pub fn status(storage: &Storage) -> Result<String> {
    let Some(state) = read_state(storage)? else {
        return Ok("No active cascade operation.\n".to_owned());
    };

    let mut output = String::new();
    output.push_str("Active cascade operation:\n");
    output.push_str(&format!("operation: {}\n", state.operation));
    output.push_str(&format!("phase: {}\n", state.phase));
    if let Some(plan_name) = &state.plan_name {
        output.push_str(&format!("plan: {plan_name}\n"));
    }
    output.push_str(&format!("plan-id: {}\n", state.plan_id));
    output.push_str(&format!(
        "new-anchor: {} -> {}\n",
        state.new_anchor.input, state.new_anchor.resolved
    ));
    output.push_str(&format!(
        "strategy: {}\n",
        if state.strategy.move_to_heads {
            "move-to-heads"
        } else {
            "preserve-fork-points"
        }
    ));
    if let Some(current) = &state.current {
        output.push_str(&format!("current-branch: {}\n", current.branch));
        output.push_str(&format!("current-commit: {}\n", current.commit));
    } else {
        output.push_str("current: none\n");
    }
    output.push_str(&format!("worktree: {}\n", state.worktree));
    output.push_str(&format!(
        "completed-temp-refs: {}\n",
        state.completed.temp_refs.len()
    ));
    if state.pending.branches.is_empty() {
        output.push_str("pending: none\n");
    } else {
        output.push_str(&format!("pending: {}\n", state.pending.branches.join(", ")));
    }

    Ok(output)
}

pub fn abort(git: &Git, storage: &Storage) -> Result<()> {
    let state = require_state(storage)?;
    if state.operation != "apply" {
        return Err(Error::InvalidInvocation(format!(
            "cannot abort unsupported operation `{}`",
            state.operation
        )));
    }

    let worktree = worktree_path(storage, &state);
    if worktree.exists() {
        Git::new(&worktree).try_cherry_pick_abort()?;
    }

    let mut refs = BTreeSet::new();
    refs.extend(state.completed.temp_refs.iter().cloned());
    refs.extend(git.refs_under(&format!("refs/cascade/tmp/{}", state.plan_id))?);
    for temp_ref in refs {
        let _ = git.delete_ref(&temp_ref);
    }

    if worktree.exists() {
        let _ = git.worktree_remove_force(&worktree);
        if worktree.exists() {
            fs::remove_dir_all(&worktree).map_err(|source| Error::IoWithPath {
                path: worktree.clone(),
                source,
            })?;
        }
    }

    remove_state(storage)
}

fn worktree_path(storage: &Storage, state: &ApplyState) -> PathBuf {
    if state.worktree.is_empty() {
        storage.worktrees_dir().join(&state.plan_id)
    } else {
        PathBuf::from(&state.worktree)
    }
}
