mod common;

use git_cascade::git::Git;
use git_cascade::plan::{Plan, PlanCommit};
use git_cascade::plan::{validate_plan, validate_plan_for_apply};

use common::repo::TestRepo;

#[test]
fn validates_generated_plan() {
    let repo = linear_stack();
    repo.cascade()
        .args([
            "plan",
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
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
        .args([
            "plan",
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
        .assert()
        .success();
    let mut plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    let node = plan
        .nodes
        .iter_mut()
        .find(|node| node.branch.as_str() == "pr-3")
        .unwrap();
    assert_eq!(node.parent(), Some("pr-2"));
    node.commits.push(node.commits[0].clone());

    let error = validate_plan(&git, &plan).unwrap_err().to_string();

    assert!(error.contains("commit list for branch `pr-3` does not match"));
}

#[test]
fn apply_validation_rejects_dependent_branch_that_moved_after_planning() {
    let repo = linear_stack();
    repo.cascade()
        .args([
            "plan",
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
        .assert()
        .success();
    let plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    repo.switch("pr-2");
    repo.git_ok(["reset", "--hard", "HEAD^"]);
    repo.commit_file("replacement.txt", "replacement\n", "replacement pr-2 work");

    validate_plan(&git, &plan).unwrap();
    let error = validate_plan_for_apply(&git, &plan)
        .unwrap_err()
        .to_string();

    assert!(error.contains("branch `pr-2` rewrote planned commits after plan generation"));
}

#[test]
fn apply_validation_allows_added_dependent_commits() {
    let repo = linear_stack();
    repo.cascade()
        .args([
            "plan",
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
        .assert()
        .success();
    let plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    repo.switch("pr-2");
    repo.commit_file("late.txt", "late\n", "late pr-2 work");

    validate_plan(&git, &plan).unwrap();
    validate_plan_for_apply(&git, &plan).unwrap();
}

/// The planned tip being reachable is not enough: if the branch was moved to
/// a merge that only carries it as a second parent, the "extra" commits are
/// foreign first-parent history and must not be replayed as the branch's own
/// work.
#[test]
fn apply_validation_rejects_branch_extended_through_second_parent() {
    let repo = linear_stack();
    repo.cascade()
        .args([
            "plan",
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
        .assert()
        .success();
    let plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    // side picks up pr-2 through a merge; pr-2 now points at that merge, so
    // its planned tip is reachable but only as a second parent.
    repo.switch_new_at("side", "main");
    repo.commit_file("side.txt", "side\n", "side work");
    repo.git_ok(["merge", "--no-ff", "pr-2", "-m", "merge pr-2"]);
    repo.git_ok(["branch", "-f", "pr-2", "side"]);
    repo.switch("main");

    validate_plan(&git, &plan).unwrap();
    let error = validate_plan_for_apply(&git, &plan)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("branch `pr-2` no longer extends planned tip"),
        "unexpected error: {error}"
    );
}

#[test]
fn validation_rejects_dependency_parent_mismatch() {
    let repo = linear_stack();
    repo.cascade()
        .args([
            "plan",
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
        .assert()
        .success();
    let mut plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    plan.dependencies[0].parent = "pr-3".into();

    let error = validate_plan(&git, &plan).unwrap_err().to_string();

    assert!(error.contains("is missing dependency edge"));
}

#[test]
fn validation_rejects_direct_child_at_anchor_base() {
    let repo = linear_stack();
    repo.cascade()
        .args([
            "plan",
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
        .assert()
        .success();
    let mut plan = read_plan(&repo, "stack");
    let git = Git::new(repo.path());

    let node = plan
        .nodes
        .iter_mut()
        .find(|node| node.branch.as_str() == "pr-2")
        .unwrap();
    assert_eq!(node.parent(), None);
    node.base = plan.source.base.clone();
    node.commits = linear_commits(&repo, node.base.as_str(), node.tip.as_str());

    let error = validate_plan(&git, &plan).unwrap_err().to_string();

    assert!(error.contains("is outside source range"));
}

fn linear_commits(repo: &TestRepo, base: &str, tip: &str) -> Vec<PlanCommit> {
    let mut previous = base.to_owned();
    repo.rev_list_reverse(&format!("{base}..{tip}"))
        .into_iter()
        .map(|oid| {
            let commit = PlanCommit::new(oid.clone(), vec![previous.clone()]);
            previous = oid;
            commit
        })
        .collect()
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
    let content = std::fs::read_to_string(repo.plan_path(name)).unwrap();
    Plan::from_yaml(&content).unwrap()
}
