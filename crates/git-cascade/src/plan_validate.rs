use std::collections::{HashMap, HashSet};

use crate::git::Git;
use crate::plan::{Dependency, Node, Plan};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, Default)]
pub struct ValidateOptions {
    pub check_branch_refs: bool,
}

pub fn validate_plan(git: &Git, plan: &Plan) -> Result<()> {
    validate_plan_with_options(git, plan, ValidateOptions::default())
}

pub fn validate_plan_for_apply(git: &Git, plan: &Plan) -> Result<()> {
    validate_plan_with_options(
        git,
        plan,
        ValidateOptions {
            check_branch_refs: true,
        },
    )
}

pub fn validate_plan_with_options(git: &Git, plan: &Plan, options: ValidateOptions) -> Result<()> {
    validate_shape(plan)?;
    validate_git_objects(git, plan)?;
    validate_git_ranges(git, plan)?;
    validate_parent_reachability(git, plan)?;
    if options.check_branch_refs {
        validate_branch_refs(git, plan)?;
    }

    Ok(())
}

pub fn topological_order(plan: &Plan) -> Result<Vec<String>> {
    let node_by_branch = node_by_branch(plan)?;
    let mut children_by_parent = HashMap::<&str, Vec<&str>>::new();
    for dependency in &plan.dependencies {
        children_by_parent
            .entry(dependency.parent.as_str())
            .or_default()
            .push(dependency.child.as_str());
    }
    for children in children_by_parent.values_mut() {
        children.sort_unstable();
    }

    let anchor = anchor_node(plan)?;
    let mut ordered = Vec::new();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    visit(
        anchor.branch.as_str(),
        &node_by_branch,
        &children_by_parent,
        &mut visiting,
        &mut visited,
        &mut ordered,
    )?;

    if visited.len() != plan.nodes.len() {
        return invalid("dependency graph does not connect every node to the anchor");
    }

    ordered.retain(|branch| branch != &anchor.branch);
    Ok(ordered)
}

fn validate_shape(plan: &Plan) -> Result<()> {
    if plan.version != 1 {
        return invalid(format!("unsupported plan version `{}`", plan.version));
    }
    validate_ref_component("plan_id", &plan.plan_id)?;

    if plan.nodes.is_empty() {
        return invalid("plan must contain at least one node");
    }

    let node_by_branch = node_by_branch(plan)?;
    let anchor = anchor_node(plan)?;
    if anchor.branch != plan.source.anchor_branch {
        return invalid("source anchor_branch does not match the anchor node");
    }
    if anchor.old_tip != plan.source.anchor_old_tip {
        return invalid("source anchor_old_tip does not match the anchor node");
    }
    if anchor.old_base != plan.source.anchor_old_base {
        return invalid("source anchor_old_base does not match the anchor node");
    }

    let dependency_set = dependency_set(plan)?;
    for node in &plan.nodes {
        if node.branch.is_empty() {
            return invalid("node branch names must not be empty");
        }

        let Some(parent) = node.parent.as_deref() else {
            continue;
        };
        if !node_by_branch.contains_key(parent) {
            return invalid(format!(
                "dependent node `{}` references unknown parent `{parent}`",
                node.branch
            ));
        }
        if !dependency_set.contains(&(parent, node.branch.as_str())) {
            return invalid(format!(
                "dependent node `{}` is missing dependency edge from `{parent}`",
                node.branch
            ));
        }
    }

    for dependency in &plan.dependencies {
        validate_dependency(dependency, &node_by_branch)?;
        let child = node_by_branch[dependency.child.as_str()];
        if child.parent.as_deref() != Some(dependency.parent.as_str()) {
            return invalid(format!(
                "dependency edge `{} -> {}` does not match child parent field",
                dependency.parent, dependency.child
            ));
        }
    }

    topological_order(plan)?;
    validate_default_fork_point_mappability(plan, &node_by_branch)?;

    Ok(())
}

