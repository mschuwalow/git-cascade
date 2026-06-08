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

    #[error("invalid plan key `{key}`: {reason}")]
    InvalidPlanKey { key: String, reason: String },

    #[error("invalid encoded component `{component}`")]
    InvalidEncodedComponent { component: String },

    #[error("plan `{key}` does not exist at {path}")]
    PlanNotFound { key: String, path: PathBuf },

    #[error("plan `{key}` already exists at {path}; pass --replace to overwrite it")]
    PlanExists { key: String, path: PathBuf },

    #[error("invalid plan: {0}")]
    InvalidPlan(String),

    #[error("invalid command invocation: {0}")]
    InvalidInvocation(String),

    #[error(
        "apply stopped while replaying branch `{branch}` commit `{commit}` in worktree {worktree}: {message}"
    )]
    ApplyStopped {
        branch: String,
        commit: String,
        worktree: PathBuf,
        message: String,
    },

    #[error("cannot start a new cascade operation while state file exists at {path}")]
    ActiveOperation { path: PathBuf },

    #[error("cannot infer old anchor base for `{anchor}`; pass --base <ref>")]
    CannotInferAnchorBase { anchor: String },

    #[error("{0}")]
    Unsupported(String),

    #[cfg(feature = "test-hooks")]
    #[error("test hook `{name}` failed with status {status}")]
    TestHookFailed { name: String, status: String },
}

pub type Result<T> = std::result::Result<T, Error>;
