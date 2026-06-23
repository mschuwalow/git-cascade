mod common;

use common::repo::TestRepo;
use git_cascade::replay::{Phase, ReplayState};
use indoc::indoc;
use std::os::unix::fs::PermissionsExt;

#[test]
fn recovers_linear_stack_after_git_operation_interruptions() {
    let mut fail_at = 1;
    loop {
        let repo = clean_stack_with_rebased_root();
        let old_pr2 = repo.rev_parse("pr-2");
        let interrupted = apply_with_interruption_at(
            &repo,
            fail_at,
            ["plan", "apply", "stack", "--new-tip", "pr-1"],
        );

        assert_ne!(repo.rev_parse("pr-2"), old_pr2);
        assert_eq!(repo.merge_base("pr-1", "pr-2"), repo.rev_parse("pr-1"));
        assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
        assert_clean_cascade_state(&repo);

        if !interrupted {
            break;
        }
        fail_at += 1;
    }
}

#[test]
fn recovers_no_dependent_branches_after_git_operation_interruptions() {
    let mut fail_at = 1;
    loop {
        let repo = root_only_stack();
        let interrupted = apply_with_interruption_at(
            &repo,
            fail_at,
            ["plan", "apply", "stack", "--new-tip", "pr-1"],
        );

        assert_eq!(repo.show("pr-1:pr1.txt"), "a\n");
        assert_clean_cascade_state(&repo);

        if !interrupted {
            break;
        }
        fail_at += 1;
    }
}

#[test]
fn recovers_branches_already_at_replay_base_after_git_operation_interruptions() {
    let mut fail_at = 1;
    loop {
        let repo = clean_stack();
        let pr2 = repo.rev_parse("pr-2");
        let interrupted = apply_with_interruption_at(
            &repo,
            fail_at,
            ["plan", "apply", "stack", "--new-tip", "pr-1"],
        );

        assert_eq!(repo.rev_parse("pr-2"), pr2);
        assert_eq!(repo.show("pr-2:pr2.txt"), "b\n");
        assert_clean_cascade_state(&repo);

        if !interrupted {
            break;
        }
        fail_at += 1;
    }
}

#[test]
fn recovers_conflict_continuation_after_git_operation_interruptions() {
    let mut fail_at = 1;
    loop {
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

        let interrupted = apply_with_interruption_at(&repo, fail_at, ["continue"]);

        assert_ne!(repo.rev_parse("pr-2"), old_pr2);
        assert_eq!(repo.show("pr-2:conflict.txt"), "resolved\n");
        assert_clean_cascade_state(&repo);

        if !interrupted {
            break;
        }
        fail_at += 1;
    }
}

fn apply_with_interruption_at<const N: usize>(
    repo: &TestRepo,
    fail_at: usize,
    args: [&str; N],
) -> bool {
    let hook = write_operation_index_hook(repo);
    let count_path = repo.path().with_extension("hook-count");
    let mut interruptions = 0;

    loop {
        let mut command = repo.cascade();
        if interruptions == 0 {
            command.args(args);
        } else {
            command.arg("continue");
        }
        let output = command
            .env("GIT_CASCADE_TEST_HOOK_BEFORE_GIT_OPERATION", &hook)
            .env("GIT_CASCADE_TEST_HOOK_AFTER_GIT_OPERATION", &hook)
            .env("GIT_CASCADE_TEST_COMMON_DIR", repo.common_dir())
            .env("GIT_CASCADE_TEST_HOOK_COUNT", &count_path)
            .env("GIT_CASCADE_TEST_HOOK_FAIL_AT", fail_at.to_string())
            .output()
            .unwrap();

        if output.status.success() {
            return interruptions > 0;
        }

        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("test hook `before-git-operation` failed")
                || String::from_utf8_lossy(&output.stderr)
                    .contains("test hook `after-git-operation` failed"),
            "unexpected failure at operation {fail_at}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        interruptions += 1;
        assert!(interruptions == 1);
    }
}

fn write_operation_index_hook(repo: &TestRepo) -> std::path::PathBuf {
    let path = repo.path().join("operation-index-hook.sh");
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
            [ "$count" -gte "$GIT_CASCADE_TEST_HOOK_FAIL_AT" ] && exit 1
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
    let repo = clean_stack();
    repo.switch("main");
    repo.commit_file("main2.txt", "new base\n", "new base");
    repo.switch("pr-1");
    repo.git_ok(["rebase", "main"]);
    repo.switch("main");
    repo
}

fn clean_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");
    create_plan(&repo);
    repo
}

fn root_only_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch("main");
    create_plan(&repo);
    repo
}

fn conflicting_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("conflict.txt", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("conflict.txt", "anchor old\n", "anchor old");
    repo.switch_new("pr-2");
    repo.commit_file("conflict.txt", "dependent\n", "dependent");
    repo.switch("main");
    create_plan(&repo);
    repo.switch("pr-1");
    repo.commit_file("conflict.txt", "anchor new\n", "anchor new");
    repo.switch("main");
    repo
}

fn create_plan(repo: &TestRepo) {
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
}

fn assert_clean_cascade_state(repo: &TestRepo) {
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(!repo.plan_path("stack").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

fn conflict_current(state: &ReplayState) -> git_cascade::replay::CurrentState {
    match &state.phase {
        Phase::Conflict { current, .. } => current.clone(),
        phase => panic!("expected conflict phase, got {phase:?}"),
    }
}

fn read_state(repo: &TestRepo) -> ReplayState {
    let content = std::fs::read_to_string(repo.common_dir().join("cascade/state.yaml")).unwrap();
    serde_yaml::from_str(&content).unwrap()
}
