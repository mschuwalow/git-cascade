mod common;

use common::repo::TestRepo;
use predicates::prelude::*;

/// A dependent branch that merged the old target tip becomes linear after
/// sync: the merge is flattened away.
#[test]
fn sync_flattens_merge_of_old_target_tip() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "m0");
    repo.switch_new("pr-1");
    repo.commit_file("c1.txt", "c1\n", "c1");
    repo.switch("main");
    repo.commit_file("m1.txt", "m1\n", "m1");
    repo.switch("pr-1");
    repo.git_ok(["merge", "--no-ff", "main", "-m", "merge main"]);
    repo.commit_file("c2.txt", "c2\n", "c2");
    repo.switch("main");
    repo.commit_file("m2.txt", "m2\n", "m2");

    repo.cascade()
        .arg("sync")
        .assert()
        .success()
        .stderr(predicate::str::contains("flattened merge"));

    assert_eq!(repo.merge_base("main", "pr-1"), repo.rev_parse("main"));
    assert!(
        repo.git_output(["rev-list", "--merges", "main..pr-1"])
            .is_empty()
    );
    assert_eq!(repo.show("pr-1:c1.txt"), "c1\n");
    assert_eq!(repo.show("pr-1:c2.txt"), "c2\n");
    assert_eq!(repo.show("pr-1:m1.txt"), "m1\n");
    assert_eq!(repo.show("pr-1:m2.txt"), "m2\n");
}

/// Sync is idempotent for a flattened branch: a second run keeps it.
#[test]
fn sync_after_flatten_is_idempotent() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "m0");
    repo.switch_new("pr-1");
    repo.commit_file("c1.txt", "c1\n", "c1");
    repo.switch("main");
    repo.commit_file("m1.txt", "m1\n", "m1");
    repo.switch("pr-1");
    repo.git_ok(["merge", "--no-ff", "main", "-m", "merge main"]);
    repo.switch("main");
    repo.commit_file("m2.txt", "m2\n", "m2");

    repo.cascade().arg("sync").assert().success();
    let synced_pr1 = repo.rev_parse("pr-1");

    repo.cascade()
        .arg("sync")
        .env("GIT_COMMITTER_DATE", "2026-02-02T00:00:00Z")
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "already starts at its replay base",
        ));

    assert_eq!(repo.rev_parse("pr-1"), synced_pr1);
}

/// Flattening drops the merge's resolution; conflicting changes re-fire as
/// regular cherry-pick conflicts and resolve through the normal flow.
#[test]
fn flatten_reconflicts_through_normal_conflict_flow() {
    let repo = TestRepo::new();
    repo.commit_file("conflict.txt", "base\n", "m0");
    repo.switch_new("pr-1");
    repo.commit_file("conflict.txt", "branch\n", "c1");
    repo.switch("main");
    repo.commit_file("conflict.txt", "main\n", "m1");
    repo.switch("pr-1");
    repo.git_fails(["merge", "main", "-m", "merge main"]);
    repo.write("conflict.txt", "resolved\n");
    repo.git_ok(["add", "conflict.txt"]);
    repo.git_ok(["commit", "--no-edit"]);
    repo.switch("main");
    repo.commit_file("m2.txt", "m2\n", "m2");

    repo.cascade()
        .arg("sync")
        .assert()
        .failure()
        .stderr(predicate::str::contains("apply stopped while replaying"));

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    std::fs::write(worktree.join("conflict.txt"), "resolved again\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "conflict.txt"]);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    assert!(
        repo.git_output(["rev-list", "--merges", "main..pr-1"])
            .is_empty()
    );
    assert_eq!(repo.show("pr-1:conflict.txt"), "resolved again\n");
}

/// Merges of in-range commits flatten cleanly when the new tip retains them.
#[test]
fn apply_flattens_merges_retained_by_new_tip() {
    let repo = double_merge_stack();

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
    repo.commit_file("a4.txt", "a4\n", "a4");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("flattened merge"));

    assert!(
        repo.git_output(["rev-list", "--merges", "pr-1..pr-2"])
            .is_empty()
    );
    assert_eq!(repo.show("pr-2:b.txt"), "b\n");
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
}

