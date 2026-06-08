use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;

use crate::encoding::{decode_component, encode_component};
use crate::git::Git;
use crate::plan::{Node, Plan};
use crate::plan_validate::{topological_order, validate_plan_for_apply};
use crate::state::{
    ApplyState, ApplyStateInput, CurrentState, StateLock, initial_apply_state, read_state,
    remove_state, write_state_atomic,
};
use crate::storage::{PlanName, Storage};
use crate::test_hooks;
use crate::{Error, Result};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct DryRunOptions {
    pub new_anchor_input: String,
    pub move_to_heads: bool,
}

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub plan_name: PlanName,
    pub new_anchor_input: String,
    pub move_to_heads: bool,
}

#[derive(Debug, Clone)]
enum ReplayBase {
    ResolvedCommit(String),
    RewrittenCommit { branch: String, old_commit: String },
    RewrittenTip { branch: String },
}

struct ActualReplayContext<'a> {
    anchor: &'a Node,
    nodes: &'a HashMap<&'a str, &'a Node>,
    selected_bases: &'a HashMap<String, String>,
    temp_tips: &'a HashMap<String, String>,
    mappings: &'a BTreeMap<String, String>,
    new_anchor: &'a str,
    move_to_heads: bool,
}

impl ReplayBase {
    fn display(&self) -> String {
        match self {
            Self::ResolvedCommit(commit) => commit.clone(),
            Self::RewrittenCommit { branch, old_commit } => {
                format!("<rewritten {branch}:{old_commit}>")
            }
            Self::RewrittenTip { branch } => format!("<rewritten {branch} tip>"),
        }
    }
}

pub fn dry_run(
    git: &Git,
    storage: &Storage,
    plan: &Plan,
    options: DryRunOptions,
) -> Result<String> {
    validate_plan_for_apply(git, plan)?;
    let new_anchor = git.resolve_commit(&options.new_anchor_input)?;
    let ordered = topological_order(plan)?;
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let anchor = plan
        .nodes
        .iter()
        .find(|node| node.parent.is_none())
        .ok_or_else(|| {
            Error::InvalidPlan("plan must contain exactly one anchor node".to_owned())
        })?;

    let mut selected_bases = HashMap::<String, ReplayBase>::new();
    let mut output = String::new();
    let worktree = storage.worktrees_dir().join("<generated-uuid>");
    let strategy = if options.move_to_heads {
        "move-to-heads"
    } else {
        "preserve-fork-points"
    };

    writeln!(output, "# git-cascade apply --dry-run").unwrap();
    writeln!(
        output,
        "new-anchor {} -> {}",
        options.new_anchor_input, new_anchor
    )
    .unwrap();
    writeln!(output, "strategy {strategy}").unwrap();
    writeln!(output, "worktree {}", worktree.display()).unwrap();
    writeln!(output, "temp-refs refs/cascade/tmp/{}", plan.plan_id).unwrap();

    for (index, branch) in ordered.iter().enumerate() {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;
        let base = replay_base(
            node,
            anchor,
            &nodes,
            &selected_bases,
            &new_anchor,
            options.move_to_heads,
        )?;
        selected_bases.insert(node.branch.clone(), base.clone());

        writeln!(output).unwrap();
        writeln!(output, "# branch {}", node.branch).unwrap();
        writeln!(output, "replay-base {}", base.display()).unwrap();
        if index == 0 {
            writeln!(
                output,
                "git worktree add --detach {} {}",
                worktree.display(),
                base.display()
            )
            .unwrap();
        } else {
            writeln!(
                output,
                "git -C {} reset --hard {}",
                worktree.display(),
                base.display()
            )
            .unwrap();
        }
        for commit in &node.commits {
            writeln!(output, "git -C {} cherry-pick {commit}", worktree.display()).unwrap();
        }
        writeln!(
            output,
            "git update-ref refs/cascade/tmp/{}/{} HEAD",
            plan.plan_id,
            encode_component(&node.branch)
        )
        .unwrap();
    }

    writeln!(output).unwrap();
    writeln!(output, "# final ref transaction").unwrap();
    writeln!(output, "git update-ref --stdin <<'EOF'").unwrap();
    writeln!(output, "start").unwrap();
    for branch in &ordered {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;
        writeln!(
            output,
            "update refs/heads/{} <rewritten {} tip> {}",
            node.branch, node.branch, node.old_tip
        )
        .unwrap();
    }
    writeln!(output, "prepare").unwrap();
    writeln!(output, "commit").unwrap();
    writeln!(output, "EOF").unwrap();

    Ok(output)
}

