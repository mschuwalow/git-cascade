use super::{Node, Plan};
use crate::model::BranchName;
use crate::{Error, Result};
use std::collections::{HashMap, HashSet};

pub fn branches_in_topological_order(plan: &Plan) -> Result<Vec<BranchName>> {
    let mut node_by_branch = HashMap::new();
    for node in &plan.nodes {
        if node_by_branch.insert(node.branch.as_str(), node).is_some() {
            return Err(Error::InvalidPlan(format!(
                "duplicate node for branch `{}`",
                node.branch
            )));
        }
    }

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
        visit_ordered(
            root,
            &node_by_branch,
            &children_by_parent,
            &mut visiting,
            &mut visited,
            &mut ordered,
        )?;
    }

    if visited.len() != plan.nodes.len() {
        return Err(Error::InvalidPlan(
            "dependency graph does not connect every node to a root".to_owned(),
        ));
    }

    Ok(ordered)
}

fn visit_ordered<'a>(
    branch: &'a str,
    node_by_branch: &HashMap<&'a str, &'a Node>,
    children_by_parent: &HashMap<&'a str, Vec<&'a str>>,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
    ordered: &mut Vec<BranchName>,
) -> Result<()> {
    if visited.contains(branch) {
        return Ok(());
    }
    if !visiting.insert(branch) {
        return Err(Error::InvalidPlan(format!(
            "dependency graph contains a cycle at `{branch}`"
        )));
    }
    if !node_by_branch.contains_key(branch) {
        return Err(Error::InvalidPlan(format!(
            "dependency graph references unknown node `{branch}`"
        )));
    }

    ordered.push(BranchName::new(branch));

    if let Some(children) = children_by_parent.get(branch) {
        for child in children {
            visit_ordered(
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

#[cfg(test)]
mod tests {
    use super::branches_in_topological_order;
    use crate::plan::{Dependency, Node, Plan, PlanCommit, PlanId, Repository, Source};
    use time::OffsetDateTime;

    #[test]
    fn orders_parents_before_children_and_sorts_siblings() {
        let plan = test_plan(
            vec![
                node("root-b", None),
                node("child-b", Some("root-a")),
                node("root-a", None),
                node("child-a", Some("root-a")),
                node("grandchild", Some("child-a")),
            ],
            vec![
                dependency("root-a", "child-b"),
                dependency("root-a", "child-a"),
                dependency("child-a", "grandchild"),
            ],
        );

        let ordered = branches_in_topological_order(&plan).unwrap();
        let ordered = ordered
            .iter()
            .map(|branch| branch.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            ["root-a", "child-a", "grandchild", "child-b", "root-b"]
        );
    }

    #[test]
    fn rejects_nodes_not_connected_to_a_root() {
        let plan = test_plan(
            vec![node("root", None), node("child", Some("missing"))],
            vec![dependency("missing", "child")],
        );

        assert!(branches_in_topological_order(&plan).is_err());
    }

    #[test]
    fn rejects_cycles_reachable_from_a_root() {
        let plan = test_plan(
            vec![
                node("root", None),
                node("child-a", Some("root")),
                node("child-b", Some("child-a")),
            ],
            vec![
                dependency("root", "child-a"),
                dependency("child-a", "child-b"),
                dependency("child-b", "child-a"),
            ],
        );

        assert!(branches_in_topological_order(&plan).is_err());
    }

    #[test]
    fn rejects_duplicate_branch_nodes() {
        let plan = test_plan(vec![node("root", None), node("root", None)], Vec::new());

        assert!(branches_in_topological_order(&plan).is_err());
    }

    fn test_plan(nodes: Vec<Node>, dependencies: Vec<Dependency>) -> Plan {
        Plan {
            version: 1,
            plan_id: PlanId::new(),
            generated_at: OffsetDateTime::UNIX_EPOCH,
            repository: Repository {
                git_dir: ".git".to_owned(),
                head_at_generation: "0".repeat(40).into(),
            },
            source: Source {
                name: "root".to_owned(),
                base: "0".repeat(40).into(),
                tip: "0".repeat(40).into(),
            },
            nodes,
            dependencies,
        }
    }

    fn node(branch: &str, parent: Option<&str>) -> Node {
        Node {
            branch: branch.into(),
            tip: "0".repeat(40).into(),
            base: "0".repeat(40).into(),
            commits: vec![PlanCommit::new("0".repeat(40), vec!["0".repeat(40)])],
            parent: parent.map(Into::into),
        }
    }

    fn dependency(parent: &str, child: &str) -> Dependency {
        Dependency {
            parent: parent.into(),
            child: child.into(),
        }
    }
}
