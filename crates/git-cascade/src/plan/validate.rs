use std::collections::{HashMap, HashSet};

use super::{Dependency, Node, Plan};
use crate::git::Git;
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

    let mut ordered = Vec::new();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    let mut roots = plan
        .nodes
        .iter()
        .filter(|node| node.is_root())
        .map(|node| node.branch.as_str())
        .collect::<Vec<_>>();
    roots.sort_unstable();
    for root in roots {
        visit(
            root,
            &node_by_branch,
            &children_by_parent,
            &mut visiting,
            &mut visited,
            &mut ordered,
        )?;
    }

    if visited.len() != plan.nodes.len() {
        return invalid("dependency graph does not connect every node to a root");
    }

    Ok(ordered)
}

fn validate_shape(plan: &Plan) -> Result<()> {
    if plan.version != 1 {
        return invalid(format!("unsupported plan version `{}`", plan.version));
    }
    let node_by_branch = node_by_branch(plan)?;
    if plan.source.name.is_empty() {
        return invalid("source name must not be empty");
    }
    if plan.source.base == plan.source.tip {
        return invalid("source base must differ from tip");
    }

    let dependency_set = dependency_set(plan)?;
    for node in &plan.nodes {
        if node.branch.is_empty() {
            return invalid("node branch names must not be empty");
        }
        if node.commits().is_empty() {
            return invalid(format!(
                "node `{}` must contain at least one commit",
                node.branch
            ));
        }
        if node.commits().last() != Some(&node.tip) {
            return invalid(format!(
                "node `{}` tip must match the last commit",
                node.branch
            ));
        }

        let Some(parent) = node.parent() else {
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
        if child.parent() != Some(dependency.parent.as_str()) {
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
    commits.insert(plan.source.base.as_str());
    commits.insert(plan.source.tip.as_str());

    for node in &plan.nodes {
        commits.insert(node.tip.as_str());
        commits.insert(node.base());
        for commit in node.commits() {
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
        let tip = node.tip.as_str();
        let base = node.base();
        let commits = git.rev_list_reverse(base, tip)?;
        if commits != node.commits() {
            return invalid(format!(
                "commit list for branch `{}` does not match {}..{}",
                node.branch, base, tip
            ));
        }

        let merges = git.rev_list_merges(base, tip)?;
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
    if !git.is_ancestor(&plan.source.base, &plan.source.tip)? {
        return invalid(format!(
            "source base `{}` is not reachable from tip `{}`",
            plan.source.base, plan.source.tip
        ));
    }

    let node_by_branch = node_by_branch(plan)?;
    for node in &plan.nodes {
        let base = node.base();
        if node.is_root() {
            if base == plan.source.base
                || !git.is_ancestor(&plan.source.base, base)?
                || !git.is_ancestor(base, &plan.source.tip)?
            {
                return invalid(format!(
                    "base `{}` for branch `{}` is outside source range {}..{}",
                    base, node.branch, plan.source.base, plan.source.tip
                ));
            }
            continue;
        }

        let parent = node_by_branch[node.parent().expect("dependent node has a parent")];
        let parent_tip = parent.tip.as_str();
        if !git.is_ancestor(base, parent_tip)? {
            return invalid(format!(
                "base `{}` for branch `{}` is not reachable from parent `{}` tip `{}`",
                base, node.branch, parent.branch, parent_tip
            ));
        }
    }

    Ok(())
}

fn validate_branch_refs(git: &Git, plan: &Plan) -> Result<()> {
    for node in &plan.nodes {
        let tip = node.tip.as_str();
        let actual = git.local_branch_tip(&node.branch)?;
        if !git.is_ancestor(tip, &actual)? {
            return invalid(format!(
                "branch `{}` rewrote planned commits after plan generation: planned tip `{}` is not reachable from `{actual}`",
                node.branch, tip
            ));
        }

        let merges = git.rev_list_merges(tip, &actual)?;
        if let Some(merge) = merges.first() {
            return invalid(format!(
                "branch `{}` added merge commit `{merge}` after plan generation; merge replay is not supported yet",
                node.branch
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
        if node.is_root() {
            continue;
        }
        let parent_branch = node.parent().expect("dependent node has a parent");
        let parent = node_by_branch[parent_branch];
        let base = node.base();
        let parent_base = parent.base();
        if base != parent_base && !parent.commits().iter().any(|commit| commit == base) {
            return invalid(format!(
                "base `{}` for branch `{}` cannot be mapped through parent `{}`",
                base, node.branch, parent.branch
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

fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::InvalidPlan(message.into()))
}

#[cfg(test)]
mod tests {
    use super::topological_order;
    use crate::plan::{Dependency, Node, Plan, PlanId, Repository, Source};
    use time::OffsetDateTime;

    #[test]
    fn topological_order_returns_parents_before_children() {
        let plan = test_plan(vec![
            node("anchor", None),
            node("child", Some("anchor")),
            node("grandchild", Some("child")),
        ]);

        assert_eq!(
            topological_order(&plan).unwrap(),
            ["anchor", "child", "grandchild"]
        );
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
                node.parent().map(|parent| Dependency {
                    parent: parent.to_owned(),
                    child: node.branch.clone(),
                })
            })
            .collect();

        Plan {
            version: 1,
            plan_id: PlanId::new(),
            generated_at: OffsetDateTime::UNIX_EPOCH,
            repository: Repository {
                git_dir: ".git".to_owned(),
                head_at_generation: "0".repeat(40),
            },
            source: Source {
                name: "anchor".to_owned(),
                base: "0".repeat(40),
                tip: "0".repeat(40),
            },
            nodes,
            dependencies,
        }
    }

    fn node(branch: &str, parent: Option<&str>) -> Node {
        Node {
            branch: branch.to_owned(),
            tip: "0".repeat(40),
            base: "0".repeat(40),
            commits: vec!["0".repeat(40)],
            parent: parent.map(str::to_owned),
        }
    }
}