pub fn execute(git: &Git, storage: &Storage, plan: &Plan, options: ApplyOptions) -> Result<()> {
    validate_plan_for_apply(git, plan)?;
    let new_anchor = git.resolve_commit(&options.new_anchor_input)?;
    let new_anchor_ref = git.symbolic_full_name(&options.new_anchor_input)?;
    let ordered = topological_order(plan)?;
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let anchor = plan
        .nodes
        .iter()
        .find(|node| node.parent.is_none())
        .ok_or_else(|| {
            Error::InvalidPlan("plan must contain exactly one anchor node".to_owned())
        })?;

    let mut mappings = BTreeMap::new();
    mappings.insert(plan.source.anchor_old_tip.clone(), new_anchor.clone());
    let worktree = storage.worktrees_dir().join(Uuid::new_v4().to_string());
    let mut state = initial_apply_state(ApplyStateInput {
        plan_name: &options.plan_name,
        plan_id: &plan.plan_id,
        new_anchor_input: &options.new_anchor_input,
        new_anchor_resolved: &new_anchor,
        new_anchor_input_was_ref: new_anchor_ref.is_some(),
        move_to_heads: options.move_to_heads,
        pending_branches: ordered.clone(),
        mappings: mappings.clone(),
        worktree: worktree.display().to_string(),
    })?;
    let state_lock = StateLock::create(storage, &state)?;

    storage.ensure_worktrees_dir()?;
    cleanup_stale_worktree(git, &worktree)?;

    let worktree_git = Git::new(&worktree);
    let mut selected_bases = HashMap::<String, String>::new();
    let mut temp_tips = HashMap::<String, String>::new();
    let mut temp_refs = Vec::<String>::new();
    let mut worktree_created = false;

    for (index, branch) in ordered.iter().enumerate() {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;
        let base = actual_replay_base(
            node,
            ActualReplayContext {
                anchor,
                nodes: &nodes,
                selected_bases: &selected_bases,
                temp_tips: &temp_tips,
                mappings: &mappings,
                new_anchor: &new_anchor,
                move_to_heads: options.move_to_heads,
            },
        )?;
        selected_bases.insert(node.branch.clone(), base.clone());
        mappings.insert(node.old_base.clone(), base.clone());
        state.mappings = mappings.clone();
        state.pending.branches = ordered[index..].to_vec();
        write_state_atomic(storage, &mut state)?;

        if index == 0 {
            git.worktree_add_detached(&worktree, &base)?;
            worktree_created = true;
        } else {
            worktree_git.reset_hard(&base)?;
        }

        for commit in &node.commits {
            state.phase = "replay".to_owned();
            state.current = Some(CurrentState {
                branch: node.branch.clone(),
                commit: commit.clone(),
                worktree: worktree.display().to_string(),
            });
            write_state_atomic(storage, &mut state)?;

            if let Err(error) = worktree_git.cherry_pick(commit) {
                state.phase = "conflict".to_owned();
                write_state_atomic(storage, &mut state)?;
                return Err(Error::ApplyStopped {
                    branch: node.branch.clone(),
                    commit: commit.clone(),
                    worktree: worktree.clone(),
                    message: error.to_string(),
                });
            }
            mappings.insert(commit.clone(), worktree_git.head_oid()?);
            state.mappings = mappings.clone();
            write_state_atomic(storage, &mut state)?;
        }

        let rewritten_tip = worktree_git.head_oid()?;
        let temp_ref = temp_ref(plan, &node.branch);
        git.update_ref(&temp_ref, &rewritten_tip)?;
        temp_tips.insert(node.branch.clone(), rewritten_tip);
        temp_refs.push(temp_ref);
        state.completed.temp_refs = temp_refs.clone();
        state.current = None;
        state.pending.branches = ordered[index + 1..].to_vec();
        write_state_atomic(storage, &mut state)?;
    }

    state.phase = "final_update".to_owned();
    write_state_atomic(storage, &mut state)?;
    test_hooks::run("before-final-update")?;
    git.update_ref_transaction(&final_ref_transaction(
        &ordered,
        &nodes,
        &temp_tips,
        new_anchor_ref.as_deref(),
        &new_anchor,
    )?)?;

    for temp_ref in &temp_refs {
        git.delete_ref(temp_ref)?;
    }
    if worktree_created {
        git.worktree_remove_force(&worktree)?;
    }
    state_lock.remove()?;

    Ok(())
}

