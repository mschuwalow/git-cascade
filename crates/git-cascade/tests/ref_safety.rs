mod common;

use common::repo::TestRepo;
use git_cascade::state::Phase;
use indoc::indoc;
use predicates::prelude::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn apply_uses_persisted_new_tip_if_new_tip_ref_moved() {
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
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("main");
    repo.commit_file("main2.txt", "new base\n", "new base");
    repo.switch("pr-1");
    repo.git_ok(["rebase", "main"]);
    let rebased_anchor = repo.rev_parse("pr-1");
    repo.switch("main");

    let hook = write_move_anchor_hook(&repo);
    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .env("GIT_CASCADE_TEST_HOOK_BEFORE_FINAL_UPDATE", &hook)
        .env("GIT_CASCADE_TEST_REPO", repo.path())
        .assert()
        .success()
        .stdout("applied cascade plan\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
    assert_ne!(repo.rev_parse("pr-1"), rebased_anchor);
    assert_eq!(repo.rev_parse("pr-1"), repo.rev_parse("main"));
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn continue_recovers_final_update_failure() {
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
    let old_pr2 = repo.rev_parse("pr-2");
    repo.switch("main");
    repo.commit_file("main2.txt", "new base\n", "new base");
    repo.switch("pr-1");
    repo.git_ok(["rebase", "main"]);
    repo.switch("main");

    let hook = write_move_dependent_hook(&repo);
    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .env("GIT_CASCADE_TEST_HOOK_BEFORE_FINAL_UPDATE", &hook)
        .env("GIT_CASCADE_TEST_REPO", repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("git update-ref --stdin"));

    let state = read_state(&repo);
    assert_eq!(state.phase, Phase::FinalUpdate);
    assert_eq!(repo.rev_parse("pr-2"), repo.rev_parse("main"));
    assert!(repo.common_dir().join("cascade/state.yaml").exists());

    repo.git_ok(["update-ref", "refs/heads/pr-2", old_pr2.as_str()]);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");
    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn continue_recovers_after_final_update_committed() {
    let repo = clean_stack_with_rebased_root();
    let old_pr2 = repo.rev_parse("pr-2");
    let hook = write_failing_hook(&repo, "after-final-update-hook.sh");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .env("GIT_CASCADE_TEST_HOOK_AFTER_FINAL_UPDATE", &hook)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "test hook `after-final-update` failed",
        ));

    let state = read_state(&repo);
    assert_eq!(state.phase, Phase::FinalUpdate);
    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert!(repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.plan_path("stack").exists());

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn continue_finishes_successful_deleting_state() {
    let repo = clean_stack_with_rebased_root();
    let old_pr2 = repo.rev_parse("pr-2");
    let hook = write_failing_hook(&repo, "after-deleting-state-written-hook.sh");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .env("GIT_CASCADE_TEST_HOOK_AFTER_DELETING_STATE_WRITTEN", &hook)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "test hook `after-deleting-state-written` failed",
        ));

    let state = read_state(&repo);
    match state.phase {
        Phase::Deleting { delete_plan } => assert!(delete_plan),
        phase => panic!("expected deleting phase, got {phase:?}"),
    }
    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert!(repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.plan_path("stack").exists());

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert_ne!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn continue_recovers_after_interruption_following_git_operations() {
    let mut total_interruptions = 0;

    for seed in 1..=12 {
        let repo = clean_stack_with_rebased_root();
        let old_pr2 = repo.rev_parse("pr-2");
        let hook = write_probabilistic_active_git_operation_hook(&repo);
        let count_path = repo.path().join("git-operation-hook-count");
        let mut interruptions = 0;

        loop {
            let mut command = repo.cascade();
            if interruptions == 0 {
                command.args(["plan", "apply", "stack", "--new-tip", "pr-1"]);
            } else {
                command.arg("continue");
            }
            let output = command
                .env("GIT_CASCADE_TEST_HOOK_AFTER_GIT_OPERATION", &hook)
                .env("GIT_CASCADE_TEST_COMMON_DIR", repo.common_dir())
                .env("GIT_CASCADE_TEST_HOOK_COUNT", &count_path)
                .env("GIT_CASCADE_TEST_HOOK_FAILURE_PERCENT", "10")
                .env("GIT_CASCADE_TEST_HOOK_SEED", seed.to_string())
                .output()
                .unwrap();

            if output.status.success() {
                break;
            }

            interruptions += 1;
            assert!(
                interruptions <= 80,
                "too many interrupted attempts for seed {seed}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("test hook `after-git-operation` failed"),
                "unexpected failure for seed {seed}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(repo.common_dir().join("cascade/state.yaml").exists());
        }

        let count = std::fs::read_to_string(count_path)
            .ok()
            .and_then(|content| content.trim().parse::<usize>().ok())
            .unwrap_or(0);
        assert!(count >= interruptions);
        total_interruptions += interruptions;
        assert_ne!(
            repo.rev_parse("pr-2"),
            old_pr2,
            "seed {seed} completed with {interruptions} interruptions but did not update pr-2"
        );
        assert!(!repo.common_dir().join("cascade/state.yaml").exists());
        assert!(!repo.plan_path("stack").exists());
        assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
    }

    assert!(total_interruptions > 0);
}

fn write_move_anchor_hook(repo: &TestRepo) -> std::path::PathBuf {
    let path = repo.path().join("move-anchor-hook.sh");
    std::fs::write(
        &path,
        indoc! {r#"
            #!/bin/sh
            git -C "$GIT_CASCADE_TEST_REPO" update-ref refs/heads/pr-1 refs/heads/main
        "#},
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn write_move_dependent_hook(repo: &TestRepo) -> std::path::PathBuf {
    let path = repo.path().join("move-dependent-hook.sh");
    std::fs::write(
        &path,
        indoc! {r#"
            #!/bin/sh
            git -C "$GIT_CASCADE_TEST_REPO" update-ref refs/heads/pr-2 refs/heads/main
        "#},
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn write_failing_hook(repo: &TestRepo, name: &str) -> std::path::PathBuf {
    let path = repo.path().join(name);
    std::fs::write(
        &path,
        indoc! {r#"
            #!/bin/sh
            exit 1
        "#},
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn write_probabilistic_active_git_operation_hook(repo: &TestRepo) -> std::path::PathBuf {
    let path = repo
        .path()
        .join("probabilistic-active-git-operation-hook.sh");
    std::fs::write(
        &path,
        indoc! {r#"
            #!/bin/sh
            state="$GIT_CASCADE_TEST_COMMON_DIR/cascade/state.yaml"
            [ -f "$state" ] || exit 0
            count=0
            [ -f "$GIT_CASCADE_TEST_HOOK_COUNT" ] && count=$(cat "$GIT_CASCADE_TEST_HOOK_COUNT")
            count=$((count + 1))
            echo "$count" > "$GIT_CASCADE_TEST_HOOK_COUNT"
            seed=${GIT_CASCADE_TEST_HOOK_SEED:-1}
            percent=${GIT_CASCADE_TEST_HOOK_FAILURE_PERCENT:-10}
            roll=$(((count * 37 + seed * 19) % 100))
            [ "$roll" -lt "$percent" ] && exit 1
            exit 0
        "#},
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn clean_stack_with_rebased_root() -> TestRepo {
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
    repo.switch("main");
    repo.commit_file("main2.txt", "new base\n", "new base");
    repo.switch("pr-1");
    repo.git_ok(["rebase", "main"]);
    repo.switch("main");
    repo
}

fn read_state(repo: &TestRepo) -> git_cascade::state::ApplyState {
    let content = std::fs::read_to_string(repo.common_dir().join("cascade/state.yaml")).unwrap();
    serde_yaml::from_str(&content).unwrap()
}