fn validate_git_objects(git: &Git, plan: &Plan) -> Result<()> {
    let mut commits = HashSet::<&str>::new();
    commits.insert(plan.source.anchor_old_base.as_str());
    commits.insert(plan.source.anchor_old_tip.as_str());

    for node in &plan.nodes {
        commits.insert(node.old_base.as_str());
        commits.insert(node.old_tip.as_str());
        for commit in &node.commits {
            commits.insert(commit.as_str());
        }
    }

    for commit in commits {
        if !git.commit_exists(commit)? {
            return invalid(format!("commit `{commit}` does not exist"));
        }
    }

    Ok(())
}

fn validate_git_ranges(git: &Git, plan: &Plan) -> Result<()> {
    for node in &plan.nodes {
        let commits = git.rev_list_reverse(&node.old_base, &node.old_tip)?;
        if commits != node.commits {
            return invalid(format!(
                "commit list for branch `{}` does not match {}..{}",
                node.branch, node.old_base, node.old_tip
            ));
        }

        let merges = git.rev_list_merges(&node.old_base, &node.old_tip)?;
        if let Some(merge) = merges.first() {
            return invalid(format!(
                "branch `{}` contains merge commit `{merge}`; merge replay is not supported yet",
                node.branch
            ));
        }
    }

    Ok(())
}

fn validate_parent_reachability(git: &Git, plan: &Plan) -> Result<()> {
    let node_by_branch = node_by_branch(plan)?;
    for node in &plan.nodes {
        let Some(parent) = node.parent.as_deref() else {
            continue;
        };
        let parent = node_by_branch[parent];
        if !git.is_ancestor(&node.old_base, &parent.old_tip)? {
            return invalid(format!(
                "old_base `{}` for branch `{}` is not reachable from parent `{}` old_tip `{}`",
                node.old_base, node.branch, parent.branch, parent.old_tip
            ));
        }
    }

    Ok(())
}

fn validate_branch_refs(git: &Git, plan: &Plan) -> Result<()> {
    for node in &plan.nodes {
        if node.parent.is_none() {
            continue;
        }

        let actual = git.local_branch_tip(&node.branch)?;
        if actual != node.old_tip {
            return invalid(format!(
                "branch `{}` moved after plan generation: expected `{}`, found `{actual}`",
                node.branch, node.old_tip
            ));
        }
    }

    Ok(())
}

fn validate_default_fork_point_mappability(
    plan: &Plan,
    node_by_branch: &HashMap<&str, &Node>,
) -> Result<()> {
    for node in &plan.nodes {
        let Some(parent_branch) = node.parent.as_deref() else {
            continue;
        };
        let parent = node_by_branch[parent_branch];
        if parent.parent.is_none() {
            continue;
        }
        if node.old_base != parent.old_base && !parent.commits.contains(&node.old_base) {
            return invalid(format!(
                "old_base `{}` for branch `{}` cannot be mapped through parent `{}`",
                node.old_base, node.branch, parent.branch
            ));
        }
    }

    Ok(())
}

fn validate_dependency<'a>(
    dependency: &'a Dependency,
    node_by_branch: &HashMap<&'a str, &'a Node>,
) -> Result<()> {
    if dependency.parent == dependency.child {
        return invalid(format!(
            "dependency for branch `{}` cannot point to itself",
            dependency.child
        ));
    }
    if !node_by_branch.contains_key(dependency.parent.as_str()) {
        return invalid(format!(
            "dependency references unknown parent `{}`",
            dependency.parent
        ));
    }
    if !node_by_branch.contains_key(dependency.child.as_str()) {
        return invalid(format!(
            "dependency references unknown child `{}`",
            dependency.child
        ));
    }

    Ok(())
}

