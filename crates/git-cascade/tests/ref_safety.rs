#![cfg(feature = "test-hooks")]

mod common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use predicates::prelude::*;

use common::repo::TestRepo;

#[test]
fn apply_refuses_final_update_if_new_anchor_ref_moved() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("pr1.txt", "a\n", "pr-1");
    repo.switch_new("pr-2");
    repo.commit_file("pr2.txt", "b\n", "pr-2");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "--anchor", "pr-1"])
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
        .args(["apply", "--old-anchor", "pr-1", "--new-anchor", "pr-1"])
        .env("GIT_CASCADE_TEST_HOOK_BEFORE_FINAL_UPDATE", &hook)
        .env("GIT_CASCADE_TEST_REPO", repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("git update-ref --stdin"));

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert_ne!(repo.rev_parse("pr-1"), rebased_anchor);
    assert!(repo.common_dir().join("cascade/state.yaml").exists());

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

fn write_move_anchor_hook(repo: &TestRepo) -> std::path::PathBuf {
    let path = repo.path().join("move-anchor-hook.sh");
    let mut file = std::fs::File::create(&path).unwrap();
    writeln!(file, "#!/bin/sh").unwrap();
    writeln!(
        file,
        "git -C \"$GIT_CASCADE_TEST_REPO\" update-ref refs/heads/pr-1 refs/heads/main"
    )
    .unwrap();

    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}
