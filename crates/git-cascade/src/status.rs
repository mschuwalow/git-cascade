use crate::Result;
use crate::state::StateFile;
use crate::storage::Storage;

pub fn status(storage: &Storage) -> Result<String> {
    let Some(mut state_file) = StateFile::open(storage)? else {
        return Ok("No active cascade operation.\n".to_owned());
    };
    let state = state_file.read_state()?;

    let mut output = String::new();
    output.push_str("Active cascade operation:\n");
    output.push_str(&format!("phase: {}\n", state.phase));
    output.push_str(&format!("plan: {}\n", state.plan_name));
    output.push_str(&format!("plan-id: {}\n", state.plan_id));
    output.push_str(&format!(
        "new-tip: {} -> {}\n",
        state.new_tip.input, state.new_tip.resolved
    ));
    output.push_str(&format!("strategy: {}\n", state.strategy.as_str()));
    output.push_str(&format!("worktree-mode: {}\n", state.worktree));
    if let Some(current) = &state.current {
        output.push_str(&format!("current-branch: {}\n", current.branch));
        output.push_str(&format!("current-commit: {}\n", current.commit));
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
