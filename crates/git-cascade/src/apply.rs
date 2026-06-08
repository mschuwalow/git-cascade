use std::collections::HashMap;
use std::fmt::Write as _;

use crate::encoding::encode_component;
use crate::git::Git;
use crate::plan::{Node, Plan};
use crate::plan_validate::{topological_order, validate_plan_for_apply};
use crate::storage::Storage;
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct DryRunOptions {
    pub new_anchor_input: String,
    pub move_to_heads: bool,
}

#[derive(Debug, Clone)]
enum ReplayBase {
    ResolvedCommit(String),
    RewrittenCommit { branch: String, old_commit: String },
    RewrittenTip { branch: String },
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
    let worktree = storage.worktrees_dir().join(&plan.plan_id);
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
