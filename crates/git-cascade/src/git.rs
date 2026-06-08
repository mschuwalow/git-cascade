use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct Git {
    cwd: PathBuf,
}

impl Git {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }

    pub fn current_dir() -> Result<Self> {
        Ok(Self::new(std::env::current_dir()?))
    }

    pub fn output<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = collect_args(args);
        let output = Command::new("git")
            .current_dir(&self.cwd)
            .args(&args)
            .output()?;

        if !output.status.success() {
            return Err(Error::Git {
                args: display_args(&args),
                status: output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        String::from_utf8(output.stdout).map_err(|_| Error::GitUtf8 {
            args: display_args(&args),
        })
    }

    pub fn git_common_dir(&self) -> Result<PathBuf> {
        let output = self.output(["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok(PathBuf::from(output.trim()))
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
}

fn collect_args<I, S>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect()
}

fn display_args(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::Git;

    #[test]
    fn stores_cwd() {
        let git = Git::new("/tmp/example");

        assert_eq!(git.cwd(), std::path::Path::new("/tmp/example"));
    }
}
