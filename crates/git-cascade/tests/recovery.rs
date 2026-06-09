mod common;

use predicates::prelude::*;

use common::repo::TestRepo;
use git_cascade::state::{Phase, RestoreState, WorktreeState};

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
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    assert_eq!(
        worktree.parent().unwrap(),
        repo.common_dir().join("cascade/worktrees")
    );
    assert_eq!(
        worktree.file_name().unwrap().to_str().unwrap(),
        state.plan_id.to_string()
    );
    assert!(worktree.exists());

    repo.cascade().arg("status").assert().success().stdout(
        predicate::str::contains("Active cascade operation:")
            .and(predicate::str::contains("phase: conflict"))
            .and(predicate::str::contains("plan: stack"))
            .and(predicate::str::contains("new-tip:"))
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
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let state = read_state(&repo);
    repo.git_ok([
        "update-ref",
        &format!("refs/cascade/tmp/{}/extra", state.plan_id),
        "HEAD",
    ]);
    assert!(repo.common_dir().join("cascade/state.yaml").exists());

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
    repo.cascade()
        .arg("status")
        .assert()
        .success()
        .stdout("No active cascade operation.\n");
}

#[test]
fn abort_in_place_conflict_restores_original_checkout() {
    let repo = conflicting_stack();
    let old_pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1", "--in-place"])
        .assert()
        .failure();
    let state = read_state(&repo);
    assert!(matches!(
        &state.worktree,
        WorktreeState::InPlace {
            restore: RestoreState::Branch { name, .. },
            ..
        } if name == "main"
    ));
    assert_eq!(std::path::Path::new(state.worktree.path()), repo.path());
    assert_eq!(repo.git_output(["branch", "--show-current"]).trim(), "");
    assert!(repo.path().join("conflict.txt").exists());

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");

    assert_eq!(repo.git_output(["branch", "--show-current"]).trim(), "main");
    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(repo.git_output(["status", "--porcelain"]).is_empty());
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn abort_succeeds_when_recorded_worktree_was_already_deleted() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let state = read_state(&repo);
    std::fs::remove_dir_all(state.worktree.path()).unwrap();

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");

    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn abort_succeeds_when_plan_was_already_deleted() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let mut state = read_state(&repo);
    state.phase = Phase::Deleting;
    write_state(&repo, &state);
    std::fs::remove_file(repo.plan_path("stack")).unwrap();

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");

    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!std::path::Path::new(state.worktree.path()).exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn status_reports_deleting_state_without_cleanup() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let mut state = read_state(&repo);
    state.phase = Phase::Deleting;
    write_state(&repo, &state);

    repo.cascade().arg("status").assert().success().stdout(
        predicate::str::contains("Active cascade operation:")
            .and(predicate::str::contains("phase: deleting"))
            .and(predicate::str::contains("plan: stack")),
    );

    assert!(repo.common_dir().join("cascade/state.yaml").exists());
    assert!(std::path::Path::new(state.worktree.path()).exists());
}

#[test]
fn continue_finishes_deleting_state() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let mut state = read_state(&repo);
    state.phase = Phase::Deleting;
    write_state(&repo, &state);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!std::path::Path::new(state.worktree.path()).exists());
    assert!(repo.plan_path("stack").exists());
}

#[test]
fn abort_finishes_cleanup_for_deleting_state() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let mut state = read_state(&repo);
    state.phase = Phase::Deleting;
    write_state(&repo, &state);

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");

    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!std::path::Path::new(state.worktree.path()).exists());
    assert!(repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
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

#[test]
fn continue_without_active_operation_fails_clearly() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");

    repo.cascade()
        .arg("continue")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no active cascade operation"));
}

#[test]
fn continue_refuses_unresolved_conflicts() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();

    repo.cascade()
        .arg("continue")
        .assert()
        .failure()
        .stderr(predicate::str::contains("still has unresolved conflicts"));
    assert!(repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn continue_after_conflict_finishes_apply() {
    let repo = conflicting_stack();
    let old_pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.current.unwrap().worktree);
    std::fs::write(worktree.join("conflict.txt"), "resolved\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "conflict.txt"]);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.show("pr-2:conflict.txt"), "resolved\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn continue_can_stop_again_on_later_conflict() {
    let repo = repeated_conflict_stack();
    let old_pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let first_state = read_state(&repo);
    let first_conflict = first_state.current.unwrap().commit;
    let worktree = std::path::PathBuf::from(first_state.worktree.path());
    std::fs::write(worktree.join("a.txt"), "resolved a\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "a.txt"]);

    repo.cascade().arg("continue").assert().failure().stderr(
        predicate::str::contains("apply stopped while replaying branch `pr-2`")
            .and(predicate::str::contains("git cascade continue"))
            .and(predicate::str::contains("Do not run")),
    );

    let second_state = read_state(&repo);
    assert_eq!(second_state.phase, Phase::Conflict);
    let second_conflict = second_state.current.unwrap().commit;
    assert_ne!(second_conflict, first_conflict);
    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
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
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    repo.switch("pr-1");
    repo.commit_file("conflict.txt", "anchor new\n", "anchor new");
    repo.switch("main");
    repo
}

fn repeated_conflict_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("a.txt", "base a\n", "initial a");
    repo.commit_file("b.txt", "base b\n", "initial b");
    repo.switch_new("pr-1");
    repo.write("a.txt", "anchor old a\n");
    repo.write("b.txt", "anchor old b\n");
    repo.git_ok(["add", "a.txt", "b.txt"]);
    repo.git_ok(["commit", "-m", "anchor old"]);
    repo.switch_new("pr-2");
    repo.commit_file("a.txt", "dependent a\n", "dependent a");
    repo.commit_file("b.txt", "dependent b\n", "dependent b");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    repo.switch("pr-1");
    repo.write("a.txt", "anchor new a\n");
    repo.write("b.txt", "anchor new b\n");
    repo.git_ok(["add", "a.txt", "b.txt"]);
    repo.git_ok(["commit", "-m", "anchor new"]);
    repo.switch("main");
    repo
}

fn read_state(repo: &TestRepo) -> git_cascade::state::ApplyState {
    let content = std::fs::read_to_string(repo.common_dir().join("cascade/state.yaml")).unwrap();
    serde_yaml::from_str(&content).unwrap()
}

fn write_state(repo: &TestRepo, state: &git_cascade::state::ApplyState) {
    let content = serde_yaml::to_string(state).unwrap();
    std::fs::write(repo.common_dir().join("cascade/state.yaml"), content).unwrap();
}
