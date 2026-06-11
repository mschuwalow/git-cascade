use crate::git::Git;
use crate::plan::{
    BranchRef, Node, Plan, PlanCommit, PlanName, branches_in_topological_order,
    validate_branch_refs, validate_merge_parents_for_apply, validate_plan,
};
use crate::replay_backend::{DryRunReplayBackend, GitReplayBackend, ReplayBackend};
use crate::state::{
    ApplyState, ApplyStateInput, CurrentState, Phase, RestoreState, StateFile, Strategy,
    WorktreeState, initial_apply_state,
};
use crate::state_writer::{LockedStateWriter, NoopStateWriter, StateWriter};
use crate::storage::Storage;
use crate::test_hooks;
use crate::{Error, Result};
use std::collections::{BTreeMap, HashMap};
use std::fs;

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub plan_name: PlanName,
    pub new_tip_input: String,
    pub strategy: Strategy,
    pub in_place: bool,
}

struct ActualReplayContext<'a> {
    nodes: &'a HashMap<&'a str, &'a Node>,
    selected_bases: &'a HashMap<String, String>,
    temp_tips: &'a HashMap<String, String>,
    mappings: &'a BTreeMap<String, String>,
    new_tip: &'a str,
    strategy: Strategy,
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
    let mut state = initial_apply_state(ApplyStateInput {
        plan_name: &options.plan_name,
        plan_id: &plan.plan_id,
        new_tip: &new_tip,
        strategy: options.strategy,
        pending_branches: ordered,
        branch_tips,
        extra_commits,
        mappings,
        worktree: worktree_state,
    })?;
    let mut state_writer = NoopStateWriter;
    let mut backend = DryRunReplayBackend::new(git, storage, plan, &state)?;
    run_apply_state(plan, &mut state_writer, &mut backend, &mut state)?;

    Ok(backend.finish())
}

