mod common;

use predicates::prelude::*;

use common::repo::TestRepo;

#[test]
fn status_reports_no_active_operation() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");

    repo.cascade()
        .arg("status")
        .assert()
        .success()
        .stdout("No active cascade operation.\n");
}

#[test]
fn status_reports_conflict_state() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["apply", "--name", "stack", "--new-anchor", "pr-1"])
        .assert()
        .failure();

    repo.cascade().arg("status").assert().success().stdout(
        predicate::str::contains("Active cascade operation:")
            .and(predicate::str::contains("operation: apply"))
            .and(predicate::str::contains("phase: conflict"))
            .and(predicate::str::contains("plan: stack"))
            .and(predicate::str::contains("strategy: preserve-fork-points"))
            .and(predicate::str::contains("current-branch: pr-2"))
            .and(predicate::str::contains("current-commit:"))
            .and(predicate::str::contains("worktree:"))
            .and(predicate::str::contains("pending: pr-2")),
    );
}

#[test]
fn abort_cleans_conflict_state_without_moving_refs() {
    let repo = conflicting_stack();
    let old_pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["apply", "--name", "stack", "--new-anchor", "pr-1"])
        .assert()
        .failure();
    assert!(repo.common_dir().join("cascade/state.yaml").exists());

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
    repo.cascade()
        .arg("status")
        .assert()
        .success()
        .stdout("No active cascade operation.\n");
}

#[test]
fn abort_without_active_operation_fails_clearly() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");

    repo.cascade()
        .arg("abort")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no active cascade operation"));
}

fn conflicting_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("conflict.txt", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("conflict.txt", "anchor old\n", "anchor old");
    repo.switch_new("pr-2");
    repo.commit_file("conflict.txt", "dependent\n", "dependent");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "pr-1", "--name", "stack"])
        .assert()
        .success();
    repo.switch("pr-1");
    repo.commit_file("conflict.txt", "anchor new\n", "anchor new");
    repo.switch("main");
    repo
}
