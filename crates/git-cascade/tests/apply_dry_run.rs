mod common;

use common::repo::TestRepo;
use predicates::prelude::*;

#[test]
fn apply_dry_run_linear_stack_prints_commands_without_mutating_refs() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    let pr2_commit = repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch_new("pr-3");
    let pr3_commit = repo.commit_file("pr3.txt", "c\n", "pr-3");
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
    rewrite_anchor(&repo);
    let pr2_tip = repo.rev_parse("pr-2");
    let pr3_tip = repo.rev_parse("pr-3");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1", "--dry-run"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("# git-cascade apply --dry-run")
                .and(predicate::str::contains("strategy move-to-current-tips"))
                .and(predicate::str::contains("# branch pr-2"))
                .and(
                    predicate::str::contains("git -C ").and(predicate::str::contains(format!(
                        "cherry-pick {pr2_commit}"
                    ))),
                )
                .and(predicate::str::contains("git update-ref refs/cascade/tmp/"))
                .and(predicate::str::contains("# branch pr-3"))
                .and(predicate::str::contains(format!(
                    "cherry-pick {pr3_commit}"
                )))
                .and(predicate::str::contains("git update-ref --stdin <<'EOF'"))
                .and(predicate::str::contains(format!(
                    "update refs/heads/pr-2 <rewritten pr-2 tip> {pr2_tip}"
                )))
                .and(predicate::str::contains(format!(
                    "update refs/heads/pr-3 <rewritten pr-3 tip> {pr3_tip}"
                ))),
        );

    assert_eq!(repo.rev_parse("pr-2"), pr2_tip);
    assert_eq!(repo.rev_parse("pr-3"), pr3_tip);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn apply_dry_run_strategy_changes_dependent_base_descriptions() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    let pr2_first = repo.commit_file("pr2-a.txt", "b\n", "pr-2 a");
    repo.commit_file("pr2-b.txt", "c\n", "pr-2 b");
    repo.switch_new_at("pr-3", &pr2_first);
    repo.commit_file("pr3.txt", "d\n", "pr-3");
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
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "preserve-fork-points",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "replay-base <rewritten pr-2:{pr2_first}>"
        )));

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-planned-tips",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "replay-base <rewritten pr-2 planned tip>",
        ));

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "replay-base <rewritten pr-2 current tip>",
        ));
}

#[test]
fn apply_dry_run_refuses_if_dependent_branch_moved() {
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
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
        .assert()
        .success();
    repo.switch("pr-2");
    repo.git_ok(["reset", "--hard", "HEAD^"]);
    repo.commit_file("replacement.txt", "replacement\n", "replacement");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "branch `pr-2` rewrote planned commits after plan generation",
        ));
}

#[test]
fn apply_dry_run_includes_added_dependent_commits() {
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
            "create",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "pr-1",
        ])
        .assert()
        .success();
    rewrite_anchor(&repo);
    repo.switch("pr-2");
    let late_commit = repo.commit_file("late.txt", "late\n", "late");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "cherry-pick {late_commit}"
        )));
}

#[test]
fn apply_dry_run_in_place_includes_restore_checkout() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    let old_pr2 = repo.commit_file("pr2.txt", "b\n", "pr-2");
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
    repo.switch("main");
    repo.commit_file("main2.txt", "new base\n", "new base");
    repo.switch("pr-1");
    repo.git_ok(["rebase", "main"]);
    repo.switch("pr-2");

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--dry-run",
            "--in-place",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("worktree-mode in-place")
                .and(predicate::str::contains(format!(
                    "git -C {} switch --detach",
                    repo.path().display()
                )))
                .and(predicate::str::contains("# restore checkout"))
                .and(predicate::str::contains(format!(
                    "git -C {} switch pr-2",
                    repo.path().display()
                ))),
        );

    assert_eq!(repo.git_output(["branch", "--show-current"]).trim(), "pr-2");
    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn apply_without_dry_run_with_no_dependents_is_a_safe_noop() {
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

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success()
        .stdout("applied cascade plan\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn apply_dry_run_skips_branches_already_at_replay_base() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    let pr2_tip = repo.commit_file("pr2.txt", "b\n", "pr-2");
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

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1", "--dry-run"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains(format!(
                "already starts at its replay base; keeping {pr2_tip}"
            ))
            .and(predicate::str::contains("cherry-pick").not())
            .and(predicate::str::contains(format!(
                "update refs/heads/pr-2 {pr2_tip} {pr2_tip}"
            ))),
        );

    assert_eq!(repo.rev_parse("pr-2"), pr2_tip);
}

fn rewrite_anchor(repo: &TestRepo) {
    repo.switch("main");
    repo.commit_file("main2.txt", "new base\n", "new base");
    repo.switch("pr-1");
    repo.git_ok(["rebase", "main"]);
    repo.switch("main");
}
