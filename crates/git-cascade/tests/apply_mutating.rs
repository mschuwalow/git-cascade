mod common;

use predicates::prelude::*;

use common::repo::TestRepo;

#[test]
fn apply_linear_stack_updates_dependents_and_cleans_up() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");
    rewrite_anchor(&repo);

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success()
        .stdout("applied cascade plan\n")
        .stderr(
            predicate::str::contains("Applying cascade plan `stack`")
                .and(predicate::str::contains("Replaying branch 1/2 `pr-2`"))
                .and(predicate::str::contains("  cherry-pick 1/1"))
                .and(predicate::str::contains("Finished branch 2/2 `pr-3`"))
                .and(predicate::str::contains("Updating branch refs")),
        );

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
    repo.git_ok(["merge-base", "--is-ancestor", "pr-2", "pr-3"]);
    assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
    assert_eq!(repo.show("pr-3:pr3.txt"), "c\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn apply_preserves_intermediate_fork_point() {
    let repo = intermediate_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    rewrite_anchor(&repo);

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    let rewritten_pr2_commits = repo.rev_list_reverse("pr-1..pr-2");
    let merge_base = repo.merge_base("pr-2", "pr-3");

    assert_eq!(merge_base, rewritten_pr2_commits[0]);
}

#[test]
fn apply_move_to_current_tips_replays_child_on_parent_tip() {
    let repo = intermediate_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
        ])
        .assert()
        .success();

    assert_eq!(repo.merge_base("pr-2", "pr-3"), repo.rev_parse("pr-2"));
}

#[test]
fn apply_move_to_planned_tips_ignores_parent_commits_added_after_planning() {
    let repo = intermediate_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    repo.switch("pr-2");
    repo.commit_file("late-parent.txt", "late\n", "late parent work");
    repo.switch("main");
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-planned-tips",
        ])
        .assert()
        .success();

    let rewritten_pr2_commits = repo.rev_list_reverse("pr-1..pr-2");
    assert_eq!(repo.merge_base("pr-2", "pr-3"), rewritten_pr2_commits[1]);
    assert_ne!(repo.merge_base("pr-2", "pr-3"), repo.rev_parse("pr-2"));
    assert_eq!(repo.show("pr-2:late-parent.txt"), "late\n");
}

#[test]
fn apply_move_to_current_tips_uses_parent_commits_added_after_planning() {
    let repo = intermediate_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    repo.switch("pr-2");
    repo.commit_file("late-parent.txt", "late\n", "late parent work");
    repo.switch("main");
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
        ])
        .assert()
        .success();

    assert_eq!(repo.merge_base("pr-2", "pr-3"), repo.rev_parse("pr-2"));
    assert_eq!(repo.show("pr-2:late-parent.txt"), "late\n");
}

#[test]
fn apply_preserve_fork_points_keeps_added_dependent_commits() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    repo.switch("pr-2");
    let old_late = repo.commit_file("late.txt", "late\n", "late dependent work");
    repo.switch("main");
    rewrite_anchor(&repo);

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    assert_ne!(repo.rev_parse("pr-2"), old_late);
    assert_eq!(repo.show("pr-2:late.txt"), "late\n");
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
}

#[test]
fn apply_move_to_current_tips_keeps_added_dependent_commits() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    repo.switch("pr-2");
    let old_late = repo.commit_file("late.txt", "late\n", "late dependent work");
    repo.switch("main");
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
        ])
        .assert()
        .success();

    assert_ne!(repo.rev_parse("pr-2"), old_late);
    assert_eq!(repo.show("pr-2:late.txt"), "late\n");
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
}

#[test]
fn apply_refuses_when_state_exists() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    rewrite_anchor(&repo);
    let state_path = repo.common_dir().join("cascade/state.yaml");
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    std::fs::write(&state_path, "version: 1\n").unwrap();
    let pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("state file exists"));

    assert_eq!(repo.rev_parse("pr-2"), pr2);
}

#[test]
fn apply_refuses_when_target_branch_is_checked_out() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    rewrite_anchor(&repo);
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("pr-2");

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("target branch(es) are checked out")
                .and(predicate::str::contains("pr-2")),
        );

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn apply_refuses_when_dependent_branch_moved() {
    let repo = linear_stack();
    repo.cascade()
        .args(["plan", "stack", "--old-base", "main", "--old-tip", "pr-1"])
        .assert()
        .success();
    rewrite_anchor(&repo);
    repo.switch("pr-2");
    repo.git_ok(["reset", "--hard", "HEAD^"]);
    repo.commit_file("replacement.txt", "replacement\n", "replacement");
    repo.switch("main");

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "branch `pr-2` rewrote planned commits after plan generation",
        ));
}

#[test]
fn apply_conflict_leaves_permanent_refs_unchanged_and_state_present() {
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
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("pr-1");
    repo.commit_file("conflict.txt", "anchor new\n", "anchor new");
    repo.switch("main");

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("apply stopped while replaying branch `pr-2`")
                .and(predicate::str::contains("git cascade continue"))
                .and(predicate::str::contains("Do not run")),
        );

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn apply_uses_arbitrary_old_tip_ref() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.git_ok(["tag", "old-anchor"]);
    repo.switch_new("pr-2");
    let old_pr2 = repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");

    repo.cascade()
        .args([
            "plan",
            "stack",
            "--old-base",
            "main",
            "--old-tip",
            "refs/tags/old-anchor",
        ])
        .assert()
        .success();
    rewrite_anchor(&repo);

    repo.cascade()
        .args(["apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
    assert!(!repo.plan_path("stack").exists());
}

#[test]
fn apply_single_commit_root_range_updates_dependent() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "old\n", "old single root");
    repo.switch_new("pr-2");
    let old_pr2 = repo.commit_file("pr2.txt", "dependent\n", "dependent");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "single", "--old-base", "pr-1^", "--old-tip", "pr-1"])
        .assert()
        .success();
    rewrite_anchor(&repo);

    repo.cascade()
        .args(["apply", "single", "--new-tip", "pr-1"])
        .assert()
        .success();

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
    assert_eq!(repo.show("pr-2:pr2.txt"), "dependent\n");
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

fn intermediate_stack() -> TestRepo {
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
    repo
}

fn rewrite_anchor(repo: &TestRepo) {
    repo.switch("main");
    repo.commit_file("main2.txt", "new base\n", "new base");
    repo.switch("pr-1");
    repo.git_ok(["rebase", "main"]);
    repo.switch("main");
}