fn visit<'a>(
    branch: &'a str,
    node_by_branch: &HashMap<&'a str, &'a Node>,
    children_by_parent: &HashMap<&'a str, Vec<&'a str>>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
    ordered: &mut Vec<String>,
) -> Result<()> {
    if visited.contains(branch) {
        return Ok(());
    }
    if !visiting.insert(branch) {
        return invalid(format!("dependency graph contains a cycle at `{branch}`"));
    }
    if !node_by_branch.contains_key(branch) {
        return invalid(format!(
            "dependency graph references unknown node `{branch}`"
        ));
    }

    ordered.push(branch.to_owned());

    if let Some(children) = children_by_parent.get(branch) {
        for child in children {
            visit(
                child,
                node_by_branch,
                children_by_parent,
                visiting,
                visited,
                ordered,
            )?;
        }
    }

    visiting.remove(branch);
    visited.insert(branch);
    Ok(())
}

fn node_by_branch(plan: &Plan) -> Result<HashMap<&str, &Node>> {
    let mut nodes = HashMap::new();
    for node in &plan.nodes {
        if nodes.insert(node.branch.as_str(), node).is_some() {
            return invalid(format!("duplicate node for branch `{}`", node.branch));
        }
    }

    Ok(nodes)
}

fn dependency_set(plan: &Plan) -> Result<HashSet<(&str, &str)>> {
    let mut dependencies = HashSet::new();
    for dependency in &plan.dependencies {
        if !dependencies.insert((dependency.parent.as_str(), dependency.child.as_str())) {
            return invalid(format!(
                "duplicate dependency edge `{} -> {}`",
                dependency.parent, dependency.child
            ));
        }
    }

    Ok(dependencies)
}

fn anchor_node(plan: &Plan) -> Result<&Node> {
    let mut anchors = plan.nodes.iter().filter(|node| node.parent.is_none());
    let Some(anchor) = anchors.next() else {
        return invalid("plan must contain exactly one anchor node");
    };
    if anchors.next().is_some() {
        return invalid("plan must contain exactly one anchor node");
    }

    Ok(anchor)
}

fn validate_ref_component(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return invalid(format!("{field} must not be empty"));
    }
    if value == "." || value == ".." || value.contains("..") || value.ends_with(".lock") {
        return invalid(format!("{field} `{value}` is not safe for a ref namespace"));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return invalid(format!("{field} `{value}` is not safe for a ref namespace"));
    }

    Ok(())
}

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::InvalidPlan(message.into()))
}

#[cfg(test)]
mod tests {
    use super::topological_order;
    use crate::plan::{Dependency, Node, Plan, Repository, Source};

    #[test]
    fn topological_order_returns_parents_before_children() {
        let plan = test_plan(vec![
            node("anchor", None),
            node("child", Some("anchor")),
            node("grandchild", Some("child")),
        ]);

        assert_eq!(topological_order(&plan).unwrap(), ["child", "grandchild"]);
    }

    #[test]
    fn topological_order_rejects_disconnected_nodes() {
        let plan = test_plan(vec![node("anchor", None), node("child", Some("missing"))]);

        assert!(topological_order(&plan).is_err());
    }

    fn test_plan(nodes: Vec<Node>) -> Plan {
        let dependencies = nodes
            .iter()
            .filter_map(|node| {
                node.parent.as_ref().map(|parent| Dependency {
                    parent: parent.clone(),
                    child: node.branch.clone(),
                })
            })
            .collect();

        Plan {
            version: 1,
            plan_id: "test-plan".to_owned(),
            generated_at: "2026-01-01T00:00:00Z".to_owned(),
            repository: Repository {
                git_dir: ".git".to_owned(),
                head_at_generation: "0".repeat(40),
            },
            source: Source {
                anchor_branch: "anchor".to_owned(),
                anchor_old_tip: "0".repeat(40),
                anchor_old_base: "0".repeat(40),
                suggested_manual_rebase_boundary: "0".repeat(40),
            },
            nodes,
            dependencies,
        }
    }

    fn node(branch: &str, parent: Option<&str>) -> Node {
        Node {
            branch: branch.to_owned(),
            parent: parent.map(str::to_owned),
            old_base: "0".repeat(40),
            old_tip: "0".repeat(40),
            commits: Vec::new(),
        }
    }
}
