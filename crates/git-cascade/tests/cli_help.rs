mod common;

use predicates::prelude::*;

use common::repo::TestRepo;

#[test]
fn cli_help_mentions_commands() {
    let repo = TestRepo::new();

    repo.cascade().arg("--help").assert().success().stdout(
        predicate::str::contains("plan")
            .and(predicate::str::contains("apply"))
            .and(predicate::str::contains("list"))
            .and(predicate::str::contains("show"))
            .and(predicate::str::contains("status"))
            .and(predicate::str::contains("abort"))
            .and(predicate::str::contains("continue"))
            .and(predicate::str::contains("completions")),
    );
}

#[test]
fn apply_help_mentions_strategy_and_dry_run() {
    let repo = TestRepo::new();

    repo.cascade()
        .args(["apply", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--strategy")
                .and(predicate::str::contains("preserve-fork-points"))
                .and(predicate::str::contains("move-to-heads"))
                .and(predicate::str::contains("--dry-run"))
                .and(predicate::str::contains("--old-anchor"))
                .and(predicate::str::contains("--new-anchor"))
                .and(predicate::str::contains(
                    "Replay planned dependent branches",
                ))
                .and(predicate::str::contains("Replacement ref or commit-ish")),
        );
}

#[test]
fn plan_help_mentions_anchor_and_replace() {
    let repo = TestRepo::new();

    repo.cascade()
        .args(["plan", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--anchor")
                .and(predicate::str::contains("--replace"))
                .and(predicate::str::contains("Old anchor ref or commit-ish")),
        );
}

#[test]
fn completions_help_mentions_shells() {
    let repo = TestRepo::new();

    repo.cascade()
        .args(["completions", "--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Generate shell completion scripts")
                .and(predicate::str::contains("bash"))
                .and(predicate::str::contains("zsh"))
                .and(predicate::str::contains("fish")),
        );
}

#[test]
fn completions_generate_bash_script() {
    let repo = TestRepo::new();

    repo.cascade()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("_git-cascade")
                .and(predicate::str::contains("--old-anchor"))
                .and(predicate::str::contains("completions")),
        );
}

#[test]
fn apply_requires_old_anchor() {
    let repo = TestRepo::new();

    repo.cascade()
        .args(["apply", "--new-anchor", "pr-1", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--old-anchor"));
}

#[test]
fn apply_rejects_invalid_strategy() {
    let repo = TestRepo::new();

    repo.cascade()
        .args([
            "apply",
            "--old-anchor",
            "pr-1",
            "--new-anchor",
            "pr-1",
            "--strategy",
            "invalid",
            "--dry-run",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid"));
}
