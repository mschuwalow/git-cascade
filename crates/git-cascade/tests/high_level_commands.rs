mod common;

use common::repo::TestRepo;
use git_cascade::replay::{Phase, ReplayState};
use predicates::prelude::*;

#[test]
fn restack_current_branch_moves_dependents_to_current_parent_tips() {
    let repo = linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");
    repo.switch("pr-1");
    repo.commit_file("pr1-new.txt", "new\n", "new pr-1 work");

    repo.cascade()
        .arg("restack")
        .assert()
        .success()
        .stdout("restacked dependent branches\n")
        .stderr(
            predicate::str::contains("Applying cascade plan `generated/restack/pr-1/")
                .and(predicate::str::contains("move-to-current-tips")),
        );

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    assert_eq!(repo.merge_base("pr-1", "pr-2"), repo.rev_parse("pr-1"));
    assert_eq!(repo.merge_base("pr-2", "pr-3"), repo.rev_parse("pr-2"));
    assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
    assert!(
        repo.git_output(["for-each-ref", "refs/heads/generated"])
            .is_empty()
    );
}

#[test]
fn restack_dry_run_does_not_write_generated_plan_or_move_refs() {
    let repo = linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("pr-1");
    repo.commit_file("pr1-new.txt", "new\n", "new pr-1 work");
    repo.switch("main");

    repo.cascade()
        .args(["restack", "pr-1", "--dry-run"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("# git-cascade apply --dry-run")
                .and(predicate::str::contains("strategy move-to-current-tips")),
        );

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/plans").exists());
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn restack_base_override_uses_non_default_base_branch() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("develop");
    repo.commit_file("develop.txt", "develop\n", "develop work");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    let old_pr2 = repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch_new_at("develop-side", "develop");
    let old_develop_side = repo.commit_file("side.txt", "side\n", "develop-side work");
    repo.switch("pr-1");
    repo.commit_file("pr1-new.txt", "new\n", "new pr-1 work");
    repo.switch("main");

    repo.cascade()
        .args(["restack", "pr-1", "--base", "develop"])
        .assert()
        .success()
        .stdout("restacked dependent branches\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.rev_parse("develop-side"), old_develop_side);
    assert_eq!(repo.merge_base("pr-1", "pr-2"), repo.rev_parse("pr-1"));
}

#[test]
fn restack_conflict_keeps_generated_plan_for_continue() {
    let repo = TestRepo::new();
    repo.commit_file("conflict.txt", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("conflict.txt", "anchor old\n", "anchor old");
    repo.switch_new("pr-2");
    let old_pr2 = repo.commit_file("conflict.txt", "dependent\n", "dependent");
    repo.switch("pr-1");
    repo.commit_file("conflict.txt", "anchor new\n", "anchor new");
    repo.switch("main");

    repo.cascade()
        .args(["restack", "pr-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "stopped on conflict while replaying branch `pr-2`",
        ));

    let state_path = repo.common_dir().join("cascade/state.yaml");
    let state: ReplayState =
        serde_yaml::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    let plan_name = state.plan_name.clone();
    assert!(repo.plan_path(plan_name.as_str()).exists());

    let worktree = std::path::PathBuf::from(conflict_worktree(&state));
    std::fs::write(worktree.join("conflict.txt"), "resolved\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "conflict.txt"]);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.show("pr-2:conflict.txt"), "resolved\n");
    assert!(!state_path.exists());
    assert!(!repo.plan_path(plan_name.as_str()).exists());
}

#[test]
fn replay_moves_dependents_between_arbitrary_roots() {
    let repo = linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");
    repo.switch("main");
    repo.commit_file("replacement-base.txt", "base\n", "replacement base");
    repo.switch_new("replacement-root");
    repo.commit_file("replacement.txt", "replacement\n", "replacement root");
    repo.switch("main");

    repo.cascade()
        .args([
            "replay",
            "--old-base",
            "main~1",
            "--old-tip",
            "pr-1",
            "--new-tip",
            "replacement-root",
        ])
        .assert()
        .success()
        .stdout("replayed dependent branches\n")
        .stderr(
            predicate::str::contains("Applying cascade plan `generated/replay/pr-1/")
                .and(predicate::str::contains("move-to-current-tips")),
        );

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    assert_eq!(
        repo.merge_base("replacement-root", "pr-2"),
        repo.rev_parse("replacement-root")
    );
    assert_eq!(repo.merge_base("pr-2", "pr-3"), repo.rev_parse("pr-2"));
    assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn replay_dry_run_does_not_write_generated_plan_or_move_refs() {
    let repo = linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("main");
    repo.commit_file("replacement-base.txt", "base\n", "replacement base");
    repo.switch_new("replacement-root");
    repo.commit_file("replacement.txt", "replacement\n", "replacement root");
    repo.switch("main");

    repo.cascade()
        .args([
            "replay",
            "--old-base",
            "main~1",
            "--old-tip",
            "pr-1",
            "--new-tip",
            "replacement-root",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("# git-cascade apply --dry-run")
                .and(predicate::str::contains("strategy move-to-current-tips")),
        );

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/plans").exists());
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn sync_after_main_advances_moves_branches_to_current_main() {
    let repo = stack_on_non_root_main_tip();
    let old_pr1 = repo.rev_parse("pr-1");
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("main");
    repo.commit_file("main-new.txt", "new\n", "new main work");

    repo.cascade()
        .arg("sync")
        .assert()
        .success()
        .stdout("synced dependent branches\n")
        .stderr(
            predicate::str::contains("Applying cascade plan `generated/sync/main/")
                .and(predicate::str::contains("move-to-current-tips")),
        );

    assert_ne!(repo.rev_parse("pr-1"), old_pr1);
    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.merge_base("main", "pr-1"), repo.rev_parse("main"));
    assert_eq!(repo.merge_base("pr-1", "pr-2"), repo.rev_parse("pr-1"));
    assert_eq!(repo.show("pr-1:pr1.txt"), "a\n");
    assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn sync_dry_run_does_not_write_generated_plan_or_move_refs() {
    let repo = stack_on_non_root_main_tip();
    let old_pr1 = repo.rev_parse("pr-1");
    repo.switch("main");
    repo.commit_file("main-new.txt", "new\n", "new main work");

    repo.cascade()
        .args(["sync", "--dry-run"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("# git-cascade apply --dry-run")
                .and(predicate::str::contains("strategy move-to-current-tips")),
        );

    assert_eq!(repo.rev_parse("pr-1"), old_pr1);
    assert!(!repo.common_dir().join("cascade/plans").exists());
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn sync_infers_old_base_from_oldest_local_fork_point() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.commit_file("main-1.txt", "main 1\n", "main 1");
    repo.switch_new("pr-1");
    let old_pr1 = repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch("main");
    repo.commit_file("main-2.txt", "main 2\n", "main 2");
    repo.commit_file("main-3.txt", "main 3\n", "main 3");
    repo.switch_new("already-current");
    let already_current = repo.commit_file("current.txt", "current\n", "current work");
    repo.switch("main");

    repo.cascade().arg("sync").assert().success();

    assert_ne!(repo.rev_parse("pr-1"), old_pr1);
    assert_eq!(repo.merge_base("main", "pr-1"), repo.rev_parse("main"));
    assert_eq!(repo.show("pr-1:pr1.txt"), "a\n");
    assert_eq!(repo.rev_parse("already-current"), already_current);
}

#[test]
fn sync_oldest_branch_bounds_the_inferred_range() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.commit_file("main-1.txt", "main 1\n", "main 1");
    repo.switch_new("older-local");
    let older_local = repo.commit_file("older.txt", "older\n", "older local work");
    repo.switch("main");
    repo.commit_file("main-2.txt", "main 2\n", "main 2");
    repo.switch_new("pr-1");
    let old_pr1 = repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    let old_pr2 = repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");
    repo.commit_file("main-3.txt", "main 3\n", "main 3");

    repo.cascade()
        .args(["sync", "--oldest-branch", "pr-1"])
        .assert()
        .success()
        .stdout("synced dependent branches\n");

    assert_eq!(repo.rev_parse("older-local"), older_local);
    assert_ne!(repo.rev_parse("pr-1"), old_pr1);
    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.merge_base("main", "pr-1"), repo.rev_parse("main"));
    assert_eq!(repo.merge_base("pr-1", "pr-2"), repo.rev_parse("pr-1"));
}

#[test]
fn sync_is_idempotent_for_already_synced_stacks() {
    let repo = stack_on_non_root_main_tip();
    repo.switch("main");
    repo.commit_file("main-new.txt", "new\n", "new main work");

    repo.cascade().arg("sync").assert().success();
    let synced_pr1 = repo.rev_parse("pr-1");
    let synced_pr2 = repo.rev_parse("pr-2");

    // A different committer date would change cherry-picked commit ids, so
    // unchanged refs prove the second sync kept the branches instead of
    // rewriting them.
    repo.cascade()
        .arg("sync")
        .env("GIT_COMMITTER_DATE", "2026-02-02T00:00:00Z")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "already starts at its replay base",
        ));

    assert_eq!(repo.rev_parse("pr-1"), synced_pr1);
    assert_eq!(repo.rev_parse("pr-2"), synced_pr2);
}

#[test]
fn sync_uses_default_branch_even_when_current_branch_is_master() {
    let repo = stack_on_non_root_main_tip();
    let old_pr1 = repo.rev_parse("pr-1");
    repo.switch("main");
    repo.commit_file("main-new.txt", "new\n", "new main work");
    repo.switch_new_at("master", "main~1");

    repo.cascade()
        .arg("sync")
        .assert()
        .success()
        .stdout("synced dependent branches\n");

    assert_ne!(repo.rev_parse("pr-1"), old_pr1);
    assert_eq!(repo.merge_base("main", "pr-1"), repo.rev_parse("main"));
    assert_eq!(repo.rev_parse("master"), repo.rev_parse("main~1"));
}

#[test]
fn sync_base_override_syncs_to_non_default_base_branch() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("develop");
    repo.commit_file("develop-old.txt", "old\n", "old develop work");
    repo.switch_new("pr-1");
    let old_pr1 = repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch("develop");
    repo.commit_file("develop-new.txt", "new\n", "new develop work");
    repo.switch("main");

    repo.cascade()
        .args(["sync", "--base", "develop"])
        .assert()
        .success()
        .stdout("synced dependent branches\n");

    assert_ne!(repo.rev_parse("pr-1"), old_pr1);
    assert_eq!(
        repo.merge_base("develop", "pr-1"),
        repo.rev_parse("develop")
    );
    assert_eq!(repo.show("pr-1:pr1.txt"), "a\n");
}

#[test]
fn landed_squash_moves_dependents_onto_main() {
    let repo = linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("main");
    repo.git_ok(["merge", "--squash", "pr-1"]);
    repo.git_ok(["commit", "-m", "squash pr-1"]);

    repo.cascade()
        .args(["landed", "pr-1", "--onto", "main"])
        .assert()
        .success()
        .stdout("updated dependents of landed branch\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.merge_base("main", "pr-2"), repo.rev_parse("main"));
    assert_eq!(repo.merge_base("pr-2", "pr-3"), repo.rev_parse("pr-2"));
    assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
}

#[test]
fn landed_merge_commit_uses_landing_merge_as_new_root() {
    let repo = linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("main");
    repo.git_ok(["merge", "--no-ff", "pr-1", "-m", "merge pr-1"]);
    let merge_commit = repo.rev_parse("HEAD");
    repo.commit_file("later-main.txt", "later\n", "later main");

    repo.cascade()
        .args(["landed", "pr-1", "--onto", "main"])
        .assert()
        .success();

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.merge_base("main", "pr-2"), merge_commit);
    assert_ne!(repo.merge_base("main", "pr-2"), repo.rev_parse("main"));
    assert_eq!(repo.merge_base("pr-2", "pr-3"), repo.rev_parse("pr-2"));
}

#[test]
fn landed_merge_includes_landed_side_and_excludes_main_side_branches() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.commit_file("main.txt", "main\n", "main work");
    let main_before_merge = repo.rev_parse("main");
    repo.switch_new_at("pr-1", "main~1");
    repo.commit_file("pr1-a.txt", "a\n", "pr-1 a");
    let landed_side_base = repo.rev_parse("pr-1");
    repo.commit_file("pr1-b.txt", "b\n", "pr-1 b");
    repo.switch_new_at("landed-side", &landed_side_base);
    let old_landed_side = repo.commit_file("landed-side.txt", "child\n", "landed-side child");
    repo.switch_new_at("main-side", &main_before_merge);
    let old_main_side = repo.commit_file("main-side.txt", "unrelated\n", "main-side child");
    repo.switch("main");
    repo.git_ok(["merge", "--no-ff", "pr-1", "-m", "merge pr-1"]);

    repo.cascade()
        .args(["landed", "pr-1", "--onto", "main"])
        .assert()
        .success();

    assert_ne!(repo.rev_parse("landed-side"), old_landed_side);
    assert_eq!(
        repo.merge_base("main", "landed-side"),
        repo.rev_parse("main")
    );
    assert_eq!(repo.rev_parse("main-side"), old_main_side);
}

#[test]
fn landed_fast_forward_without_old_base_fails_clearly() {
    let repo = linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("main");
    repo.git_ok(["merge", "--ff-only", "pr-1"]);

    repo.cascade()
        .args(["landed", "pr-1", "--onto", "main"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("cannot infer old base")
                .and(predicate::str::contains("fast-forward"))
                .and(predicate::str::contains("--old-base")),
        );

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn landed_fast_forward_with_old_base_moves_dependents() {
    let repo = linear_stack();
    let old_base = repo.rev_parse("main");
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("main");
    repo.git_ok(["merge", "--ff-only", "pr-1"]);
    repo.commit_file("later-main.txt", "later\n", "later main");

    repo.cascade()
        .args(["landed", "pr-1", "--onto", "main", "--old-base", &old_base])
        .assert()
        .success();

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.merge_base("main", "pr-2"), repo.rev_parse("main"));
    assert_eq!(repo.merge_base("pr-2", "pr-3"), repo.rev_parse("pr-2"));
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

fn stack_on_non_root_main_tip() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.commit_file("main-old.txt", "old\n", "old main work");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");
    repo
}

fn conflict_worktree(state: &ReplayState) -> String {
    match &state.phase {
        Phase::Conflict { .. } => state.worktree.path().to_owned(),
        phase => panic!("expected conflict phase, got {phase:?}"),
    }
}
