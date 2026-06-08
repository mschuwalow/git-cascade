mod common;

use git_cascade::plan::{NodeKind, Plan};
use predicates::prelude::*;

use common::repo::TestRepo;

#[test]
fn plan_creates_named_plan_for_linear_stack() {
    let repo = TestRepo::new();
    let initial = repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1-a.txt", "a\n", "pr-1 a");
    let pr1_b = repo.commit_file("pr1-b.txt", "b\n", "pr-1 b");
    repo.switch_new("pr-2");
    let pr2 = repo.commit_file("pr2.txt", "c\n", "pr-2");
    repo.switch_new("pr-3");
    let pr3 = repo.commit_file("pr3.txt", "d\n", "pr-3");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success()
        .stdout("created plan `stack`\n");

    let plan = read_plan(&repo, "stack");
    assert_eq!(plan.version, 1);
    assert_eq!(plan.source.name, "stack");
    assert_eq!(plan.source.old_base, initial);
    assert_eq!(plan.source.old_tip, pr1_b);
    assert_eq!(plan.nodes.len(), 2);
    assert_eq!(plan.dependencies.len(), 1);

    assert_eq!(plan.nodes[0].branch, "pr-2");
    assert_eq!(plan.nodes[0].old_base(), Some(pr1_b.as_str()));
    assert_eq!(plan.nodes[0].old_tip, pr2);
    assert_eq!(plan.nodes[0].commits(), std::slice::from_ref(&pr2));
    assert!(matches!(plan.nodes[0].kind, NodeKind::Root { .. }));

    assert_eq!(plan.nodes[1].branch, "pr-3");
    assert_eq!(plan.nodes[1].parent(), Some("pr-2"));
    assert_eq!(plan.nodes[1].old_base(), Some(pr2.as_str()));
    assert_eq!(plan.nodes[1].old_tip, pr3);
    assert_eq!(plan.nodes[1].commits(), std::slice::from_ref(&pr3));

    assert_eq!(plan.dependencies[0].parent, "pr-2");
    assert_eq!(plan.dependencies[0].child, "pr-3");
}

#[test]
fn plan_preserves_intermediate_fork_point() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");
    let fork_point = repo.commit_file("b.txt", "b\n", "b");
    repo.commit_file("c.txt", "c\n", "c");
    repo.switch_new_at("pr-2", &fork_point);
    repo.commit_file("d.txt", "d\n", "d");

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();

    let plan = read_plan(&repo, "stack");
    let child = plan
        .nodes
        .iter()
        .find(|node| node.branch == "pr-2")
        .unwrap();

    assert_eq!(child.parent(), None);
    assert_eq!(child.old_base(), Some(fork_point.as_str()));
    assert!(matches!(child.kind, NodeKind::Root { .. }));
}

#[test]
fn plan_does_not_treat_advanced_main_as_dependent() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");
    repo.commit_file("main.txt", "new main\n", "advance main");

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();

    let plan = read_plan(&repo, "stack");
    let branches = plan
        .nodes
        .iter()
        .map(|node| node.branch.as_str())
        .collect::<Vec<_>>();
    assert_eq!(branches, ["pr-2"]);
}

#[test]
fn plan_old_base_option_uses_merge_base_with_old_tip() {
    let repo = TestRepo::new();
    let initial = repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");
    repo.commit_file("main.txt", "new main\n", "advance main");

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();

    let plan = read_plan(&repo, "stack");
    assert_eq!(plan.source.old_base, initial);
    assert_eq!(plan.nodes.len(), 1);
}

#[test]
fn plan_infers_old_base_from_local_default_branch() {
    let repo = TestRepo::new();
    let initial = repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");
    repo.commit_file("main.txt", "new main\n", "advance main");

    repo.cascade()
        .args(["plan", "stack", "--old-tip", "pr-1"])
        .assert()
        .success();

    let plan = read_plan(&repo, "stack");
    assert_eq!(plan.source.old_base, initial);
    let branches = plan
        .nodes
        .iter()
        .map(|node| node.branch.as_str())
        .collect::<Vec<_>>();
    assert_eq!(branches, ["pr-2"]);
}

