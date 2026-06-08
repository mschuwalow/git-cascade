use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;

use crate::encoding::{decode_component, encode_component};
use crate::git::Git;
use crate::plan::{Node, Plan};
use crate::plan_validate::{topological_order, validate_plan_for_apply};
use crate::recovery;
use crate::state::{
    ApplyState, ApplyStateInput, CurrentState, Operation, Phase, StateFile, Strategy,
    initial_apply_state,
};
use crate::storage::{PlanName, Storage};
use crate::test_hooks;
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct DryRunOptions {
    pub new_tip_input: String,
    pub strategy: Strategy,
}

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub plan_name: PlanName,
    pub new_tip_input: String,
    pub strategy: Strategy,
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
    new_tip: &'a str,
    strategy: Strategy,
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
    let new_tip = git.resolve_commit(&options.new_tip_input)?;
    let ordered = topological_order(plan)?;
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let anchor = plan
        .nodes
        .iter()
        .find(|node| node.is_anchor())
        .ok_or_else(|| {
            Error::InvalidPlan("plan must contain exactly one anchor node".to_owned())
        })?;

    let mut selected_bases = HashMap::<String, ReplayBase>::new();
    let mut output = String::new();
    let worktree = storage.worktrees_dir().join(&plan.plan_id);
    let strategy = options.strategy.as_str();

    writeln!(output, "# git-cascade apply --dry-run").unwrap();
    writeln!(output, "new-tip {} -> {}", options.new_tip_input, new_tip).unwrap();
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
            &new_tip,
            options.strategy,
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
        for commit in node.commits() {
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
    let new_tip = git.resolve_commit(&options.new_tip_input)?;
    let new_tip_ref = git.symbolic_full_name(&options.new_tip_input)?;
    let ordered = topological_order(plan)?;
    let nodes = plan
        .nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let anchor = plan
        .nodes
        .iter()
        .find(|node| node.is_anchor())
        .ok_or_else(|| {
            Error::InvalidPlan("plan must contain exactly one anchor node".to_owned())
        })?;

    let mut mappings = BTreeMap::new();
    mappings.insert(plan.source.old_tip.clone(), new_tip.clone());
    let worktree = storage.worktrees_dir().join(&plan.plan_id);
    let mut state = initial_apply_state(ApplyStateInput {
        plan_name: &options.plan_name,
        plan_id: &plan.plan_id,
        new_tip_input: &options.new_tip_input,
        new_tip_resolved: &new_tip,
        new_tip_input_was_ref: new_tip_ref.is_some(),
        strategy: options.strategy,
        pending_branches: ordered.clone(),
        mappings: mappings.clone(),
        worktree: worktree.display().to_string(),
    })?;
    let mut state_file = StateFile::create(storage, &state)?;

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
                new_tip: &new_tip,
                strategy: options.strategy,
            },
        )?;
        selected_bases.insert(node.branch.clone(), base.clone());
        mappings.insert(
            node.old_base()
                .expect("dependent node has old base")
                .to_owned(),
            base.clone(),
        );
        state.mappings = mappings.clone();
        state.pending.branches = ordered[index..].to_vec();
        state_file.write_state(&mut state)?;

        if index == 0 {
            git.worktree_add_detached(&worktree, &base)?;
            worktree_created = true;
        } else {
            worktree_git.reset_hard(&base)?;
        }

        for commit in node.commits() {
            state.phase = Phase::Replay;
            state.current = Some(CurrentState {
                branch: node.branch.clone(),
                commit: commit.clone(),
                worktree: worktree.display().to_string(),
            });
            state_file.write_state(&mut state)?;

            if let Err(error) = worktree_git.cherry_pick(commit) {
                state.phase = Phase::Conflict;
                state_file.write_state(&mut state)?;
                return Err(Error::ApplyStopped {
                    branch: node.branch.clone(),
                    commit: commit.clone(),
                    worktree: worktree.clone(),
                    message: error.to_string(),
                });
            }
            mappings.insert(commit.clone(), worktree_git.head_oid()?);
            state.mappings = mappings.clone();
            state_file.write_state(&mut state)?;
        }

        let rewritten_tip = worktree_git.head_oid()?;
        let temp_ref = temp_ref(plan, &node.branch);
        git.update_ref(&temp_ref, &rewritten_tip)?;
        temp_tips.insert(node.branch.clone(), rewritten_tip);
        temp_refs.push(temp_ref);
        state.completed.temp_refs = temp_refs.clone();
        state.current = None;
        state.pending.branches = ordered[index + 1..].to_vec();
        state_file.write_state(&mut state)?;
    }

    state.phase = Phase::FinalUpdate;
    state_file.write_state(&mut state)?;
    test_hooks::run("before-final-update")?;
    git.update_ref_transaction(&final_ref_transaction(
        &ordered,
        &nodes,
        &temp_tips,
        new_tip_ref.as_deref(),
        &new_tip,
    )?)?;

    if worktree_created || !temp_refs.is_empty() || storage.state_path().exists() {
        recovery::mark_deleting_and_cleanup(git, storage, state_file, &mut state)?;
        storage.delete_plan(options.plan_name)?;
    } else {
        state_file.remove()?;
        storage.delete_plan(options.plan_name)?;
    }

    Ok(())
}

