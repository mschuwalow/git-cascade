mod common;

use common::repo::TestRepo;
use git_cascade::replay::{
    CurrentState, PausedState, Phase, ReplayMode, ReplayState, RestoreState, WorktreeState,
};
use predicates::prelude::*;

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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();
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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();
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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1", "--in-place"])
        .assert()
        .success();
    let state = read_state(&repo);
    assert!(matches!(
        &state.worktree,
        WorktreeState::InPlace {
            restore: RestoreState::Branch { name, .. },
            ..
        } if name.as_str() == "main"
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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();
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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();
    let mut state = read_state(&repo);
    state.phase = deleting_phase();
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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();
    let mut state = read_state(&repo);
    state.phase = deleting_phase();
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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();
    let mut state = read_state(&repo);
    state.phase = deleting_phase();
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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();
    let mut state = read_state(&repo);
    state.phase = deleting_phase();
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
fn pause_at_checkpoints_allows_fix_before_replaying_child() {
    let repo = paused_linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    let state = read_state(&repo);
    assert!(matches!(state.phase, Phase::Paused { .. }));
    assert_eq!(state.replay_mode, ReplayMode::PauseAtCheckpoints);
    assert_eq!(paused_state(&state).branch(), "pr-2");
    assert_eq!(pending_branch_names(&state), vec!["pr-3"]);
    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.rev_parse("pr-3"), old_pr3);

    let worktree = std::path::PathBuf::from(state.worktree.path());
    std::fs::write(worktree.join("fix.txt"), "fix\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "fix.txt"]);
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "commit",
        "-m",
        "fix pr-2 after replay",
    ]);
    let fixed_pr2 = repo
        .git_output(["-C", worktree.to_str().unwrap(), "rev-parse", "HEAD"])
        .trim()
        .to_owned();

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-3`"));

    let state = read_state(&repo);
    assert!(matches!(state.phase, Phase::Paused { .. }));
    assert_eq!(paused_state(&state).branch(), "pr-3");
    assert!(state.pending_branches.is_empty());
    assert_eq!(
        repo.git_output(["-C", worktree.to_str().unwrap(), "show", "HEAD:fix.txt"]),
        "fix\n"
    );

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_eq!(repo.rev_parse("pr-2"), fixed_pr2);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    repo.git_ok(["merge-base", "--is-ancestor", "pr-2", "pr-3"]);
    assert_eq!(repo.show("pr-3:fix.txt"), "fix\n");
    assert_eq!(repo.show("pr-3:pr3.txt"), "c\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn branch_end_pause_rejects_rewrite_that_drops_branch_replay_base() {
    let repo = paused_linear_stack();
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    repo.git_ok(["-C", worktree.to_str().unwrap(), "reset", "--hard", "main"]);

    repo.cascade().arg("continue").assert().failure().stderr(
        predicate::str::contains("does not preserve")
            .and(predicate::str::contains("replay base for branch `pr-2`")),
    );

    let state = read_state(&repo);
    assert!(matches!(
        paused_state(&state),
        PausedState::BranchEnd { .. }
    ));
    assert_eq!(pending_branch_names(&state), vec!["pr-3"]);
}

#[test]
fn branch_end_pause_allows_squashing_before_replaying_child_to_current_tip() {
    let repo = paused_multi_commit_branch_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    let state = read_state(&repo);
    let PausedState::BranchEnd { rewritten_tip, .. } = paused_state(&state) else {
        panic!("expected branch-end pause");
    };
    let rewritten_tip = rewritten_tip.clone();
    let worktree = std::path::PathBuf::from(state.worktree.path());
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "reset",
        "--soft",
        "HEAD~2",
    ]);
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "commit",
        "-m",
        "squash pr-2",
    ]);
    repo.git_fails([
        "-C",
        worktree.to_str().unwrap(),
        "merge-base",
        "--is-ancestor",
        rewritten_tip.as_str(),
        "HEAD",
    ]);
    let squashed_pr2 = repo
        .git_output(["-C", worktree.to_str().unwrap(), "rev-parse", "HEAD"])
        .trim()
        .to_owned();

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-3`"));

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    assert_eq!(repo.rev_parse("pr-2"), squashed_pr2);
    assert_eq!(repo.merge_base("pr-2", "pr-3"), squashed_pr2);
    assert_eq!(repo.show("pr-2:pr2-a.txt"), "b\n");
    assert_eq!(repo.show("pr-2:pr2-b.txt"), "c\n");
    assert_eq!(repo.show("pr-3:pr3.txt"), "d\n");
}

