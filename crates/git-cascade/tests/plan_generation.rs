mod common;

use git_cascade::plan::Plan;
use predicates::prelude::*;

use common::repo::TestRepo;

#[test]
fn plan_creates_named_plan_for_linear_stack() {
    let repo = TestRepo::new();
    let main = repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    let pr1_a = repo.commit_file("pr1-a.txt", "a\n", "pr-1 a");
    let pr1_b = repo.commit_file("pr1-b.txt", "b\n", "pr-1 b");
    repo.switch_new("pr-2");
    let pr2 = repo.commit_file("pr2.txt", "c\n", "pr-2");
    repo.switch_new("pr-3");
    let pr3 = repo.commit_file("pr3.txt", "d\n", "pr-3");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "pr-1", "--name", "stack"])
        .assert()
        .success()
        .stdout("created plan `stack`\n");

    let plan = read_plan(&repo, "stack");
    assert_eq!(plan.version, 1);
    assert_eq!(plan.source.anchor_branch, "pr-1");
    assert_eq!(plan.source.anchor_old_base, main);
    assert_eq!(plan.source.anchor_old_tip, pr1_b);
    assert_eq!(plan.nodes.len(), 3);
    assert_eq!(plan.dependencies.len(), 2);

    assert_eq!(plan.nodes[0].branch, "pr-1");
    assert_eq!(plan.nodes[0].parent, None);
    assert_eq!(plan.nodes[0].commits, vec![pr1_a, pr1_b.clone()]);

    assert_eq!(plan.nodes[1].branch, "pr-2");
    assert_eq!(plan.nodes[1].parent.as_deref(), Some("pr-1"));
    assert_eq!(plan.nodes[1].old_base, pr1_b);
    assert_eq!(plan.nodes[1].old_tip, pr2);

    assert_eq!(plan.nodes[2].branch, "pr-3");
    assert_eq!(plan.nodes[2].parent.as_deref(), Some("pr-2"));
    assert_eq!(plan.nodes[2].old_base, pr2);
    assert_eq!(plan.nodes[2].old_tip, pr3);

    assert_eq!(plan.dependencies[0].parent, "pr-1");
    assert_eq!(plan.dependencies[0].child, "pr-2");
    assert_eq!(plan.dependencies[1].parent, "pr-2");
    assert_eq!(plan.dependencies[1].child, "pr-3");
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
        .args(["plan", "pr-1", "--name", "intermediate"])
        .assert()
        .success();

    let plan = read_plan(&repo, "intermediate");
    let child = plan
        .nodes
        .iter()
        .find(|node| node.branch == "pr-2")
        .unwrap();

    assert_eq!(child.parent.as_deref(), Some("pr-1"));
    assert_eq!(child.old_base, fork_point);
}

#[test]
fn plan_uses_explicit_main_ref() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("trunk");
    let trunk_tip = repo.commit_file("trunk.txt", "trunk\n", "trunk");
    repo.switch_new("pr-1");
    repo.commit_file("feature.txt", "feature\n", "feature");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "pr-1", "--name", "explicit-main", "--main", "trunk"])
        .assert()
        .success();

    let plan = read_plan(&repo, "explicit-main");

    assert_eq!(plan.source.anchor_old_base, trunk_tip);
}

#[test]
fn plan_uses_origin_default_branch_when_main_is_not_passed() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("trunk");
    let trunk_tip = repo.commit_file("trunk.txt", "trunk\n", "trunk");
    repo.switch_new("pr-1");
    repo.commit_file("feature.txt", "feature\n", "feature");
    repo.switch("main");
    repo.git_ok(["update-ref", "refs/remotes/origin/trunk", &trunk_tip]);
    repo.git_ok([
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    repo.git_ok(["branch", "-D", "trunk"]);

    repo.cascade()
        .args(["plan", "pr-1", "--name", "origin-default"])
        .assert()
        .success();

    let plan = read_plan(&repo, "origin-default");

    assert_eq!(plan.source.anchor_old_base, trunk_tip);
}

#[test]
fn plan_refuses_to_overwrite_without_replace() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");

    repo.cascade()
        .args(["plan", "pr-1", "--name", "stack"])
        .assert()
        .success();

    repo.cascade()
        .args(["plan", "pr-1", "--name", "stack"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    repo.cascade()
        .args(["plan", "pr-1", "--name", "stack", "--replace"])
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
        .args(["plan", "pr-1", "--name", "stack"])
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
    repo.switch_new_at("side", "main");
    repo.commit_file("side.txt", "side\n", "side");
    repo.switch("pr-1");
    repo.git_ok(["merge", "--no-ff", "side", "-m", "merge side"]);

    repo.cascade()
        .args(["plan", "pr-1", "--name", "stack"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("merge replay is not supported"));
}

fn read_plan(repo: &TestRepo, name: &str) -> Plan {
    let content = std::fs::read_to_string(repo.named_plan_path(name)).unwrap();
    serde_yaml::from_str(&content).unwrap()
}
