use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::encoding::encode_component;
use crate::git::{Git, LocalBranch};
use crate::plan::{Dependency, Node, NodeRole, Plan, Repository, Source};
use crate::plan_validate::validate_plan;
use crate::storage::Storage;
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct GenerateOptions {
    pub anchor_branch: String,
    pub name: String,
    pub replace: bool,
    pub main: Option<String>,
}

#[derive(Debug, Clone)]
struct Candidate {
    branch: LocalBranch,
    parent_branch: String,
    old_base: String,
    commits: Vec<String>,
}

pub fn generate_named_plan(git: &Git, storage: &Storage, options: GenerateOptions) -> Result<Plan> {
    let plan = generate_plan(
        git,
        &options.anchor_branch,
        &options.name,
        options.main.as_deref(),
    )?;
    validate_plan(git, &plan)?;
    write_named_plan(storage, &options.name, &plan, options.replace)?;
    Ok(plan)
}

pub fn generate_plan(
    git: &Git,
    anchor_branch: &str,
    plan_name: &str,
    main: Option<&str>,
) -> Result<Plan> {
    let anchor_tip = git.local_branch_tip(anchor_branch)?;
    let anchor_base = infer_anchor_base(git, anchor_branch, &anchor_tip, main)?;
    let anchor_commits = owned_commits(git, &anchor_base, &anchor_tip, anchor_branch)?;

    let mut nodes = vec![Node {
        branch: anchor_branch.to_owned(),
        role: NodeRole::Anchor,
        parent: None,
        old_base: anchor_base.clone(),
        old_tip: anchor_tip.clone(),
        commits: anchor_commits,
    }];
    let mut dependencies = Vec::new();
    let mut assigned = HashSet::from([anchor_branch.to_owned()]);
    let branches = git.local_branches()?;

    while let Some(candidate) = next_candidate(git, &branches, &nodes, &assigned)? {
        assigned.insert(candidate.branch.name.clone());
        dependencies.push(Dependency {
            parent: candidate.parent_branch.clone(),
            child: candidate.branch.name.clone(),
        });
        nodes.push(Node {
            branch: candidate.branch.name,
            role: NodeRole::Dependent,
            parent: Some(candidate.parent_branch),
            old_base: candidate.old_base,
            old_tip: candidate.branch.tip,
            commits: candidate.commits,
        });
    }

    let now = OffsetDateTime::now_utc();
    let generated_at = now.format(&Rfc3339).map_err(|error| {
        Error::Unsupported(format!("failed to format generation timestamp: {error}"))
    })?;
    let plan_id = format!("{}-{}", now.unix_timestamp(), encode_component(plan_name));

    Ok(Plan {
        version: 1,
        plan_id,
        generated_at,
        repository: Repository {
            git_dir: git.git_common_dir()?.display().to_string(),
            head_at_generation: git.head_oid()?,
        },
        source: Source {
            anchor_branch: anchor_branch.to_owned(),
            anchor_old_tip: anchor_tip,
            anchor_old_base: anchor_base.clone(),
            suggested_manual_rebase_boundary: anchor_base,
        },
        nodes,
        dependencies,
    })
}

fn write_named_plan(storage: &Storage, name: &str, plan: &Plan, replace: bool) -> Result<()> {
    if storage.state_path().exists() {
        return Err(Error::ActiveOperation {
            path: storage.state_path(),
        });
    }

    storage.ensure_plans_dir()?;
    let path = storage.named_plan_path(name)?;
    if path.exists() && !replace {
        return Err(Error::PlanExists {
            name: name.to_owned(),
            path,
        });
    }

    let yaml = serde_yaml::to_string(plan)?;
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if replace {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }

    let mut file = options
        .open(&path)
        .map_err(|source| Error::IoWithPath { path, source })?;
    file.write_all(yaml.as_bytes())?;
    Ok(())
}

fn infer_anchor_base(
    git: &Git,
    anchor_branch: &str,
    anchor_tip: &str,
    main: Option<&str>,
) -> Result<String> {
    if let Some(main) = main {
        let main_tip = git.rev_parse(&format!("{main}^{{commit}}"))?;
        if let Some(base) = git.merge_base(anchor_tip, &main_tip)? {
            return Ok(base);
        }
    }

    if let Some(origin_default_tip) = git.origin_default_branch_tip()?
        && let Some(base) = git.merge_base(anchor_tip, &origin_default_tip)?
    {
        return Ok(base);
    }

    for base_branch in ["main", "master"] {
        if base_branch == anchor_branch {
            continue;
        }
        let Some(base_tip) = git.try_rev_parse(&format!("refs/heads/{base_branch}^{{commit}}"))?
        else {
            continue;
        };
        if let Some(base) = git.merge_base(anchor_tip, &base_tip)? {
            return Ok(base);
        }
    }

    Err(Error::CannotInferAnchorBase {
        branch: anchor_branch.to_owned(),
    })
}

fn next_candidate(
    git: &Git,
    branches: &[LocalBranch],
    nodes: &[Node],
    assigned: &HashSet<String>,
) -> Result<Option<Candidate>> {
    let node_by_branch = nodes
        .iter()
        .map(|node| (node.branch.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut best: Option<Candidate> = None;

    for branch in branches {
        if assigned.contains(&branch.name) {
            continue;
        }

        for parent in nodes {
            let Some(base) = git.merge_base(&parent.old_tip, &branch.tip)? else {
                continue;
            };
            if !parent.commits.contains(&base) {
                continue;
            }

            let commits = owned_commits(git, &base, &branch.tip, &branch.name)?;
            if commits.is_empty() {
                continue;
            }

            let candidate = Candidate {
                branch: branch.clone(),
                parent_branch: parent.branch.clone(),
                old_base: base,
                commits,
            };

            if is_better_candidate(&candidate, best.as_ref(), &node_by_branch) {
                best = Some(candidate);
            }
        }
    }

    Ok(best)
}

fn is_better_candidate(
    candidate: &Candidate,
    current: Option<&Candidate>,
    node_by_branch: &HashMap<&str, &Node>,
) -> bool {
    let Some(current) = current else {
        return true;
    };

    let candidate_parent_depth = parent_depth(&candidate.parent_branch, node_by_branch);
    let current_parent_depth = parent_depth(&current.parent_branch, node_by_branch);
    candidate_parent_depth
        .cmp(&current_parent_depth)
        .then_with(|| current.commits.len().cmp(&candidate.commits.len()))
        .then_with(|| current.branch.name.cmp(&candidate.branch.name))
        .is_gt()
}

fn parent_depth(branch: &str, node_by_branch: &HashMap<&str, &Node>) -> usize {
    let mut depth = 0;
    let mut current = node_by_branch.get(branch).copied();
    while let Some(node) = current {
        let Some(parent) = node.parent.as_deref() else {
            break;
        };
        depth += 1;
        current = node_by_branch.get(parent).copied();
    }
    depth
}

fn owned_commits(git: &Git, base: &str, tip: &str, branch: &str) -> Result<Vec<String>> {
    let merges = git.rev_list_merges(base, tip)?;
    if let Some(merge) = merges.first() {
        return Err(Error::Unsupported(format!(
            "branch `{branch}` contains merge commit `{merge}`; merge replay is not supported yet"
        )));
    }

    git.rev_list_reverse(base, tip)
}