#[test]
fn pause_at_checkpoints_pauses_unchanged_branches_without_rewriting_if_unchanged() {
    let repo = paused_linear_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    let state = read_state(&repo);
    assert_eq!(paused_state(&state).branch(), "pr-2");
    assert_eq!(pending_branch_names(&state), vec!["pr-3"]);
    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.rev_parse("pr-3"), old_pr3);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-3`"));

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.rev_parse("pr-3"), old_pr3);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn pause_at_checkpoints_walks_unchanged_child_base_before_branch_end() {
    let repo = paused_intermediate_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused at child base"));

    let state = read_state(&repo);
    assert_eq!(paused_state(&state).branch(), "pr-2");
    assert!(matches!(
        paused_state(&state),
        PausedState::ChildBase { .. }
    ));
    assert_eq!(pending_branch_names(&state), vec!["pr-2", "pr-3"]);
    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.rev_parse("pr-3"), old_pr3);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    let state = read_state(&repo);
    assert!(matches!(
        paused_state(&state),
        PausedState::BranchEnd { .. }
    ));
    assert_eq!(pending_branch_names(&state), vec!["pr-3"]);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-3`"));

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.rev_parse("pr-3"), old_pr3);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn unchanged_child_base_pause_allows_fix_before_remaining_branch() {
    let repo = paused_intermediate_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused at child base"));

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    std::fs::write(worktree.join("base-fix.txt"), "base fix\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "base-fix.txt"]);
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "commit",
        "-m",
        "fix unchanged child base",
    ]);
    let fixed_child_base = repo
        .git_output(["-C", worktree.to_str().unwrap(), "rev-parse", "HEAD"])
        .trim()
        .to_owned();

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-3`"));

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    assert_eq!(repo.merge_base("pr-2", "pr-3"), fixed_child_base);
    assert_eq!(repo.show("pr-2:base-fix.txt"), "base fix\n");
    assert_eq!(repo.show("pr-3:base-fix.txt"), "base fix\n");
    assert_eq!(repo.show("pr-3:pr3.txt"), "d\n");
}

#[test]
fn pause_at_checkpoints_allows_rewriting_unchanged_branch_before_child() {
    let repo = paused_multi_commit_branch_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "reset",
        "--soft",
        "HEAD~2",
    ]);
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "commit",
        "-m",
        "squash pr-2",
    ]);
    let squashed_pr2 = repo
        .git_output(["-C", worktree.to_str().unwrap(), "rev-parse", "HEAD"])
        .trim()
        .to_owned();

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-3`"));

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    assert_eq!(repo.rev_parse("pr-2"), squashed_pr2);
    assert_eq!(repo.merge_base("pr-2", "pr-3"), squashed_pr2);
    assert_eq!(repo.show("pr-2:pr2-a.txt"), "b\n");
    assert_eq!(repo.show("pr-2:pr2-b.txt"), "c\n");
    assert_eq!(repo.show("pr-3:pr3.txt"), "d\n");
}

#[test]
fn continue_refuses_dirty_paused_worktree() {
    let repo = paused_linear_stack();
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success();
    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    std::fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();

    repo.cascade().arg("continue").assert().failure().stderr(
        predicate::str::contains("paused worktree")
            .and(predicate::str::contains("has uncommitted changes")),
    );

    let state = read_state(&repo);
    assert!(matches!(state.phase, Phase::Paused { .. }));
    assert_eq!(paused_state(&state).branch(), "pr-2");
}

