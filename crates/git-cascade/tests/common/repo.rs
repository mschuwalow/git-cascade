use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;

use git_cascade::storage::PlanName;
use tempfile::TempDir;

pub struct TestRepo {
    root: TempDir,
    home: TempDir,
}

impl TestRepo {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let repo = Self { root, home };
        repo.git_ok(["init", "-b", "main"]);
        repo.git_ok(["config", "user.name", "Test Author"]);
        repo.git_ok(["config", "user.email", "test@example.com"]);
        repo.git_ok(["config", "commit.gpgsign", "false"]);
        repo.git_ok(["config", "tag.gpgsign", "false"]);
        repo
    }

    pub fn path(&self) -> &Path {
        self.root.path()
    }

    pub fn cascade(&self) -> assert_cmd::Command {
        let mut command = assert_cmd::Command::cargo_bin("git-cascade").unwrap();
        self.configure_command(&mut command);
        command
    }

    pub fn git_output<I, S>(&self, args: I) -> String
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new("git");
        self.configure_command(&mut command);
        let output = command.args(args).output().unwrap();

        assert!(
            output.status.success(),
            "git command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        String::from_utf8(output.stdout).unwrap()
    }

    pub fn git_ok<I, S>(&self, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.git_output(args);
    }

    pub fn write(&self, path: &str, contents: &str) {
        let path = self.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    pub fn commit_file(&self, path: &str, contents: &str, message: &str) -> String {
        self.write(path, contents);
        self.git_ok(["add", path]);
        self.git_ok(["commit", "-m", message]);
        self.rev_parse("HEAD")
    }

    pub fn switch(&self, branch: &str) {
        self.git_ok(["switch", branch]);
    }

    pub fn switch_new(&self, branch: &str) {
        self.git_ok(["switch", "-c", branch]);
    }

    pub fn switch_new_at(&self, branch: &str, start_point: &str) {
        self.git_ok(["switch", "-c", branch, start_point]);
    }

    pub fn rev_parse(&self, rev: &str) -> String {
        self.git_output(["rev-parse", rev]).trim().to_owned()
    }

    pub fn common_dir(&self) -> std::path::PathBuf {
        self.git_output(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .trim()
            .into()
    }

    pub fn named_plan_path(&self, name: &str) -> std::path::PathBuf {
        let name = PlanName::new(name).unwrap();
        self.common_dir()
            .join("cascade")
            .join("plans")
            .join(format!("{}.yaml", name.encoded()))
    }

    fn configure_command<'a, C>(&self, command: &'a mut C) -> &'a mut C
    where
        C: CommandLike,
    {
        command
            .current_dir(self.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("GIT_AUTHOR_NAME", "Test Author")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test Author")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
    }
}

trait CommandLike {
    fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self;
    fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, key: K, value: V) -> &mut Self;
}

impl CommandLike for Command {
    fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        Command::current_dir(self, dir)
    }

    fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, key: K, value: V) -> &mut Self {
        Command::env(self, key, value)
    }
}

impl CommandLike for assert_cmd::Command {
    fn current_dir<P: AsRef<Path>>(&mut self, dir: P) -> &mut Self {
        assert_cmd::Command::current_dir(self, dir)
    }

    fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, key: K, value: V) -> &mut Self {
        assert_cmd::Command::env(self, key, value)
    }
}
