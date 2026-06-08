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
            .and(predicate::str::contains("continue")),
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
                .and(predicate::str::contains("--name"))
                .and(predicate::str::contains("--new-anchor")),
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
                .and(predicate::str::contains("--name")),
        );
}

#[test]
fn apply_requires_name() {
    let repo = TestRepo::new();

    repo.cascade()
        .args(["apply", "--new-anchor", "pr-1", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--name"));
}

#[test]
fn apply_rejects_invalid_strategy() {
    let repo = TestRepo::new();

    repo.cascade()
        .args([
            "apply",
            "--name",
            "stack",
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