#[test]
fn abort_in_place_paused_replay_discards_dirty_worktree() {
    let repo = paused_linear_stack();
    rewrite_anchor(&repo);
    repo.switch("pr-2");

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--pause-at-checkpoints",
            "--in-place",
        ])
        .assert()
        .success();
    repo.write("pr2.txt", "dirty\n");

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");

    assert_eq!(repo.git_output(["branch", "--show-current"]).trim(), "pr-2");
    assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn pause_at_checkpoints_stops_at_child_base_before_branch_end() {
    let repo = paused_intermediate_stack();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused at child base"));

    let state = read_state(&repo);
    let first_pause = paused_state(&state);
    assert_eq!(first_pause.branch(), "pr-2");
    assert!(matches!(first_pause, PausedState::ChildBase { .. }));
    assert_eq!(pending_branch_names(&state), vec!["pr-2", "pr-3"]);
    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.rev_parse("pr-3"), old_pr3);

    let worktree = std::path::PathBuf::from(state.worktree.path());
    std::fs::write(worktree.join("base-fix.txt"), "base fix\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "base-fix.txt"]);
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "commit",
        "-m",
        "fix child base",
    ]);
    let fixed_child_base = repo
        .git_output(["-C", worktree.to_str().unwrap(), "rev-parse", "HEAD"])
        .trim()
        .to_owned();

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    let state = read_state(&repo);
    assert!(matches!(
        paused_state(&state),
        PausedState::BranchEnd { .. }
    ));
    assert_eq!(pending_branch_names(&state), vec!["pr-3"]);
    std::fs::write(worktree.join("tip-fix.txt"), "tip fix\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "tip-fix.txt"]);
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "commit",
        "-m",
        "fix branch tip",
    ]);
    let fixed_pr2_tip = repo
        .git_output(["-C", worktree.to_str().unwrap(), "rev-parse", "HEAD"])
        .trim()
        .to_owned();

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-3`"));

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_eq!(repo.rev_parse("pr-2"), fixed_pr2_tip);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    assert_eq!(repo.merge_base("pr-2", "pr-3"), fixed_child_base);
    assert_eq!(repo.show("pr-3:base-fix.txt"), "base fix\n");
    repo.git_fails(["show", "pr-3:tip-fix.txt"]);
    assert_eq!(repo.show("pr-2:tip-fix.txt"), "tip fix\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
}

#[test]
fn branch_end_pause_rejects_squashing_preserved_child_replay_base() {
    let repo = paused_intermediate_stack();
    rewrite_anchor(&repo);

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--pause-at-checkpoints",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("paused at child base"));

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains("paused after branch `pr-2`"));

    let state = read_state(&repo);
    assert!(matches!(
        paused_state(&state),
        PausedState::BranchEnd { .. }
    ));
    let worktree = std::path::PathBuf::from(state.worktree.path());
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "reset",
        "--soft",
        "HEAD~2",
    ]);
    repo.git_ok([
        "-C",
        worktree.to_str().unwrap(),
        "commit",
        "-m",
        "squash pr-2",
    ]);

    repo.cascade().arg("continue").assert().failure().stderr(
        predicate::str::contains("does not preserve").and(predicate::str::contains(
            "replay base for child branch `pr-3`",
        )),
    );

    let state = read_state(&repo);
    assert!(matches!(
        paused_state(&state),
        PausedState::BranchEnd { .. }
    ));
    assert_eq!(pending_branch_names(&state), vec!["pr-3"]);
}

#[test]
fn continue_refuses_unresolved_conflicts() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(conflict_current(&state).worktree);
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
fn continue_after_conflict_continues_to_child_branch() {
    let repo = conflicting_stack_with_child();
    let old_pr2 = repo.rev_parse("pr-2");
    let old_pr3 = repo.rev_parse("pr-3");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(conflict_current(&state).worktree);
    std::fs::write(worktree.join("conflict.txt"), "resolved\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "conflict.txt"]);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_ne!(repo.rev_parse("pr-3"), old_pr3);
    assert_eq!(repo.show("pr-3:conflict.txt"), "resolved\n");
    assert_eq!(repo.show("pr-3:pr3.txt"), "child\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
}

#[test]
fn continue_resumes_replay_phase_after_crash() {
    let repo = conflicting_stack();
    let old_pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    // Simulate a crash mid-replay: the persisted phase is `replay` with no
    // current commit, and the worktree still has a cherry-pick in progress.
    let mut state = read_state(&repo);
    state.phase = Phase::Replay { current: None };
    state.mappings.clear();
    write_state(&repo, &state);

    // Continue restarts the branch from scratch and stops on the same
    // conflict again.
    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "stopped on conflict while replaying branch `pr-2`",
        ));

    let state = read_state(&repo);
    assert!(matches!(state.phase, Phase::Conflict { .. }));
    let worktree = std::path::PathBuf::from(conflict_current(&state).worktree);
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
}

#[test]
fn continue_rejects_tampered_plan_but_abort_recovers() {
    let repo = conflicting_stack();

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    // Tamper with the stored plan: duplicate a planned commit.
    let plan_path = repo.plan_path("stack");
    let mut plan =
        git_cascade::plan::Plan::from_yaml(&std::fs::read_to_string(&plan_path).unwrap()).unwrap();
    let commit = plan.nodes[0].commits[0].clone();
    plan.nodes[0].commits.push(commit);
    std::fs::write(&plan_path, serde_yaml::to_string(&plan).unwrap()).unwrap();

    repo.cascade()
        .arg("continue")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid plan"));
    assert!(repo.common_dir().join("cascade/state.yaml").exists());

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

#[test]
fn continue_can_stop_again_on_later_conflict() {
    let repo = repeated_conflict_stack();
    let old_pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();
    let first_state = read_state(&repo);
    let first_conflict = conflict_current(&first_state).commit;
    let worktree = std::path::PathBuf::from(first_state.worktree.path());
    std::fs::write(worktree.join("a.txt"), "resolved a\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "a.txt"]);

    repo.cascade().arg("continue").assert().success().stdout(
        predicate::str::contains("stopped on conflict while replaying branch `pr-2`")
            .and(predicate::str::contains("git cascade continue")),
    );

    let second_state = read_state(&repo);
    assert!(matches!(second_state.phase, Phase::Conflict { .. }));
    let second_conflict = conflict_current(&second_state).commit;
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
    repo
}

fn conflicting_stack_with_child() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("conflict.txt", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("conflict.txt", "anchor old\n", "anchor old");
    repo.switch_new("pr-2");
    repo.commit_file("conflict.txt", "dependent\n", "dependent");
    repo.switch_new("pr-3");
    repo.commit_file("pr3.txt", "child\n", "child");
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
    repo.write("a.txt", "anchor new a\n");
    repo.write("b.txt", "anchor new b\n");
    repo.git_ok(["add", "a.txt", "b.txt"]);
    repo.git_ok(["commit", "-m", "anchor new"]);
    repo.switch("main");
    repo
}

fn paused_linear_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch_new("pr-3");
    repo.commit_file("pr3.txt", "c\n", "pr-3");
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
    repo
}

fn paused_intermediate_stack() -> TestRepo {
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
    repo
}

fn paused_multi_commit_branch_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2-a.txt", "b\n", "pr-2 a");
    repo.commit_file("pr2-b.txt", "c\n", "pr-2 b");
    repo.switch_new("pr-3");
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
    repo
}

fn rewrite_anchor(repo: &TestRepo) {
    repo.switch("main");
    repo.commit_file("main2.txt", "new base\n", "new base");
    repo.switch("pr-1");
    repo.git_ok(["rebase", "main"]);
    repo.switch("main");
}

fn deleting_phase() -> Phase {
    Phase::Deleting { delete_plan: false }
}

fn conflict_current(state: &ReplayState) -> CurrentState {
    match &state.phase {
        Phase::Conflict { current, .. }
        | Phase::Replay {
            current: Some(current),
        } => current.clone(),
        phase => panic!("expected conflict or replay current phase, got {phase:?}"),
    }
}

fn paused_state(state: &ReplayState) -> &PausedState {
    match &state.phase {
        Phase::Paused { paused } => paused,
        phase => panic!("expected paused phase, got {phase:?}"),
    }
}

fn pending_branch_names(state: &ReplayState) -> Vec<&str> {
    state
        .pending_branches
        .iter()
        .map(|branch| branch.as_str())
        .collect()
}

fn read_state(repo: &TestRepo) -> ReplayState {
    let content = std::fs::read_to_string(repo.common_dir().join("cascade/state.yaml")).unwrap();
    serde_yaml::from_str(&content).unwrap()
}

fn write_state(repo: &TestRepo, state: &ReplayState) {
    let content = serde_yaml::to_string(state).unwrap();
    std::fs::write(repo.common_dir().join("cascade/state.yaml"), content).unwrap();
}
