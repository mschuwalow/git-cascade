use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {path}: {source}")]
    IoWithPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("git command failed: git {args}\nstatus: {status}\nstderr: {stderr}")]
    Git {
        args: String,
        status: String,
        stderr: String,
    },

    #[error("git command output was not valid UTF-8: git {args}")]
    GitUtf8 { args: String },

    #[error("invalid plan name `{name}`: {reason}")]
    InvalidPlanName { name: String, reason: String },

    #[error("invalid encoded component `{component}`")]
    InvalidEncodedComponent { component: String },

    #[error("plan `{name}` does not exist at {path}")]
    PlanNotFound { name: String, path: PathBuf },

    #[error("plan `{name}` already exists at {path}; pass --replace to overwrite it")]
    PlanExists { name: String, path: PathBuf },

    #[error("invalid plan: {0}")]
    InvalidPlan(String),

    #[error("invalid command invocation: {0}")]
    InvalidInvocation(String),

    #[error(
        "apply stopped while replaying branch `{branch}` commit `{commit}` in worktree {worktree}: {message}\n\nResolve the conflicts in that worktree, stage the resolved files with `git -C {worktree} add <files>`, then run `git cascade continue`. Do not run `git -C {worktree} cherry-pick --continue` manually; git-cascade will do that after checking its recovery state."
    )]
    ApplyStopped {
        branch: String,
        commit: String,
        worktree: PathBuf,
        message: String,
    },

    #[error("cannot start a new cascade operation while state file exists at {path}")]
    ActiveOperation { path: PathBuf },

    #[error(
        "cannot infer old base for plan `{name}` from old tip `{old_tip}`; pass --old-base <ref>"
    )]
    CannotInferOldBase { name: String, old_tip: String },

    #[error("{0}")]
    Unsupported(String),

    #[cfg(feature = "test-hooks")]
    #[error("test hook `{name}` failed with status {status}")]
    TestHookFailed { name: String, status: String },
}

pub type Result<T> = std::result::Result<T, Error>;