/// A merge of history not retained by the new tip cannot be flattened and is
/// rejected at apply time.
#[test]
fn apply_rejects_merge_of_history_lost_by_rewrite() {
    let repo = double_merge_stack();

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
    repo.switch_new_at("replacement", "main");
    repo.commit_file("replacement.txt", "r\n", "replacement");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "replacement"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "not contained in the new tip; rebase the branch to linearize it first",
        ));
}

/// A merge of an unrelated local branch cannot be flattened; the branch is
/// skipped during generation with a warning.
#[test]
fn generation_skips_branch_with_merged_local_work() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");
    repo.switch_new("pr-2");
    repo.commit_file("b.txt", "b\n", "b");
    repo.switch_new_at("side", "main");
    repo.commit_file("side.txt", "side\n", "side");
    repo.switch("pr-2");
    repo.git_ok(["merge", "--no-ff", "side", "-m", "merge side"]);
    let old_pr2 = repo.rev_parse("pr-2");
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
        .success()
        .stderr(predicate::str::contains(
            "skipping branch `pr-2`: it merges history that is not part of the old tip",
        ));

    let plan_yaml = std::fs::read_to_string(repo.plan_path("stack")).unwrap();
    assert!(!plan_yaml.contains("pr-2"));
    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
}

/// The same merged-local-work branch is rejected when it is part of a stored
/// plan's refs after planning.
#[test]
fn apply_rejects_merged_local_work_added_after_planning() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");
    repo.switch_new("pr-2");
    repo.commit_file("b.txt", "b\n", "b");
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
    // Merge unrelated local work into pr-2 after planning.
    repo.switch_new_at("side", "main");
    repo.commit_file("side.txt", "side\n", "side");
    repo.switch("pr-2");
    repo.git_ok(["merge", "--no-ff", "side", "-m", "merge side"]);
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "not contained in the new tip; rebase the branch to linearize it first",
        ));
}

/// Merges added to a branch after planning are flattened like planned ones.
#[test]
fn apply_flattens_merge_added_after_planning() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");
    repo.switch_new("pr-2");
    repo.commit_file("b.txt", "b\n", "b");
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
    // pr-1 advances; pr-2 merges the new pr-1 tip to catch up.
    repo.switch("pr-1");
    repo.commit_file("a2.txt", "a2\n", "a2");
    repo.switch("pr-2");
    repo.git_ok(["merge", "--no-ff", "pr-1", "-m", "merge pr-1"]);
    repo.switch("main");

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--strategy",
            "move-to-current-tips",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("flattened merge"));

    assert!(
        repo.git_output(["rev-list", "--merges", "pr-1..pr-2"])
            .is_empty()
    );
    assert_eq!(repo.show("pr-2:b.txt"), "b\n");
    assert_eq!(repo.show("pr-2:a2.txt"), "a2\n");
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
}

/// Dry-run prints merge flattening faithfully.
#[test]
fn apply_dry_run_prints_flattened_merges() {
    let repo = double_merge_stack();

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
    repo.commit_file("a4.txt", "a4\n", "a4");
    repo.switch("main");
    let old_pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# flatten merge"));

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

/// main -> pr-1 (a1, a2, a3); pr-2 forks at a1, then merges a2 and a3. The
/// merged commits sit outside pr-2's first-parent range, so the chain stays
/// linear and the merges are flattenable.
fn double_merge_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    let a1 = repo.commit_file("a1.txt", "a1\n", "a1");
    let a2 = repo.commit_file("a2.txt", "a2\n", "a2");
    let a3 = repo.commit_file("a3.txt", "a3\n", "a3");
    repo.switch_new_at("pr-2", &a1);
    repo.commit_file("b.txt", "b\n", "b");
    repo.git_ok(["merge", "--no-ff", &a2, "-m", "merge a2"]);
    repo.git_ok(["merge", "--no-ff", &a3, "-m", "merge a3"]);
    repo.switch("main");
    repo
}

fn read_state(repo: &TestRepo) -> git_cascade::state::ApplyState {
    let content = std::fs::read_to_string(repo.common_dir().join("cascade/state.yaml")).unwrap();
    serde_yaml::from_str(&content).unwrap()
}