pub fn continue_apply(git: &Git, storage: &Storage) -> Result<()> {
    let mut state_file = StateFile::open(storage)?
        .ok_or_else(|| Error::InvalidInvocation("no active cascade operation".to_owned()))?;
    let mut state = state_file.read_state()?;
    if state.phase == Phase::Deleting {
        recovery::cleanup_state_artifacts(git, storage, state_file, &state)?;
        return Err(Error::InvalidInvocation(
            "no active cascade operation".to_owned(),
        ));
    }
    if state.operation != Operation::Apply {
        return Err(Error::InvalidInvocation(format!(
            "cannot continue unsupported operation `{}`",
            state.operation
        )));
    }
    if state.phase != Phase::Conflict {
        return Err(Error::InvalidInvocation(format!(
            "cannot continue cascade operation in phase `{}`",
            state.phase
        )));
    }

    let current = state.current.clone().ok_or_else(|| {
        Error::InvalidInvocation("active apply state has no current commit".to_owned())
    })?;
    let plan_name = state.plan_name.clone();
    let plan: Plan = serde_yaml::from_str(&storage.read_plan(&plan_name)?)?;
    validate_plan_for_apply(git, &plan)?;

    if state.new_tip.input_was_ref {
        let resolved = git.resolve_commit(&state.new_tip.input)?;
        if resolved != state.new_tip.resolved {
            return Err(Error::InvalidInvocation(format!(
                "new tip `{}` moved after apply started: expected `{}`, found `{resolved}`",
                state.new_tip.input, state.new_tip.resolved
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
    state.phase = Phase::Replay;
    state_file.write_state(&mut state)?;

    continue_replay_after_resolved_commit(
        git,
        storage,
        &plan,
        state_file,
        state,
        &current.branch,
        &current.commit,
    )
}

fn continue_replay_after_resolved_commit(
    git: &Git,
    storage: &Storage,
    plan: &Plan,
    mut state_file: StateFile,
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
        .find(|node| node.is_anchor())
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
            node.commits()
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
                    new_tip: &state.new_tip.resolved,
                    strategy: state.strategy,
                },
            )?;
            selected_bases.insert(node.branch.clone(), base.clone());
            mappings.insert(
                node.old_base()
                    .expect("dependent node has old base")
                    .to_owned(),
                base.clone(),
            );
            state.mappings = mappings.clone();
            state.pending.branches = ordered[index..].to_vec();
            state_file.write_state(&mut state)?;
            worktree_git.reset_hard(&base)?;
            0
        };

        for commit in node.commits().iter().skip(start_commit_index) {
            state.phase = Phase::Replay;
            state.current = Some(CurrentState {
                branch: node.branch.clone(),
                commit: commit.clone(),
                worktree: worktree.display().to_string(),
            });
            state_file.write_state(&mut state)?;

            if let Err(error) = worktree_git.cherry_pick(commit) {
                state.phase = Phase::Conflict;
                state_file.write_state(&mut state)?;
                return Err(Error::ApplyStopped {
                    branch: node.branch.clone(),
                    commit: commit.clone(),
                    worktree: worktree.clone(),
                    message: error.to_string(),
                });
            }
            mappings.insert(commit.clone(), worktree_git.head_oid()?);
            state.mappings = mappings.clone();
            state_file.write_state(&mut state)?;
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
        state_file.write_state(&mut state)?;
    }

    state.phase = Phase::FinalUpdate;
    state_file.write_state(&mut state)?;
    let new_tip_ref = if state.new_tip.input_was_ref {
        Some(
            git.symbolic_full_name(&state.new_tip.input)?
                .ok_or_else(|| {
                    Error::InvalidInvocation(format!(
                        "new tip `{}` no longer resolves to a ref",
                        state.new_tip.input
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
        new_tip_ref.as_deref(),
        &state.new_tip.resolved,
    )?)?;

    recovery::mark_deleting_and_cleanup(git, storage, state_file, &mut state)?;
    let plan_name = state.plan_name.clone();
    storage.delete_plan(plan_name)?;

    Ok(())
}

fn replay_base(
    node: &Node,
    anchor: &Node,
    nodes: &HashMap<&str, &Node>,
    selected_bases: &HashMap<String, ReplayBase>,
    new_tip: &str,
    strategy: Strategy,
) -> Result<ReplayBase> {
    let parent_branch = node.parent().ok_or_else(|| {
        Error::InvalidPlan(format!("anchor node `{}` cannot be replayed", node.branch))
    })?;
    let parent = nodes
        .get(parent_branch)
        .ok_or_else(|| Error::InvalidPlan(format!("unknown parent `{parent_branch}`")))?;

    if parent.branch == anchor.branch {
        return Ok(ReplayBase::ResolvedCommit(new_tip.to_owned()));
    }

    if strategy == Strategy::MoveToHeads {
        return Ok(ReplayBase::RewrittenTip {
            branch: parent.branch.clone(),
        });
    }

    let old_base = node.old_base().expect("dependent node has old base");
    if Some(old_base) == parent.old_base() {
        return selected_bases.get(&parent.branch).cloned().ok_or_else(|| {
            Error::InvalidPlan(format!(
                "parent `{}` has no selected replay base",
                parent.branch
            ))
        });
    }

    Ok(ReplayBase::RewrittenCommit {
        branch: parent.branch.clone(),
        old_commit: old_base.to_owned(),
    })
}

fn actual_replay_base(node: &Node, context: ActualReplayContext<'_>) -> Result<String> {
    let parent_branch = node.parent().ok_or_else(|| {
        Error::InvalidPlan(format!("anchor node `{}` cannot be replayed", node.branch))
    })?;
    let parent = context
        .nodes
        .get(parent_branch)
        .ok_or_else(|| Error::InvalidPlan(format!("unknown parent `{parent_branch}`")))?;

    if parent.branch == context.anchor.branch {
        return Ok(context.new_tip.to_owned());
    }

    if context.strategy == Strategy::MoveToHeads {
        return context
            .temp_tips
            .get(&parent.branch)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no rewritten tip", parent.branch))
            });
    }

    let old_base = node.old_base().expect("dependent node has old base");
    if Some(old_base) == parent.old_base() {
        return context
            .selected_bases
            .get(&parent.branch)
            .cloned()
            .ok_or_else(|| {
                Error::InvalidPlan(format!("parent `{}` has no selected base", parent.branch))
            });
    }

    context.mappings.get(old_base).cloned().ok_or_else(|| {
        Error::InvalidPlan(format!(
            "old_base `{}` for branch `{}` was not mapped",
            old_base, node.branch
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
                .get(node.old_base()?)
                .map(|base| (node.branch.clone(), base.clone()))
        })
        .collect()
}

fn final_ref_transaction(
    ordered: &[String],
    nodes: &HashMap<&str, &Node>,
    temp_tips: &HashMap<String, String>,
    new_tip_ref: Option<&str>,
    new_tip: &str,
) -> Result<String> {
    let mut transaction = String::new();
    writeln!(transaction, "start").unwrap();
    if let Some(new_tip_ref) = new_tip_ref {
        writeln!(transaction, "verify {new_tip_ref} {new_tip}").unwrap();
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
