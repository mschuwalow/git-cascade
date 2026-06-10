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