pub fn continue_apply(git: &Git, storage: &Storage) -> Result<()> {
    let mut state = read_state(storage)?
        .ok_or_else(|| Error::InvalidInvocation("no active cascade operation".to_owned()))?;
    if state.operation != "apply" {
        return Err(Error::InvalidInvocation(format!(
            "cannot continue unsupported operation `{}`",
            state.operation
        )));
    }
    if state.phase != "conflict" {
        return Err(Error::InvalidInvocation(format!(
            "cannot continue cascade operation in phase `{}`",
            state.phase
        )));
    }

    let current = state.current.clone().ok_or_else(|| {
        Error::InvalidInvocation("active apply state has no current commit".to_owned())
    })?;
    let plan_name = PlanName::new(state.plan_name.clone().ok_or_else(|| {
        Error::InvalidInvocation("active apply state does not record a named plan".to_owned())
    })?)?;
    let plan: Plan = serde_yaml::from_str(&storage.read_named_plan(&plan_name)?)?;
    validate_plan_for_apply(git, &plan)?;

    if state.new_anchor.input_was_ref {
        let resolved = git.resolve_commit(&state.new_anchor.input)?;
        if resolved != state.new_anchor.resolved {
            return Err(Error::InvalidInvocation(format!(
                "new anchor `{}` moved after apply started: expected `{}`, found `{resolved}`",
                state.new_anchor.input, state.new_anchor.resolved
            )));
        }
    }

    let worktree = std::path::PathBuf::from(&current.worktree);
    let worktree_git = Git::new(&worktree);
    if !worktree_git.unmerged_entries()?.is_empty() {
        return Err(Error::InvalidInvocation(format!(
            "worktree {} still has unresolved conflicts; resolve them and git add the files before continuing",
            worktree.display()
        )));
    }

    worktree_git.cherry_pick_continue()?;
    state
        .mappings
        .insert(current.commit.clone(), worktree_git.head_oid()?);
    state.phase = "replay".to_owned();
    write_state_atomic(storage, &mut state)?;

    continue_replay_after_resolved_commit(
        git,
        storage,
        &plan,
        state,
        &current.branch,
        &current.commit,
    )
}

