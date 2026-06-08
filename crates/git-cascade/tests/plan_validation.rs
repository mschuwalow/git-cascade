mod common;

use git_cascade::git::Git;
use git_cascade::plan::Plan;
use git_cascade::plan_validate::{validate_plan, validate_plan_for_apply};

use common::repo::TestRepo;

#[test]
fn validates_generated_plan() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "--anchor", "pr-1", "--name", "stack"])
        .assert()
        .success();
    let plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    validate_plan(&git, &plan).unwrap();
    validate_plan_for_apply(&git, &plan).unwrap();
}

#[test]
fn validation_rejects_tampered_commit_list() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "--anchor", "pr-1", "--name", "stack"])
        .assert()
        .success();
    let mut plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    plan.nodes
        .iter_mut()
        .find(|node| node.branch == "pr-3")
        .unwrap()
        .commits
        .clear();

    let error = validate_plan(&git, &plan).unwrap_err().to_string();

    assert!(error.contains("commit list for branch `pr-3` does not match"));
}

#[test]
fn apply_validation_rejects_dependent_branch_that_moved_after_planning() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "--anchor", "pr-1", "--name", "stack"])
        .assert()
        .success();
    let plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    repo.switch("pr-2");
    repo.commit_file("late.txt", "late\n", "late pr-2 work");

    validate_plan(&git, &plan).unwrap();
    let error = validate_plan_for_apply(&git, &plan)
        .unwrap_err()
        .to_string();

    assert!(error.contains("branch `pr-2` moved after plan generation"));
}

#[test]
fn validation_rejects_dependency_parent_mismatch() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "--anchor", "pr-1", "--name", "stack"])
        .assert()
        .success();
    let mut plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    plan.dependencies[0].parent = "pr-3".to_owned();

    let error = validate_plan(&git, &plan).unwrap_err().to_string();

    assert!(error.contains("is missing dependency edge"));
}

fn linear_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch_new("pr-3");
    repo.commit_file("pr3.txt", "c\n", "pr-3");
    repo.switch("main");
    repo
}

fn read_plan(repo: &TestRepo, name: &str) -> Plan {
    let content = std::fs::read_to_string(repo.named_plan_path(name)).unwrap();
    serde_yaml::from_str(&content).unwrap()
}
