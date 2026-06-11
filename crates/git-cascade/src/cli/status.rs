use crate::Result;
use crate::git::Git;
use crate::state::read_state;
use crate::storage::Storage;

pub(super) fn status() -> Result<()> {
    let git = Git::current_dir()?;
    let storage = Storage::discover(&git)?;
    print!("{}", status_output(&storage)?);

    Ok(())
}

fn status_output(storage: &Storage) -> Result<String> {
    let Some(state) = read_state(storage)? else {
        return Ok("No active cascade operation.\n".to_owned());
    };

    let mut output = String::new();
    output.push_str("Active cascade operation:\n");
    output.push_str(&format!("phase: {}\n", state.phase));
    output.push_str(&format!("plan: {}\n", state.plan_name));
    output.push_str(&format!("plan-id: {}\n", state.plan_id));
    output.push_str(&format!("new-tip: {}\n", state.new_tip));
    output.push_str(&format!(
        "base-strategy: {}\n",
        state.base_strategy.as_str()
    ));
    output.push_str(&format!(
        "merge-strategy: {}\n",
        state.merge_strategy.as_str()
    ));
    output.push_str(&format!("worktree-mode: {}\n", state.worktree));
    if let Some(current) = &state.current {
        output.push_str(&format!("current-branch: {}\n", current.branch));
        output.push_str(&format!("current-commit: {}\n", current.commit));
        output.push_str(&format!("current-op: {}\n", current.op));
    } else {
        output.push_str("current: none\n");
    }
    output.push_str(&format!("worktree: {}\n", state.worktree.path()));
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