fn continue_replay_after_resolved_commit(
    git: &Git,
    storage: &Storage,
    plan: &Plan,
    mut state: ApplyState,
    current_branch: &str,
    resolved_commit: &str,
) -> Result<()> {
    let ordered = topological_order(plan)?;
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let anchor = plan
        .nodes
        .iter()
        .find(|node| node.parent.is_none())
        .ok_or_else(|| {
            Error::InvalidPlan("plan must contain exactly one anchor node".to_owned())
        })?;
    let worktree = std::path::PathBuf::from(&state.worktree);
    let worktree_git = Git::new(&worktree);
    let mut mappings = state.mappings.clone();
    let mut temp_refs = state.completed.temp_refs.clone();
    let mut temp_tips = temp_tips_from_refs(git, &temp_refs)?;
    let mut selected_bases = selected_bases_from_mappings(plan, &mappings);
    let current_index = ordered
        .iter()
        .position(|branch| branch == current_branch)
        .ok_or_else(|| {
            Error::InvalidPlan(format!("current branch `{current_branch}` is not in plan"))
        })?;

    for (index, branch) in ordered.iter().enumerate().skip(current_index) {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;

        let start_commit_index = if branch == current_branch {
            node.commits
                .iter()
                .position(|commit| commit == resolved_commit)
                .ok_or_else(|| {
                    Error::InvalidPlan(format!(
                        "current commit `{resolved_commit}` is not part of branch `{current_branch}`"
                    ))
                })?
                + 1
        } else {
            let base = actual_replay_base(
                node,
                ActualReplayContext {
                    anchor,
                    nodes: &nodes,
                    selected_bases: &selected_bases,
                    temp_tips: &temp_tips,
                    mappings: &mappings,
                    new_anchor: &state.new_anchor.resolved,
                    move_to_heads: state.strategy.move_to_heads,
                },
            )?;
            selected_bases.insert(node.branch.clone(), base.clone());
            mappings.insert(node.old_base.clone(), base.clone());
            state.mappings = mappings.clone();
            state.pending.branches = ordered[index..].to_vec();
            write_state_atomic(storage, &mut state)?;
            worktree_git.reset_hard(&base)?;
            0
        };

        for commit in node.commits.iter().skip(start_commit_index) {
            state.phase = "replay".to_owned();
            state.current = Some(CurrentState {
                branch: node.branch.clone(),
                commit: commit.clone(),
                worktree: worktree.display().to_string(),
            });
            write_state_atomic(storage, &mut state)?;

            if let Err(error) = worktree_git.cherry_pick(commit) {
                state.phase = "conflict".to_owned();
                write_state_atomic(storage, &mut state)?;
                return Err(Error::ApplyStopped {
                    branch: node.branch.clone(),
                    commit: commit.clone(),
                    worktree: worktree.clone(),
                    message: error.to_string(),
                });
            }
            mappings.insert(commit.clone(), worktree_git.head_oid()?);
            state.mappings = mappings.clone();
            write_state_atomic(storage, &mut state)?;
        }

        let rewritten_tip = worktree_git.head_oid()?;
        let temp_ref = temp_ref(plan, &node.branch);
        git.update_ref(&temp_ref, &rewritten_tip)?;
        temp_tips.insert(node.branch.clone(), rewritten_tip);
        if !temp_refs.contains(&temp_ref) {
            temp_refs.push(temp_ref);
        }
        state.completed.temp_refs = temp_refs.clone();
        state.current = None;
        state.pending.branches = ordered[index + 1..].to_vec();
        write_state_atomic(storage, &mut state)?;
    }

    state.phase = "final_update".to_owned();
    write_state_atomic(storage, &mut state)?;
    let new_anchor_ref = if state.new_anchor.input_was_ref {
        Some(
            git.symbolic_full_name(&state.new_anchor.input)?
                .ok_or_else(|| {
                    Error::InvalidInvocation(format!(
                        "new anchor `{}` no longer resolves to a ref",
                        state.new_anchor.input
                    ))
                })?,
        )
    } else {
        None
    };
    test_hooks::run("before-final-update")?;
    git.update_ref_transaction(&final_ref_transaction(
        &ordered,
        &nodes,
        &temp_tips,
        new_anchor_ref.as_deref(),
        &state.new_anchor.resolved,
    )?)?;

    for temp_ref in &temp_refs {
        git.delete_ref(temp_ref)?;
    }
    git.worktree_remove_force(&worktree)?;
    remove_state(storage)?;

    Ok(())
}

