use crate::Result;
use crate::git::Git;
use crate::replay::state::read_state;
use crate::replay::{PauseReason, PausedKind, Phase};
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
    output.push_str(&format!("strategy: {}\n", state.strategy.as_str()));
    output.push_str(&format!("replay-mode: {}\n", state.replay_mode));
    output.push_str(&format!("worktree-mode: {}\n", state.worktree));
    match &state.phase {
        Phase::ContinueReplay { replay } => {
            output.push_str(&format!("current-branch: {}\n", replay.branch));
            output.push_str("current-commit: none\n");
        }
        Phase::Conflict { current, .. } | Phase::ContinueAfterConflict { current } => {
            output.push_str(&format!("current-branch: {}\n", current.branch));
            output.push_str(&format!("current-commit: {}\n", current.commit));
        }
        _ => output.push_str("current: none\n"),
    }
    if let Phase::Paused { paused } | Phase::ContinueAfterPause { paused } = &state.phase {
        if paused.reasons().contains(&PauseReason::BranchEnd) {
            output.push_str("paused-kind: branch-end\n");
        } else if let PausedKind::MidBranch { commit } = &paused.kind {
            if paused.reasons().contains(&PauseReason::ChildBase) {
                output.push_str("paused-kind: child-base\n");
            } else {
                output.push_str("paused-kind: commit\n");
            }
            output.push_str(&format!("paused-commit: {commit}\n"));
        }
        output.push_str(&format!("paused-reasons: {}\n", paused.reason_list()));
        output.push_str(&format!("paused-branch: {}\n", paused.branch()));
        output.push_str(&format!("paused-tip: {}\n", paused.rewritten_tip()));
    }
    output.push_str(&format!("worktree: {}\n", state.worktree.path()));
    output.push_str(&format!(
        "completed-temp-refs: {}\n",
        state.completed_temp_refs.len()
    ));
    if state.pending_branches.is_empty() {
        output.push_str("pending: none\n");
    } else {
        let pending = state
            .pending_branches
            .iter()
            .map(|branch| branch.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        output.push_str(&format!("pending: {pending}\n"));
    }

    Ok(output)
}