#[test]
fn plan_inference_ignores_old_tip_branch_upstream() {
    let repo = TestRepo::new();
    let initial = repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.git_ok(["update-ref", "refs/remotes/origin/pr-1", "pr-1"]);
    repo.git_ok(["config", "branch.pr-1.remote", "origin"]);
    repo.git_ok(["config", "branch.pr-1.merge", "refs/heads/pr-1"]);
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");
    repo.commit_file("main.txt", "new main\n", "advance main");

    repo.cascade()
        .args(["plan", "stack", "--old-tip", "pr-1"])
        .assert()
        .success();

    let plan = read_plan(&repo, "stack");
    assert_eq!(plan.source.old_base, initial);
    let branches = plan
        .nodes
        .iter()
        .map(|node| node.branch.as_str())
        .collect::<Vec<_>>();
    assert_eq!(branches, ["pr-2"]);
}

#[test]
fn plan_supports_single_commit_root_range() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    let old_commit = repo.commit_file("pr1.txt", "a\n", "single root commit");
    let old_base = repo.rev_parse("pr-1^");
    repo.switch_new("pr-2");
    let pr2 = repo.commit_file("pr2.txt", "b\n", "dependent");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "single", "--old-base", "pr-1^", "--old-tip", "pr-1"])
        .assert()
        .success();

    let plan = read_plan(&repo, "single");
    assert_eq!(plan.source.old_base, old_base);
    assert_eq!(plan.source.old_tip, old_commit);
    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].branch, "pr-2");
    assert_eq!(plan.nodes[0].parent(), None);
    assert_eq!(plan.nodes[0].old_base(), Some(old_commit.as_str()));
    assert_eq!(plan.nodes[0].old_tip, pr2);
    assert!(matches!(plan.nodes[0].kind, NodeKind::Root { .. }));
}

#[test]
fn plan_accepts_tag_anchor() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    let anchor_tip = repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.git_ok(["tag", "stack-anchor"]);
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");

    repo.cascade()
        .args([
            "plan",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "refs/tags/stack-anchor",
        ])
        .assert()
        .success();

    let plan = read_plan(&repo, "stack");
    assert_eq!(plan.source.name, "stack");
    assert_eq!(plan.source.old_tip, anchor_tip);
    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].branch, "pr-2");
    assert_eq!(plan.nodes[0].parent(), None);
    assert!(matches!(plan.nodes[0].kind, NodeKind::Root { .. }));
}

#[test]
fn plan_with_full_local_branch_ref_does_not_include_anchor_branch_as_dependent() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");

    repo.cascade()
        .args([
            "plan",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "refs/heads/pr-1",
        ])
        .assert()
        .success();

    let plan = read_plan(&repo, "stack");
    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].branch, "pr-2");
    assert_eq!(plan.nodes[0].parent(), None);
    assert!(matches!(plan.nodes[0].kind, NodeKind::Root { .. }));
}

#[test]
fn plan_refuses_to_overwrite_without_replace() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    repo.cascade()
        .args([
            "plan",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
            "--replace",
        ])
        .assert()
        .success();
}

#[test]
fn plan_refuses_while_state_exists() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");
    let state_path = repo.common_dir().join("cascade/state.yaml");
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(&state_path, "version: 1\n").unwrap();

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("state file exists"));
}

#[test]
fn plan_rejects_merge_commits() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");
    repo.switch_new("pr-2");
    repo.commit_file("b.txt", "b\n", "b");
    repo.switch_new_at("side", "pr-1");
    repo.commit_file("side.txt", "side\n", "side");
    repo.switch("pr-2");
    repo.git_ok(["merge", "--no-ff", "side", "-m", "merge side"]);
    repo.switch("main");

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("merge replay is not supported"));
}

fn read_plan(repo: &TestRepo, name: &str) -> Plan {
    let content = std::fs::read_to_string(repo.plan_path(name)).unwrap();
    serde_yaml::from_str(&content).unwrap()
}
