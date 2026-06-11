use crate::{Error, Result};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBranch {
    pub name: String,
    pub tip: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: Option<String>,
}

/// Author identity of an existing commit, used to preserve authorship when
/// re-creating merge commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitAuthor {
    pub name: String,
    pub email: String,
    pub date: String,
}

impl CommitAuthor {
    fn env(&self) -> [(&'static str, &str); 3] {
        [
            ("GIT_AUTHOR_NAME", self.name.as_str()),
            ("GIT_AUTHOR_EMAIL", self.email.as_str()),
            ("GIT_AUTHOR_DATE", self.date.as_str()),
        ]
    }
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
        self.output_with(args, &[], None)
    }

    fn output_with<I, S>(
        &self,
        args: I,
        envs: &[(&str, &str)],
        stdin: Option<&str>,
    ) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = collect_args(args);
        let mut command = Command::new("git");
        command.current_dir(&self.cwd).args(&args);
        for (key, value) in envs {
            command.env(key, value);
        }

        let output = if let Some(input) = stdin {
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn()?;
            child
                .stdin
                .as_mut()
                .expect("stdin is piped")
                .write_all(input.as_bytes())?;
            child.wait_with_output()?
        } else {
            command.output()?
        };

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

    fn output_allowing_status<I, S>(
        &self,
        args: I,
        allowed_statuses: &[i32],
    ) -> Result<Option<String>>
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
            if output
                .status
                .code()
                .is_some_and(|status| allowed_statuses.contains(&status))
            {
                return Ok(None);
            }
            return Err(Error::Git {
                args: display_args(&args),
                status: output
                    .status
                    .code()
                    .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
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

    pub fn worktree_root(&self) -> Result<PathBuf> {
        let output = self.output(["rev-parse", "--path-format=absolute", "--show-toplevel"])?;
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
        if let Some(refname) = self
            .output_allowing_status(
                [
                    "rev-parse",
                    "--symbolic-full-name",
                    "--verify",
                    "--quiet",
                    rev,
                ],
                &[1],
            )?
            .map(|output| output.trim().to_owned())
            .filter(|output| output.starts_with("refs/"))
        {
            return Ok(Some(refname));
        }

        let local_branch = format!("refs/heads/{rev}");
        if self
            .try_rev_parse(&format!("{local_branch}^{{commit}}"))?
            .is_some()
        {
            return Ok(Some(local_branch));
        }

        let remote_branch = format!("refs/remotes/{rev}");
        if self
            .try_rev_parse(&format!("{remote_branch}^{{commit}}"))?
            .is_some()
        {
            return Ok(Some(remote_branch));
        }

        Ok(None)
    }

    pub fn try_rev_parse(&self, rev: &str) -> Result<Option<String>> {
        Ok(self
            .output_allowing_status(["rev-parse", "--verify", "--quiet", rev], &[1])?
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

    pub fn checked_out_branches(&self) -> Result<Vec<String>> {
        let mut branches = self
            .worktrees()?
            .into_iter()
            .filter_map(|worktree| worktree.branch)
            .collect::<Vec<_>>();
        branches.sort();
        branches.dedup();
        Ok(branches)
    }

    pub fn worktrees(&self) -> Result<Vec<WorktreeEntry>> {
        let output = self.output(["worktree", "list", "--porcelain"])?;
        let mut worktrees = Vec::new();
        let mut path = None;
        let mut branch = None;

        for line in output.lines() {
            if line.is_empty() {
                if let Some(path) = path.take() {
                    worktrees.push(WorktreeEntry {
                        path,
                        branch: branch.take(),
                    });
                }
                continue;
            }

            if let Some(value) = line.strip_prefix("worktree ") {
                if let Some(path) = path.replace(PathBuf::from(value)) {
                    worktrees.push(WorktreeEntry {
                        path,
                        branch: branch.take(),
                    });
                }
                continue;
            }

            let Some(refname) = line.strip_prefix("branch ") else {
                continue;
            };
            let Some(name) = refname.strip_prefix("refs/heads/") else {
                continue;
            };
            branch = Some(name.to_owned());
        }

        if let Some(path) = path {
            worktrees.push(WorktreeEntry { path, branch });
        }

        Ok(worktrees)
    }

    pub fn current_branch(&self) -> Result<Option<String>> {
        Ok(self
            .output_allowing_status(["symbolic-ref", "--quiet", "--short", "HEAD"], &[1])?
            .map(|output| output.trim().to_owned())
            .filter(|output| !output.is_empty()))
    }

    pub fn ensure_clean_worktree(&self) -> Result<()> {
        let status = self.output(["status", "--porcelain"])?;
        if status.is_empty() {
            return Ok(());
        }

        Err(Error::InvalidInvocation(
            "cannot apply in-place with a dirty worktree; commit, stash, or discard local changes first".to_owned(),
        ))
    }

    pub fn checked_out_branches_except(&self, excluded_path: &Path) -> Result<Vec<String>> {
        let excluded_path = std::fs::canonicalize(excluded_path)?;
        let mut branches = Vec::new();
        for worktree in self.worktrees()? {
            let Ok(path) = std::fs::canonicalize(&worktree.path) else {
                continue;
            };
            if path == excluded_path {
                continue;
            };
            if let Some(branch) = worktree.branch {
                branches.push(branch);
            }
        }
        branches.sort();
        branches.dedup();
        Ok(branches)
    }

    pub fn merge_base(&self, left: &str, right: &str) -> Result<Option<String>> {
        Ok(self
            .output_allowing_status(["merge-base", left, right], &[1])?
            .map(|output| output.trim().to_owned())
            .filter(|output| !output.is_empty()))
    }

    /// All merge bases between two commits. More than one entry indicates a
    /// criss-cross history where the fork point is ambiguous.
    pub fn merge_bases_all(&self, left: &str, right: &str) -> Result<Vec<String>> {
        Ok(self
            .output_allowing_status(["merge-base", "--all", left, right], &[1])?
            .map(|output| output.lines().map(str::to_owned).collect())
            .unwrap_or_default())
    }

    pub fn rev_list_reverse(&self, base: &str, tip: &str) -> Result<Vec<String>> {
        let range = format!("{base}..{tip}");
        Ok(self
            .output(["rev-list", "--reverse", &range])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// Commits in `base..tip` with their parents, parents-before-children.
    pub fn rev_list_with_parents(
        &self,
        base: &str,
        tip: &str,
    ) -> Result<Vec<(String, Vec<String>)>> {
        let range = format!("{base}..{tip}");
        let output = self.output(["rev-list", "--reverse", "--topo-order", "--parents", &range])?;
        let mut commits = Vec::new();
        for line in output.lines() {
            let mut parts = line.split_whitespace().map(str::to_owned);
            let Some(oid) = parts.next() else {
                continue;
            };
            commits.push((oid, parts.collect()));
        }
        Ok(commits)
    }

    pub fn rev_list_merges(&self, base: &str, tip: &str) -> Result<Vec<String>> {
        let range = format!("{base}..{tip}");
        Ok(self
            .output(["rev-list", "--merges", &range])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    pub fn rev_list_first_parent_merges(&self, tip: &str) -> Result<Vec<String>> {
        Ok(self
            .output(["rev-list", "--first-parent", "--merges", tip])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    pub fn commit_parents(&self, commit: &str) -> Result<Vec<String>> {
        let output = self.output(["rev-list", "--parents", "-n", "1", commit])?;
        Ok(output
            .split_whitespace()
            .skip(1)
            .map(str::to_owned)
            .collect())
    }

    pub fn commit_exists(&self, oid: &str) -> Result<bool> {
        self.output_allowing_status(["cat-file", "-e", &format!("{oid}^{{commit}}")], &[1, 128])
            .map(|output| output.is_some())
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        self.output_allowing_status(["merge-base", "--is-ancestor", ancestor, descendant], &[1])
            .map(|output| output.is_some())
    }

    pub fn origin_default_branch_tip(&self) -> Result<Option<String>> {
        let Some(default_ref) = self.output_allowing_status(
            ["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            &[1, 128],
        )?
        else {
            return Ok(None);
        };
        let default_ref = default_ref.trim();
        if default_ref.is_empty() {
            return Ok(None);
        }

        self.try_rev_parse(&format!("{default_ref}^{{commit}}"))
    }

    pub fn local_default_branch_tip(&self) -> Result<Option<String>> {
        for branch in ["main", "master"] {
            if let Some(tip) = self.try_rev_parse(&format!("refs/heads/{branch}^{{commit}}"))? {
                return Ok(Some(tip));
            }
        }

        Ok(None)
    }

    pub fn default_branch_ref(&self) -> Result<Option<String>> {
        if let Some(default_ref) = self.output_allowing_status(
            ["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            &[1, 128],
        )? {
            let default_ref = default_ref.trim();
            if !default_ref.is_empty() {
                return Ok(Some(default_ref.to_owned()));
            }
        }

        for branch in ["main", "master"] {
            if self
                .try_rev_parse(&format!("refs/heads/{branch}^{{commit}}"))?
                .is_some()
            {
                return Ok(Some(branch.to_owned()));
            }
        }

        Ok(None)
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

    pub fn switch_detached(&self, commit: &str) -> Result<()> {
        self.run(["switch", "--detach", commit])
    }

    pub fn switch_branch(&self, branch: &str) -> Result<()> {
        self.run(["switch", branch])
    }

    pub fn cherry_pick(&self, commit: &str) -> Result<()> {
        self.run(["cherry-pick", commit])
    }

    /// Stages the first-parent diff of a merge commit without committing.
    pub fn cherry_pick_mainline_no_commit(&self, commit: &str) -> Result<()> {
        self.run(["cherry-pick", "-m", "1", "--no-commit", commit])
    }

    pub fn cherry_pick_continue(&self) -> Result<()> {
        self.run(["cherry-pick", "--continue"])
    }

    pub fn cherry_pick_skip(&self) -> Result<()> {
        self.run(["cherry-pick", "--skip"])
    }

    pub fn try_cherry_pick_quit(&self) -> Result<()> {
        self.output_allowing_status(["cherry-pick", "--quit"], &[1, 128])
            .map(|_| ())
    }

    pub fn cherry_pick_in_progress(&self) -> Result<bool> {
        Ok(self.try_rev_parse("CHERRY_PICK_HEAD")?.is_some())
    }

    /// Reports whether the index differs from HEAD.
    pub fn has_staged_changes(&self) -> Result<bool> {
        self.output_allowing_status(["diff", "--cached", "--quiet"], &[1])
            .map(|output| output.is_none())
    }

    pub fn commit_author(&self, commit: &str) -> Result<CommitAuthor> {
        let output = self.output(["log", "-1", "--format=%an%x00%ae%x00%aD", commit])?;
        let mut parts = output.trim_end_matches('\n').splitn(3, '\0');
        let (Some(name), Some(email), Some(date)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::Unsupported(format!(
                "cannot read author of commit `{commit}`"
            )));
        };
        Ok(CommitAuthor {
            name: name.to_owned(),
            email: email.to_owned(),
            date: date.to_owned(),
        })
    }

    pub fn commit_message(&self, commit: &str) -> Result<String> {
        self.output(["log", "-1", "--format=%B", commit])
    }

    pub fn write_tree(&self) -> Result<String> {
        Ok(self.output(["write-tree"])?.trim().to_owned())
    }

    /// Creates a commit object for `tree` with explicit parents, preserving
    /// the given author. The committer is the current user, matching
    /// cherry-pick behavior.
    pub fn commit_tree(
        &self,
        tree: &str,
        parents: &[String],
        message: &str,
        author: &CommitAuthor,
    ) -> Result<String> {
        let mut args = vec![OsString::from("commit-tree"), OsString::from(tree)];
        for parent in parents {
            args.push(OsString::from("-p"));
            args.push(OsString::from(parent));
        }
        Ok(self
            .output_with(args, &author.env(), Some(message))?
            .trim()
            .to_owned())
    }

    /// Re-merges `commit` into HEAD with the original merge's message and
    /// author.
    pub fn merge_no_ff(&self, commit: &str, message: &str, author: &CommitAuthor) -> Result<()> {
        self.output_with(
            ["merge", "--no-ff", "--no-edit", "-m", message, commit],
            &author.env(),
            None,
        )
        .map(|_| ())
    }

    pub fn merge_in_progress(&self) -> Result<bool> {
        Ok(self.try_rev_parse("MERGE_HEAD")?.is_some())
    }

    pub fn try_merge_abort(&self) -> Result<()> {
        self.output_allowing_status(["merge", "--abort"], &[1, 128])
            .map(|_| ())
    }

    /// Completes an in-progress merge after conflict resolution, preserving
    /// the original author.
    pub fn commit_no_edit_with_author(&self, author: &CommitAuthor) -> Result<()> {
        self.output_with(["commit", "--no-edit"], &author.env(), None)
            .map(|_| ())
    }

    pub fn try_cherry_pick_abort(&self) -> Result<()> {
        self.output_allowing_status(["cherry-pick", "--abort"], &[1, 128])
            .map(|_| ())
    }

    pub fn unmerged_entries(&self) -> Result<Vec<String>> {
        Ok(self
            .output(["ls-files", "-u"])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    pub fn update_ref(&self, refname: &str, new_value: &str) -> Result<()> {
        self.run(["update-ref", refname, new_value])
    }

    pub fn delete_ref(&self, refname: &str) -> Result<()> {
        self.run(["update-ref", "-d", refname])
    }

    pub fn refs_under(&self, namespace: &str) -> Result<Vec<String>> {
        Ok(self
            .output(["for-each-ref", "--format=%(refname)", namespace])?
            .lines()
            .map(str::to_owned)
            .collect())
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
