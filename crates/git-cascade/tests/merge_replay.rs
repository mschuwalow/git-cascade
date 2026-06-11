mod common;

use common::repo::TestRepo;
use predicates::prelude::*;

/// A dependent branch that merged the old target tip becomes linear after
/// sync: the merge is redundant on the new base and is dropped.
#[test]
fn sync_drops_merge_of_old_target_tip() {
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
        .stderr(predicate::str::contains("skipped redundant merge"));

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

/// `replay-resolution` preserves a manually resolved (evil) merge tree
/// byte-for-byte and keeps both parents.
#[test]
fn replay_resolution_preserves_evil_merge() {
    let repo = evil_merge_stack();

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
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success();

    // The branch tip is still a 2-parent merge.
    let parents = commit_parents(&repo, "pr-2");
    assert_eq!(parents.len(), 2);
    // The manual resolution is preserved exactly.
    assert_eq!(repo.show("pr-2:conflict.txt"), "evil resolution\n");
    assert_eq!(repo.show("pr-2:side.txt"), "side\n");
    // The branch sits on the rewritten anchor.
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
    assert_eq!(
        repo.git_output(["log", "-1", "--format=%s", "pr-2"]).trim(),
        "merge side"
    );
}

/// A merge whose first-parent diff conflicts with the rewritten base stops
/// with a merge-resolution operation; continue creates the 2-parent commit.
#[test]
fn replay_resolution_conflict_continues_to_merge_commit() {
    // pr-1 owns conflict.txt, an unrelated side branch also edits it, and
    // pr-2 merged side with a manual resolution.
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("conflict.txt", "pr-1\n", "a");
    repo.switch_new("pr-2");
    repo.commit_file("b.txt", "b\n", "b");
    repo.switch_new_at("side", "main");
    repo.commit_file("conflict.txt", "side\n", "side conflict");
    repo.switch("pr-2");
    repo.git_fails(["merge", "side", "-m", "merge side"]);
    repo.write("conflict.txt", "evil resolution\n");
    repo.git_ok(["add", "conflict.txt"]);
    repo.git_ok(["commit", "--no-edit"]);
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
    // Rewrite the anchor so it changes the file the merge resolution
    // touched; replaying the resolution then conflicts.
    repo.switch("pr-1");
    repo.write("conflict.txt", "anchor takes over\n");
    repo.git_ok(["add", "conflict.txt"]);
    repo.git_ok(["commit", "--amend", "--no-edit"]);
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("apply stopped while replaying"));

    repo.cascade().arg("status").assert().success().stdout(
        predicate::str::contains("phase: conflict")
            .and(predicate::str::contains("current-op: merge-resolution")),
    );

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    std::fs::write(worktree.join("conflict.txt"), "resolved again\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "conflict.txt"]);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    let parents = commit_parents(&repo, "pr-2");
    assert_eq!(parents.len(), 2);
    assert_eq!(repo.show("pr-2:conflict.txt"), "resolved again\n");
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

/// `re-merge` recreates the merge on the rewritten parents, keeping the
/// original message.
#[test]
fn re_merge_recreates_clean_merge() {
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
            "--merge-strategy",
            "re-merge",
        ])
        .assert()
        .success();

    let parents = commit_parents(&repo, "pr-2");
    assert_eq!(parents.len(), 2);
    assert_eq!(repo.show("pr-2:side.txt"), "side\n");
    assert_eq!(repo.show("pr-2:b.txt"), "b\n");
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
    assert_eq!(
        repo.git_output(["log", "-1", "--format=%s", "pr-2"]).trim(),
        "merge side"
    );
}

/// `re-merge` recomputes the resolution; conflicts stop with a re-merge
/// operation and continue completes the merge.
#[test]
fn re_merge_conflict_continues_to_merge_commit() {
    let repo = evil_merge_stack();

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

    // Re-merging the side branch re-runs the original conflict.
    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--merge-strategy",
            "re-merge",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("apply stopped while replaying"));

    repo.cascade().arg("status").assert().success().stdout(
        predicate::str::contains("merge-strategy: re-merge")
            .and(predicate::str::contains("current-op: re-merge")),
    );

    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    std::fs::write(worktree.join("conflict.txt"), "re-resolved\n").unwrap();
    repo.git_ok(["-C", worktree.to_str().unwrap(), "add", "conflict.txt"]);

    repo.cascade()
        .arg("continue")
        .assert()
        .success()
        .stdout("continued cascade operation\n");

    let parents = commit_parents(&repo, "pr-2");
    assert_eq!(parents.len(), 2);
    assert_eq!(repo.show("pr-2:conflict.txt"), "re-resolved\n");
    assert_eq!(
        repo.git_output(["log", "-1", "--format=%s", "pr-2"]).trim(),
        "merge side"
    );
}