fn replay_base(
    node: &Node,
    anchor: &Node,
    nodes: &HashMap<&str, &Node>,
    selected_bases: &HashMap<String, ReplayBase>,
    new_anchor: &str,
    move_to_heads: bool,
) -> Result<ReplayBase> {
    let parent_branch = node.parent.as_deref().ok_or_else(|| {
        Error::InvalidPlan(format!("anchor node `{}` cannot be replayed", node.branch))
    })?;
    let parent = nodes
        .get(parent_branch)
        .ok_or_else(|| Error::InvalidPlan(format!("unknown parent `{parent_branch}`")))?;

    if parent.branch == anchor.branch {
        return Ok(ReplayBase::ResolvedCommit(new_anchor.to_owned()));
    }

    if move_to_heads {
        return Ok(ReplayBase::RewrittenTip {
            branch: parent.branch.clone(),
        });
    }

    if node.old_base == parent.old_base {
        return selected_bases.get(&parent.branch).cloned().ok_or_else(|| {
            Error::InvalidPlan(format!(
                "parent `{}` has no selected replay base",
                parent.branch
            ))
        });
    }

    Ok(ReplayBase::RewrittenCommit {
        branch: parent.branch.clone(),
        old_commit: node.old_base.clone(),
    })
}

fn actual_replay_base(node: &Node, context: ActualReplayContext<'_>) -> Result<String> {
    let parent_branch = node.parent.as_deref().ok_or_else(|| {
        Error::InvalidPlan(format!("anchor node `{}` cannot be replayed", node.branch))
    })?;
    let parent = context
        .nodes
        .get(parent_branch)
        .ok_or_else(|| Error::InvalidPlan(format!("unknown parent `{parent_branch}`")))?;

    if parent.branch == context.anchor.branch {
        return Ok(context.new_anchor.to_owned());
    }

    if context.move_to_heads {
        return context
            .temp_tips
            .get(&parent.branch)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no rewritten tip", parent.branch))
            });
    }

    if node.old_base == parent.old_base {
        return context
            .selected_bases
            .get(&parent.branch)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no selected base", parent.branch))
            });
    }

    context
        .mappings
        .get(&node.old_base)
        .cloned()
        .ok_or_else(|| {
            Error::InvalidPlan(format!(
                "old_base `{}` for branch `{}` was not mapped",
                node.old_base, node.branch
            ))
        })
}

fn temp_ref(plan: &Plan, branch: &str) -> String {
    format!(
        "refs/cascade/tmp/{}/{}",
        plan.plan_id,
        encode_component(branch)
    )
}

fn temp_tips_from_refs(git: &Git, temp_refs: &[String]) -> Result<HashMap<String, String>> {
    let mut temp_tips = HashMap::new();
    for temp_ref in temp_refs {
        let Some(encoded_branch) = temp_ref.rsplit('/').next() else {
            continue;
        };
        let branch = decode_component(encoded_branch)?;
        temp_tips.insert(branch, git.resolve_commit(temp_ref)?);
    }

    Ok(temp_tips)
}

fn selected_bases_from_mappings(
    plan: &Plan,
    mappings: &BTreeMap<String, String>,
) -> HashMap<String, String> {
    plan.nodes
        .iter()
        .filter_map(|node| {
            mappings
                .get(&node.old_base)
                .map(|base| (node.branch.clone(), base.clone()))
        })
        .collect()
}

fn final_ref_transaction(
    ordered: &[String],
    nodes: &HashMap<&str, &Node>,
    temp_tips: &HashMap<String, String>,
    new_anchor_ref: Option<&str>,
    new_anchor: &str,
) -> Result<String> {
    let mut transaction = String::new();
    writeln!(transaction, "start").unwrap();
    if let Some(new_anchor_ref) = new_anchor_ref {
        writeln!(transaction, "verify {new_anchor_ref} {new_anchor}").unwrap();
    }
    for branch in ordered {
        let node = nodes
            .get(branch.as_str())
            .ok_or_else(|| Error::InvalidPlan(format!("unknown node `{branch}` in order")))?;
        let new_tip = temp_tips.get(&node.branch).ok_or_else(|| {
            Error::InvalidPlan(format!("branch `{}` has no rewritten tip", node.branch))
        })?;
        writeln!(
            transaction,
            "update refs/heads/{} {} {}",
            node.branch, new_tip, node.old_tip
        )
        .unwrap();
    }
    writeln!(transaction, "prepare").unwrap();
    writeln!(transaction, "commit").unwrap();

    Ok(transaction)
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
