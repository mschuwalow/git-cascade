use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBranch {
    pub name: String,
    pub tip: String,
}

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

    pub fn run<I, S>(&self, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.output(args).map(|_| ())
    }

    pub fn try_output<I, S>(&self, args: I) -> Result<Option<String>>
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
            return Ok(None);
        }

        String::from_utf8(output.stdout)
            .map(Some)
            .map_err(|_| Error::GitUtf8 {
                args: display_args(&args),
            })
    }

    pub fn git_common_dir(&self) -> Result<PathBuf> {
        let output = self.output(["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        Ok(PathBuf::from(output.trim()))
    }

    pub fn head_oid(&self) -> Result<String> {
        self.rev_parse("HEAD")
    }

    pub fn rev_parse(&self, rev: &str) -> Result<String> {
        Ok(self
            .output(["rev-parse", "--verify", rev])?
            .trim()
            .to_owned())
    }

    pub fn resolve_commit(&self, rev: &str) -> Result<String> {
        self.rev_parse(&format!("{rev}^{{commit}}"))
    }

    pub fn symbolic_full_name(&self, rev: &str) -> Result<Option<String>> {
        Ok(self
            .try_output(["rev-parse", "--symbolic-full-name", rev])?
            .map(|output| output.trim().to_owned())
            .filter(|output| output.starts_with("refs/")))
    }

    pub fn try_rev_parse(&self, rev: &str) -> Result<Option<String>> {
        Ok(self
            .try_output(["rev-parse", "--verify", "--quiet", rev])?
            .map(|output| output.trim().to_owned())
            .filter(|output| !output.is_empty()))
    }

    pub fn local_branch_tip(&self, branch: &str) -> Result<String> {
        self.rev_parse(&format!("refs/heads/{branch}^{{commit}}"))
    }

    pub fn local_branches(&self) -> Result<Vec<LocalBranch>> {
        let output = self.output([
            "for-each-ref",
            "--format=%(refname:short)%09%(objectname)",
            "refs/heads",
        ])?;

        let mut branches = Vec::new();
        for line in output.lines() {
            let Some((name, tip)) = line.split_once('\t') else {
                continue;
            };
            branches.push(LocalBranch {
                name: name.to_owned(),
                tip: tip.to_owned(),
            });
        }
        branches.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(branches)
    }

    pub fn merge_base(&self, left: &str, right: &str) -> Result<Option<String>> {
        Ok(self
            .try_output(["merge-base", left, right])?
            .map(|output| output.trim().to_owned())
            .filter(|output| !output.is_empty()))
    }

    pub fn rev_list_reverse(&self, base: &str, tip: &str) -> Result<Vec<String>> {
        let range = format!("{base}..{tip}");
        Ok(self
            .output(["rev-list", "--reverse", &range])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    pub fn rev_list_merges(&self, base: &str, tip: &str) -> Result<Vec<String>> {
        let range = format!("{base}..{tip}");
        Ok(self
            .output(["rev-list", "--merges", &range])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    pub fn commit_exists(&self, oid: &str) -> Result<bool> {
        self.try_output(["cat-file", "-e", &format!("{oid}^{{commit}}")])
            .map(|output| output.is_some())
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        self.try_output(["merge-base", "--is-ancestor", ancestor, descendant])
            .map(|output| output.is_some())
    }

    pub fn upstream_tip(&self, branch: &str) -> Result<Option<String>> {
        self.try_rev_parse(&format!("{branch}@{{upstream}}^{{commit}}"))
    }

    pub fn origin_default_branch_tip(&self) -> Result<Option<String>> {
        let Some(default_ref) =
            self.try_output(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])?
        else {
            return Ok(None);
        };
        let default_ref = default_ref.trim();
        if default_ref.is_empty() {
            return Ok(None);
        }

        self.try_rev_parse(&format!("{default_ref}^{{commit}}"))
    }

    pub fn worktree_add_detached(&self, path: &Path, commit: &str) -> Result<()> {
        self.output([
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            path.as_os_str().to_owned(),
            OsString::from(commit),
        ])
        .map(|_| ())
    }

    pub fn worktree_remove_force(&self, path: &Path) -> Result<()> {
        self.output([
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            path.as_os_str().to_owned(),
        ])
        .map(|_| ())
    }

    pub fn reset_hard(&self, commit: &str) -> Result<()> {
        self.run(["reset", "--hard", commit])
    }

    pub fn cherry_pick(&self, commit: &str) -> Result<()> {
        self.run(["cherry-pick", commit])
    }

    pub fn update_ref(&self, refname: &str, new_value: &str) -> Result<()> {
        self.run(["update-ref", refname, new_value])
    }

    pub fn delete_ref(&self, refname: &str) -> Result<()> {
        self.run(["update-ref", "-d", refname])
    }

    pub fn update_ref_transaction(&self, commands: &str) -> Result<()> {
        let args = [OsString::from("update-ref"), OsString::from("--stdin")];
        let mut child = Command::new("git")
            .current_dir(&self.cwd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        child
            .stdin
            .as_mut()
            .expect("stdin is piped")
            .write_all(commands.as_bytes())?;
        let output = child.wait_with_output()?;

        if output.status.success() {
            return Ok(());
        }

        Err(Error::Git {
            args: display_args(&args),
            status: output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
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