#[test]
fn abort_during_merge_conflict_restores_refs_and_cleans_worktree() {
    let repo = evil_merge_stack();
    let old_pr2 = repo.rev_parse("pr-2");

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
    repo.commit_file("conflict.txt", "anchor takes over\n", "anchor conflict");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .failure();
    let state = read_state(&repo);
    let worktree = std::path::PathBuf::from(state.worktree.path());
    assert!(worktree.exists());

    repo.cascade()
        .arg("abort")
        .assert()
        .success()
        .stdout("aborted cascade operation\n");

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!worktree.exists());
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
    assert!(repo.git_output(["for-each-ref", "refs/cascade"]).is_empty());
}

#[test]
fn plan_rejects_octopus_merges() {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");
    repo.switch_new("side-1");
    repo.commit_file("s1.txt", "s1\n", "s1");
    repo.switch_new_at("side-2", "pr-1");
    repo.commit_file("s2.txt", "s2\n", "s2");
    repo.switch_new_at("pr-2", "pr-1");
    repo.commit_file("b.txt", "b\n", "b");
    repo.git_ok(["merge", "side-1", "side-2", "-m", "octopus"]);
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
        .failure()
        .stderr(predicate::str::contains("octopus merges are not supported"));
}

#[test]
fn apply_dry_run_prints_merge_commands_for_both_strategies() {
    let repo = evil_merge_stack();

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
    let old_pr2 = repo.rev_parse("pr-2");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1", "--dry-run"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("merge-strategy replay-resolution")
                .and(predicate::str::contains("cherry-pick -m 1 --no-commit"))
                .and(predicate::str::contains("commit-tree"))
                .and(predicate::str::contains("may be dropped at apply time")),
        );

    repo.cascade()
        .args([
            "plan",
            "apply",
            "stack",
            "--new-tip",
            "pr-1",
            "--merge-strategy",
            "re-merge",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("merge-strategy re-merge")
                .and(predicate::str::contains("merge --no-ff")),
        );

    assert_eq!(repo.rev_parse("pr-2"), old_pr2);
    assert!(!repo.common_dir().join("cascade/state.yaml").exists());
}

/// A merge parent strictly inside a genuinely rewritten root range cannot be
/// mapped and is rejected at apply time.
#[test]
fn apply_rejects_merge_parent_lost_by_rewrite() {
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
    // Replace pr-1's history entirely: the merged mid-range commit no longer
    // exists in the new history.
    repo.switch_new_at("replacement", "main");
    repo.commit_file("replacement.txt", "r\n", "replacement");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "replacement"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "not retained by the new tip; its rewritten counterpart is unknown",
        ));
}

/// The same plan applies cleanly when the new tip still contains the merged
/// mid-range commit (restack-style advance without rewriting); the merges
/// become redundant and are dropped.
#[test]
fn apply_keeps_merge_parent_retained_by_new_tip() {
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
    // pr-1 advances without rewriting; the merged commits stay reachable
    // from the new tip.
    repo.switch("pr-1");
    repo.commit_file("a4.txt", "a4\n", "a4");
    repo.switch("main");

    repo.cascade()
        .args(["plan", "apply", "stack", "--new-tip", "pr-1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("skipped redundant merge"));

    assert!(
        repo.git_output(["rev-list", "--merges", "pr-1..pr-2"])
            .is_empty()
    );
    assert_eq!(repo.show("pr-2:b.txt"), "b\n");
    repo.git_ok(["merge-base", "--is-ancestor", "pr-1", "pr-2"]);
}

/// main -> pr-1 (a1, a2, a3); pr-2 forks at a1, then merges a2 and a3 from
/// inside the root range, leaving a merge parent (a2) that is neither the
/// node base (a3) nor one of the node's own commits.
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

/// Builds main -> pr-1 -> pr-2 where pr-2 merged an unrelated side branch
/// with a conflict resolved to content not present on either side.
fn evil_merge_stack() -> TestRepo {
    let repo = TestRepo::new();
    repo.commit_file("README.md", "base\n", "initial");
    repo.commit_file("conflict.txt", "base\n", "conflict base");
    repo.switch_new("pr-1");
    repo.commit_file("a.txt", "a\n", "a");
    repo.switch_new("pr-2");
    repo.commit_file("conflict.txt", "pr-2\n", "b");
    repo.switch_new_at("side", "main");
    repo.commit_file("conflict.txt", "side\n", "side conflict");
    repo.commit_file("side.txt", "side\n", "side");
    repo.switch("pr-2");
    repo.git_fails(["merge", "side", "-m", "merge side"]);
    repo.write("conflict.txt", "evil resolution\n");
    repo.git_ok(["add", "conflict.txt"]);
    repo.git_ok(["commit", "--no-edit"]);
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

fn commit_parents(repo: &TestRepo, rev: &str) -> Vec<String> {
    repo.git_output(["rev-list", "--parents", "-n", "1", rev])
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect()
}

fn read_state(repo: &TestRepo) -> git_cascade::state::ApplyState {
    let content = std::fs::read_to_string(repo.common_dir().join("cascade/state.yaml")).unwrap();
    serde_yaml::from_str(&content).unwrap()
}