pub fn execute(git: &Git, storage: &Storage, plan: &Plan, options: ApplyOptions) -> Result<()> {
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
    let state = initial_apply_state(ApplyStateInput {
        plan_name: &options.plan_name,
        plan_id: &plan.plan_id,
        new_tip: &new_tip,
        strategy: options.strategy,
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
    let mut state = state;
    let mut state_writer = LockedStateWriter::new(state_file);
    let mut backend = GitReplayBackend::new(git, storage);
    run_apply_state(plan, &mut state_writer, &mut backend, &mut state)
}

pub fn continue_apply(git: &Git, storage: &Storage) -> Result<()> {
    let mut state_file = StateFile::open(storage)?
        .ok_or_else(|| Error::InvalidInvocation("no active cascade operation".to_owned()))?;
    let mut state = state_file.read_state()?;

    let mut state_writer = LockedStateWriter::new(state_file);
    let mut backend = GitReplayBackend::new(git, storage);
    if state.phase == Phase::Deleting {
        run_deleting_state(&mut state_writer, &mut backend, &mut state)
    } else {
        let plan_name = state.plan_name.clone();
        let plan = Plan::from_yaml(&storage.read_plan(&plan_name)?)?;
        // Branch refs are not re-checked here: they may legitimately already
        // point at rewritten tips when resuming a final update.
        validate_plan(git, &plan)?;
        run_apply_state(&plan, &mut state_writer, &mut backend, &mut state)
    }
}

pub fn abort(git: &Git, storage: &Storage) -> Result<()> {
    let Some(mut state_file) = StateFile::open(storage)? else {
        return Err(Error::InvalidInvocation(
            "no active cascade operation".to_owned(),
        ));
    };
    let mut state = state_file.read_state()?;

    if state.phase != Phase::Deleting {
        state.phase = Phase::Deleting;
        state.cleanup.delete_plan = false;
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

fn run_apply_state(
    plan: &Plan,
    state_writer: &mut dyn StateWriter,
    backend: &mut dyn ReplayBackend,
    state: &mut ApplyState,
) -> Result<()> {
    backend.start(state)?;
    loop {
        match state.phase {
            Phase::Replay => {
                replay_pending_branches(plan, state_writer, backend, state)?;
                state.phase = Phase::FinalUpdate;
                state_writer.write_state(state)?;
            }
            Phase::FinalUpdate => {
                backend.final_update(plan, state)?;
                test_hooks::run("after-final-update")?;
                state.phase = Phase::Deleting;
                state.cleanup.delete_plan = true;
                state_writer.write_state(state)?;
                test_hooks::run("after-deleting-state-written")?;
            }
            Phase::Conflict => {
                resolve_conflict(backend, state)?;
                state.phase = Phase::Replay;
                state_writer.write_state(state)?;
            }
            Phase::Deleting => {
                return run_deleting_state(state_writer, backend, state);
            }
        }
    }
}

fn run_deleting_state(
    state_writer: &mut dyn StateWriter,
    backend: &mut dyn ReplayBackend,
    state: &mut ApplyState,
) -> Result<()> {
    if state.cleanup.delete_plan {
        backend.delete_applied_plan(state)?;
    }
    backend.cleanup_deleting_state(state)?;
    state_writer.remove_state()
}

fn resolve_conflict(backend: &mut dyn ReplayBackend, state: &mut ApplyState) -> Result<()> {
    let current = state.current.clone().ok_or_else(|| {
        Error::InvalidInvocation("active apply state has no current commit".to_owned())
    })?;
    let rewritten_commit = backend.continue_cherry_pick(state, &current)?;
    state.mappings.insert(current.commit, rewritten_commit);
    Ok(())
}

fn replay_pending_branches(
    plan: &Plan,
    state_writer: &mut dyn StateWriter,
    backend: &mut dyn ReplayBackend,
    state: &mut ApplyState,
) -> Result<()> {
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut mappings = state.mappings.clone();
    let mut temp_refs = state.completed.temp_refs.clone();
    let mut temp_tips = backend.temp_tips(&temp_refs)?;
    let mut selected_bases = selected_bases_from_mappings(plan, &mappings);
    let total_branches = plan.nodes.len();

    if total_branches == 0 {
        backend.no_branches()?;
    }

    while let Some(branch) = state.pending.branches.first().cloned() {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown pending branch `{branch}`")))?;
        let branch_index = temp_refs.len() + 1;
        let commits = replay_commits_from_extra(node, &state.extra_commits);
        let was_resuming = state.current.is_some();

        let (start_commit_index, mut last_rewritten) = if was_resuming {
            let start = resume_start_commit_index(node, state, &mappings, &commits)?;
            let head = commits
                .get(start.wrapping_sub(1))
                .and_then(|commit| mappings.get(&commit.oid))
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidPlan(format!(
                        "branch `{}` has no rewritten commit to resume from",
                        node.branch
                    ))
                })?;
            (start, head)
        } else {
            let base = actual_replay_base(
                node,
                ActualReplayContext {
                    nodes: &nodes,
                    selected_bases: &selected_bases,
                    temp_tips: &temp_tips,
                    mappings: &mappings,
                    new_tip: &state.new_tip,
                    strategy: state.strategy,
                },
            )?;
            selected_bases.insert(node.branch.clone(), base.clone());
            mappings.insert(node.base().to_owned(), base.clone());

            if base == node.base() {
                // Already at its replay base; keep the existing commits.
                for commit in &commits {
                    mappings.insert(commit.oid.clone(), commit.oid.clone());
                }
                let current_tip =
                    commits
                        .last()
                        .map(|commit| commit.oid.clone())
                        .ok_or_else(|| {
                            Error::InvalidPlan(format!("branch `{}` has no commits", node.branch))
                        })?;
                let (temp_ref, branch_tip) =
                    backend.skip_replay(plan, node, branch_index, total_branches, &current_tip)?;
                temp_tips.insert(node.branch.clone(), branch_tip);
                if !temp_refs.contains(&temp_ref) {
                    temp_refs.push(temp_ref);
                }
                remove_pending_branch(state, &branch)?;
                checkpoint_completed_branch(state, state_writer, &mappings, &temp_refs)?;
                continue;
            }

            backend.prepare_branch(state, branch_index, total_branches, node, &base)?;
            (0, base)
        };

        backend.start_replay(
            branch_index,
            total_branches,
            node,
            commits.len(),
            start_commit_index,
            was_resuming,
        )?;
        for (commit_index, commit) in commits.iter().enumerate().skip(start_commit_index) {
            if commit.is_merge() {
                // The merged history is contained in the new base; flatten.
                backend.flatten_merge(node, &commit.oid, commit_index, commits.len())?;
                mappings.insert(commit.oid.clone(), last_rewritten.clone());
                continue;
            }

            let rewritten_commit =
                match backend.cherry_pick(state, node, &commit.oid, commit_index, commits.len()) {
                    Ok(rewritten_commit) => rewritten_commit,
                    Err(error) => {
                        state.current = Some(CurrentState {
                            branch: node.branch.clone(),
                            commit: commit.oid.clone(),
                            worktree: state.worktree.path().to_owned(),
                        });
                        state.mappings = mappings.clone();
                        state.completed.temp_refs = temp_refs.clone();
                        state.phase = Phase::Conflict;
                        state_writer.write_state(state)?;
                        return Err(error);
                    }
                };
            mappings.insert(commit.oid.clone(), rewritten_commit.clone());
            last_rewritten = rewritten_commit;
        }

        let rewritten_tip = if let Some(commit) = commits.last() {
            mappings.get(&commit.oid).cloned().ok_or_else(|| {
                Error::InvalidPlan(format!(
                    "commit `{}` for branch `{}` has no rewritten mapping",
                    commit.oid, node.branch
                ))
            })?
        } else {
            selected_bases.get(&node.branch).cloned().ok_or_else(|| {
                Error::InvalidPlan(format!("branch `{}` has no selected base", node.branch))
            })?
        };
        let (temp_ref, branch_tip) =
            backend.write_temp_ref(plan, node, branch_index, total_branches, &rewritten_tip)?;
        temp_tips.insert(node.branch.clone(), branch_tip);
        if !temp_refs.contains(&temp_ref) {
            temp_refs.push(temp_ref);
        }
        remove_pending_branch(state, &branch)?;
        checkpoint_completed_branch(state, state_writer, &mappings, &temp_refs)?;
    }

    state.current = None;
    state.mappings = mappings;
    state.completed.temp_refs = temp_refs;

    Ok(())
}

/// Persists progress after a branch finished so a crashed apply can resume
/// from the next pending branch.
fn checkpoint_completed_branch(
    state: &mut ApplyState,
    state_writer: &mut dyn StateWriter,
    mappings: &BTreeMap<String, String>,
    temp_refs: &[String],
) -> Result<()> {
    state.current = None;
    state.mappings = mappings.clone();
    state.completed.temp_refs = temp_refs.to_vec();
    state_writer.write_state(state)
}

fn resume_start_commit_index(
    node: &Node,
    state: &mut ApplyState,
    mappings: &BTreeMap<String, String>,
    commits: &[PlanCommit],
) -> Result<usize> {
    let current = state.current.as_ref().expect("resume requires current");
    if current.branch != node.branch {
        return Err(Error::InvalidPlan(format!(
            "current branch `{}` is not the next pending branch `{}`",
            current.branch, node.branch
        )));
    }
    if !mappings.contains_key(&current.commit) {
        return Err(Error::InvalidPlan(format!(
            "current commit `{}` for branch `{}` has no rewritten mapping",
            current.commit, current.branch
        )));
    }

    let start_commit_index = commits
        .iter()
        .position(|commit| commit.oid == current.commit)
        .map(|index| index + 1)
        .ok_or_else(|| {
            Error::InvalidPlan(format!(
                "current commit `{}` is not part of branch `{}`",
                current.commit, current.branch
            ))
        })?;
    state.current = None;
    Ok(start_commit_index)
}

fn remove_pending_branch(state: &mut ApplyState, branch: &str) -> Result<()> {
    if state.pending.branches.first().map(String::as_str) != Some(branch) {
        return Err(Error::InvalidPlan(format!(
            "completed branch `{branch}` is not first in pending state"
        )));
    }
    state.pending.branches.remove(0);
    Ok(())
}

fn actual_replay_base(node: &Node, context: ActualReplayContext<'_>) -> Result<String> {
    if node.is_root() {
        return Ok(context.new_tip.to_owned());
    }

    let parent_branch = node.parent().ok_or_else(|| {
        Error::InvalidPlan(format!("root node `{}` has no branch parent", node.branch))
    })?;
    let parent = context
        .nodes
        .get(parent_branch)
        .ok_or_else(|| Error::InvalidPlan(format!("unknown parent `{parent_branch}`")))?;

    if context.strategy == Strategy::MoveToPlannedTips {
        return context.mappings.get(&parent.tip).cloned().ok_or_else(|| {
            Error::InvalidPlan(format!(
                "parent `{}` has no rewritten planned tip",
                parent.branch
            ))
        });
    }

    if context.strategy == Strategy::MoveToCurrentTips {
        return context
            .temp_tips
            .get(&parent.branch)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no rewritten tip", parent.branch))
            });
    }

    let base = node.base();
    if base == parent.base() {
        return context
            .selected_bases
            .get(&parent.branch)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no selected base", parent.branch))
            });
    }

    context.mappings.get(base).cloned().ok_or_else(|| {
        Error::InvalidPlan(format!(
            "base `{}` for branch `{}` was not mapped",
            base, node.branch
        ))
    })
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
