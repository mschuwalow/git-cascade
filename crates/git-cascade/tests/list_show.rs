mod common;

use common::repo::TestRepo;
use git_cascade::plan::PlanName;
use predicates::prelude::*;

#[test]
fn list_reads_named_plans_from_git_common_dir() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "hello\n", "initial");
    let plans_dir = repo.common_dir().join("cascade/plans");
    std::fs::create_dir_all(&plans_dir).unwrap();
    std::fs::write(
        plans_dir.join(format!("{}.yaml", PlanName::new("beta").unwrap().encoded())),
        "version: 1\n",
    )
    .unwrap();
    std::fs::write(
        plans_dir.join(format!(
            "{}.yaml",
            PlanName::new("alpha").unwrap().encoded()
        )),
        "version: 1\n",
    )
    .unwrap();
    std::fs::write(plans_dir.join("ignore.txt"), "not a plan\n").unwrap();

    repo.cascade()
        .args(["plan", "list"])
        .assert()
        .success()
        .stdout("alpha\nbeta\n");
}

#[test]
fn show_prints_a_named_plan() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "hello\n", "initial");
    let plans_dir = repo.common_dir().join("cascade/plans");
    std::fs::create_dir_all(&plans_dir).unwrap();
    std::fs::write(
        plans_dir.join(format!(
            "{}.yaml",
            PlanName::new("stack").unwrap().encoded()
        )),
        "version: 1\nplan_id: test\n",
    )
    .unwrap();

    repo.cascade()
        .args(["plan", "show", "stack"])
        .assert()
        .success()
        .stdout("version: 1\nplan_id: test\n");
}

#[test]
fn show_rejects_empty_name() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "hello\n", "initial");

    repo.cascade()
        .args(["plan", "show", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid plan name"));
}

#[test]
fn plan_names_can_contain_path_separators() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("feature/stack");
    repo.commit_file("feature.txt", "feature\n", "feature");

    repo.cascade()
        .args([
            "plan",
            "create",
            "feature/stack",
            "--old-base",
            "main",
            "--old-tip",
            "feature/stack",
        ])
        .assert()
        .success();

    assert!(repo.plan_path("feature/stack").exists());
}

#[test]
fn remove_deletes_a_named_plan() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch("main");

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
    assert!(repo.plan_path("stack").exists());

    repo.cascade()
        .args(["plan", "remove", "stack"])
        .assert()
        .success()
        .stdout("removed plan `stack`\n");

    assert!(!repo.plan_path("stack").exists());
    repo.cascade()
        .args(["plan", "list"])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn remove_unknown_plan_fails_clearly() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");

    repo.cascade()
        .args(["plan", "remove", "missing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("plan `missing` does not exist"));
}

#[test]
fn remove_refuses_plan_referenced_by_active_operation() {
    let repo = TestRepo::new();
    repo.commit_file("conflict.txt", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("conflict.txt", "anchor old\n", "anchor old");
    repo.switch_new("pr-2");
    repo.commit_file("conflict.txt", "dependent\n", "dependent");
    repo.switch("main");

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
    repo.switch("pr-1");
    repo.commit_file("conflict.txt", "anchor new\n", "anchor new");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    repo.cascade()
        .args(["plan", "remove", "stack"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "referenced by the active cascade operation",
        ));

    assert!(repo.plan_path("stack").exists());
}
