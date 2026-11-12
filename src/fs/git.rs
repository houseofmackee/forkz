//! Git status for the Git column: the repository a folder is in, and what
//! `git status` says about each item. A folder shows the most important
//! status of anything inside it. Also the Git menu's actions: stage,
//! unstage, commit, pull, push, and switching branches.
//!
//! Uses the git command: Homebrew's, or Apple's when the command line tools
//! are installed (otherwise `/usr/bin/git` would offer to install them, so
//! the column stays empty instead).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    // In order of importance (a folder shows its most important).
    Ignored,
    Untracked,
    Renamed,
    Added,
    Deleted,
    Modified,
    Conflicted,
}

impl Status {
    pub fn title(self) -> &'static str {
        match self {
            Status::Ignored => "Ignored",
            Status::Untracked => "Untracked",
            Status::Renamed => "Renamed",
            Status::Added => "Added",
            Status::Deleted => "Deleted",
            Status::Modified => "Modified",
            Status::Conflicted => "Conflict",
        }
    }
}

/// The repository's top folder, if `folder` is in one.
pub fn repository_root(folder: &Path) -> Option<PathBuf> {
    folder.ancestors().find(|dir| dir.join(".git").exists()).map(Path::to_path_buf)
}

/// The git command to run, if there is a usable one.
fn git() -> Option<PathBuf> {
    for candidate in ["/opt/homebrew/bin/git", "/usr/local/bin/git"] {
        if Path::new(candidate).exists() {
            return Some(candidate.into());
        }
    }
    // Apple's git only with the command line tools (or Xcode) installed.
    let tools = Command::new("/usr/bin/xcode-select").arg("-p").output().ok()?;
    tools.status.success().then(|| "/usr/bin/git".into())
}

/// Runs git in a repository, never waiting for input: no password
/// prompts (credentials come from the keychain helper or the SSH agent),
/// no editor (merge messages are git's own). Its output, or its error
/// message.
pub fn run(root: &Path, args: &[&str], input: Option<&str>) -> io::Result<String> {
    let git = git().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "git is not installed (install the command line tools or Homebrew's git)",
        )
    })?;
    let mut command = Command::new(git);
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .stdin(if input.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // SSH may not ask for passphrases either (the agent has the keys).
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let mut child = command.spawn()?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write;
        stdin.write_all(text.as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        let error = if error.trim().is_empty() { String::from_utf8_lossy(&output.stdout) } else { error };
        Err(io::Error::other(error.trim().to_string()))
    }
}

/// The current branch and how it stands against its upstream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Branch {
    /// None when not on a branch (a detached HEAD).
    pub name: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

impl Branch {
    /// "main", "main \u{2191}2 \u{2193}1", or "detached".
    pub fn summary(&self) -> String {
        let mut text = self.name.clone().unwrap_or_else(|| "detached".into());
        if self.ahead > 0 {
            text.push_str(&format!(" \u{2191}{}", self.ahead));
        }
        if self.behind > 0 {
            text.push_str(&format!(" \u{2193}{}", self.behind));
        }
        text
    }
}

fn parse_branch(output: &str) -> Branch {
    let mut branch = Branch::default();
    for line in output.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            branch.name = (head != "(detached)").then(|| head.to_string());
        } else if let Some(upstream) = line.strip_prefix("# branch.upstream ") {
            branch.upstream = Some(upstream.to_string());
        } else if let Some(counts) = line.strip_prefix("# branch.ab ") {
            for count in counts.split_whitespace() {
                if let Some(n) = count.strip_prefix('+') {
                    branch.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = count.strip_prefix('-') {
                    branch.behind = n.parse().unwrap_or(0);
                }
            }
        }
    }
    branch
}

/// The current branch of a repository.
pub fn branch(root: &Path) -> io::Result<Branch> {
    let output = run_quietly(root, &["status", "--porcelain=v2", "--branch", "--untracked-files=no"])?;
    Ok(parse_branch(&output))
}

/// For reading only: no lock that could get in the way of other tools.
fn run_quietly(root: &Path, args: &[&str]) -> io::Result<String> {
    let git = git().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git is not installed"))?;
    let output = Command::new(git).arg("-C").arg(root).args(args).env("GIT_OPTIONAL_LOCKS", "0").output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_string()))
    }
}

/// The local branches, by name.
pub fn branches(root: &Path) -> io::Result<Vec<String>> {
    let output = run_quietly(root, &["branch", "--format=%(refname:short)"])?;
    Ok(output.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
}

/// What a commit would take: changes staged, and changes not staged
/// (untracked files included).
pub fn change_counts(root: &Path) -> io::Result<(usize, usize)> {
    let output = run_quietly(root, &["status", "--porcelain=v1", "-z"])?;
    let mut staged = 0;
    let mut unstaged = 0;
    for record in output.split('\0').filter(|r| r.len() >= 3) {
        let bytes = record.as_bytes();
        let (x, y) = (bytes[0], bytes[1]);
        if x == b'?' {
            unstaged += 1;
            continue;
        }
        if x != b' ' && x != b'!' {
            staged += 1;
        }
        if y != b' ' && y != b'!' {
            unstaged += 1;
        }
    }
    Ok((staged, unstaged))
}

/// Paths as git takes them: relative to the repository (`--` keeps names
/// that start with "-" from being options).
fn pathspecs(root: &Path, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|p| p.strip_prefix(root).unwrap_or(p).to_string_lossy().into_owned())
        .map(|p| if p.is_empty() { ".".to_string() } else { p })
        .collect()
}

/// Stages items (all changes in them, deletions too).
pub fn stage(root: &Path, paths: &[PathBuf]) -> io::Result<()> {
    let specs = pathspecs(root, paths);
    let mut args = vec!["add", "--all", "--"];
    args.extend(specs.iter().map(String::as_str));
    run(root, &args, None).map(|_| ())
}

/// Unstages items (their changes stay in the files).
pub fn unstage(root: &Path, paths: &[PathBuf]) -> io::Result<()> {
    let specs = pathspecs(root, paths);
    let mut args = vec!["restore", "--staged", "--"];
    args.extend(specs.iter().map(String::as_str));
    match run(root, &args, None) {
        Ok(_) => Ok(()),
        // Before the first commit there is nothing to restore from.
        Err(_) => {
            let mut args = vec!["rm", "--cached", "-r", "-q", "--"];
            args.extend(specs.iter().map(String::as_str));
            run(root, &args, None).map(|_| ())
        }
    }
}

/// Commits what is staged (everything, first, if `all`).
pub fn commit(root: &Path, message: &str, all: bool) -> io::Result<String> {
    if all {
        run(root, &["add", "--all"], None)?;
    }
    run(root, &["commit", "--file=-"], Some(message))
}

/// Pulls from the branch's upstream.
pub fn pull(root: &Path) -> io::Result<String> {
    run(root, &["pull"], None)
}

/// Pushes the branch; the first push of a branch sets its upstream (on
/// "origin", or the only remote).
pub fn push(root: &Path) -> io::Result<String> {
    let current = branch(root)?;
    let Some(name) = current.name else {
        return Err(io::Error::other("Not on a branch: switch to one to push."));
    };
    if current.upstream.is_some() {
        return run(root, &["push"], None);
    }
    let remotes = run_quietly(root, &["remote"])?;
    let remotes: Vec<&str> = remotes.lines().map(str::trim).filter(|r| !r.is_empty()).collect();
    let remote = match remotes.as_slice() {
        [] => return Err(io::Error::other("This repository has no remote to push to.")),
        [one] => one.to_string(),
        many if many.contains(&"origin") => "origin".to_string(),
        _ => return Err(io::Error::other("The branch has no upstream, and there are several remotes to choose from.")),
    };
    run(root, &["push", "--set-upstream", &remote, &name], None)
}

/// Switches to a branch, or makes a new one there (`create`).
pub fn switch(root: &Path, name: &str, create: bool) -> io::Result<String> {
    // git's own rules for branch names (none start with "-", so the name
    // cannot be taken for an option).
    if name.starts_with('-') || run_quietly(root, &["check-ref-format", "--branch", name]).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("\u{201c}{name}\u{201d} is not a valid branch name."),
        ));
    }
    let mut args = vec!["switch"];
    if create {
        args.push("--create");
    }
    args.push(name);
    run(root, &args, None)
}

/// What `git status` says, by path (folders that are untracked or ignored
/// as a whole are listed once, as the folder).
pub fn status(root: &Path) -> io::Result<Statuses> {
    let git = git().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "git is not installed"))?;
    let output = Command::new(git)
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--ignored", "--no-renames"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_string()));
    }
    Ok(parse(root, &output.stdout))
}

#[derive(Debug, Default, Clone)]
pub struct Statuses {
    files: HashMap<PathBuf, Status>,
    /// Folders listed as a whole ("dir/"): untracked or ignored.
    folders: Vec<(PathBuf, Status)>,
}

fn parse(root: &Path, output: &[u8]) -> Statuses {
    let mut statuses = Statuses::default();
    for record in output.split(|b| *b == 0) {
        if record.len() < 4 {
            continue;
        }
        let (x, y) = (record[0], record[1]);
        let name = String::from_utf8_lossy(&record[3..]);
        let status = match (x, y) {
            (b'?', b'?') => Status::Untracked,
            (b'!', b'!') => Status::Ignored,
            (b'D', b'D') | (b'A', b'A') | (b'U', _) | (_, b'U') => Status::Conflicted,
            (b'R', _) | (_, b'R') => Status::Renamed,
            (b'A', _) => Status::Added,
            (b'D', _) | (_, b'D') => Status::Deleted,
            _ => Status::Modified,
        };
        match name.strip_suffix('/') {
            Some(folder) => statuses.folders.push((root.join(folder), status)),
            None => {
                statuses.files.insert(root.join(name.as_ref()), status);
            }
        }
    }
    statuses
}

impl Statuses {
    /// An item's status: a file's own, or for a folder the most important
    /// of what is inside (or of the folder as a whole).
    pub fn of(&self, path: &Path, is_folder: bool) -> Option<Status> {
        // Inside a folder that is untracked or ignored as a whole.
        let whole = self.folders.iter().filter(|(dir, _)| path.starts_with(dir)).map(|(_, s)| *s).max();
        if !is_folder {
            return self.files.get(path).copied().or(whole);
        }
        let inside = self.files.iter().filter(|(file, _)| file.starts_with(path)).map(|(_, s)| *s);
        let below = self.folders.iter().filter(|(dir, _)| dir.starts_with(path) && dir != path).map(|(_, s)| *s);
        let most = inside.chain(below).filter(|s| *s != Status::Ignored).max();
        most.or(whole)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.folders.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_output() {
        let root = Path::new("/repo");
        let output = b" M src/main.rs\0A  new.rs\0?? scratch/\0!! target/\0UU both.rs\0 D gone.txt\0?? notes.md\0";
        let s = parse(root, output);
        assert_eq!(s.of(Path::new("/repo/src/main.rs"), false), Some(Status::Modified));
        assert_eq!(s.of(Path::new("/repo/new.rs"), false), Some(Status::Added));
        assert_eq!(s.of(Path::new("/repo/both.rs"), false), Some(Status::Conflicted));
        assert_eq!(s.of(Path::new("/repo/gone.txt"), false), Some(Status::Deleted));
        assert_eq!(s.of(Path::new("/repo/notes.md"), false), Some(Status::Untracked));
        assert_eq!(s.of(Path::new("/repo/README.md"), false), None);
        // Folders: what is inside, or the folder as a whole.
        assert_eq!(s.of(Path::new("/repo/src"), true), Some(Status::Modified));
        assert_eq!(s.of(Path::new("/repo/scratch"), true), Some(Status::Untracked));
        assert_eq!(s.of(Path::new("/repo/scratch/a.txt"), false), Some(Status::Untracked));
        assert_eq!(s.of(Path::new("/repo/target"), true), Some(Status::Ignored));
        assert_eq!(s.of(Path::new("/repo/target/debug/x"), false), Some(Status::Ignored));
        // The top shows the most important change (an ignored folder aside).
        assert_eq!(s.of(Path::new("/repo"), true), Some(Status::Conflicted));
    }

    #[test]
    fn reads_the_branch() {
        let output = "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -1\n";
        let branch = parse_branch(output);
        assert_eq!(branch.name.as_deref(), Some("main"));
        assert_eq!(branch.upstream.as_deref(), Some("origin/main"));
        assert_eq!((branch.ahead, branch.behind), (2, 1));
        assert_eq!(branch.summary(), "main \u{2191}2 \u{2193}1");
        let detached = parse_branch("# branch.oid abc\n# branch.head (detached)\n");
        assert_eq!(detached.summary(), "detached");
        assert_eq!(parse_branch("# branch.head dev\n").summary(), "dev");
    }

    /// Stage, unstage, commit, branches, and push and pull with a local
    /// "server" (a bare repository).
    #[test]
    fn actions_in_a_real_repository() {
        let Some(_) = git() else { return };
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let (server, mine, theirs) = (base.join("server.git"), base.join("mine"), base.join("theirs"));
        let identity = |root: &Path| {
            run(root, &["config", "user.name", "Test"], None).unwrap();
            run(root, &["config", "user.email", "test@example.com"], None).unwrap();
        };
        run(&base, &["init", "-q", "--bare", "-b", "main", "server.git"], None).unwrap();
        run(&base, &["init", "-q", "-b", "main", "mine"], None).unwrap();
        identity(&mine);
        run(&mine, &["remote", "add", "origin", &server.to_string_lossy()], None).unwrap();

        std::fs::write(mine.join("a.txt"), "one").unwrap();
        std::fs::write(mine.join("-dash.txt"), "dash").unwrap();
        assert_eq!(change_counts(&mine).unwrap(), (0, 2));
        stage(&mine, &[mine.join("a.txt"), mine.join("-dash.txt")]).unwrap();
        assert_eq!(change_counts(&mine).unwrap(), (2, 0));
        // Before the first commit, unstaging still works.
        unstage(&mine, &[mine.join("-dash.txt")]).unwrap();
        assert_eq!(change_counts(&mine).unwrap(), (1, 1));
        commit(&mine, "First\n\nWith a second paragraph.", false).unwrap();
        assert_eq!(change_counts(&mine).unwrap(), (0, 1));
        let message = run(&mine, &["log", "-1", "--format=%B"], None).unwrap();
        assert_eq!(message.trim(), "First\n\nWith a second paragraph.");
        commit(&mine, "Second", true).unwrap();
        assert_eq!(change_counts(&mine).unwrap(), (0, 0));
        assert!(commit(&mine, "Nothing", false).is_err(), "nothing to commit");

        // The first push sets the upstream.
        assert_eq!(branch(&mine).unwrap().upstream, None);
        push(&mine).unwrap();
        let after = branch(&mine).unwrap();
        assert_eq!((after.upstream.as_deref(), after.ahead), (Some("origin/main"), 0));

        // Someone else pushes; pulling brings it.
        run(&base, &["clone", "-q", &server.to_string_lossy(), "theirs"], None).unwrap();
        identity(&theirs);
        std::fs::write(theirs.join("b.txt"), "two").unwrap();
        commit(&theirs, "Theirs", true).unwrap();
        push(&theirs).unwrap();
        pull(&mine).unwrap();
        assert!(mine.join("b.txt").exists());

        // Branches.
        switch(&mine, "feature", true).unwrap();
        assert_eq!(branch(&mine).unwrap().name.as_deref(), Some("feature"));
        assert_eq!(branches(&mine).unwrap(), ["feature", "main"]);
        switch(&mine, "main", false).unwrap();
        assert_eq!(branch(&mine).unwrap().summary(), "main");
        assert!(switch(&mine, "nope", false).is_err());
        assert_eq!(switch(&mine, "-x", true).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(switch(&mine, "two words", true).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn real_repository() {
        let Some(_) = git() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let run = |args: &[&str]| {
            let ok = Command::new(git().unwrap()).arg("-C").arg(&root).args(args).output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::write(root.join("kept.txt"), "1").unwrap();
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        run(&["add", "."]);
        run(&["-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "first"]);
        std::fs::write(root.join("kept.txt"), "2").unwrap();
        std::fs::write(root.join("fresh.txt"), "").unwrap();
        std::fs::create_dir(root.join("build")).unwrap();
        std::fs::write(root.join("build/out.o"), "").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        assert_eq!(repository_root(&root.join("sub")), Some(root.clone()));
        let s = status(&root).unwrap();
        assert_eq!(s.of(&root.join("kept.txt"), false), Some(Status::Modified));
        assert_eq!(s.of(&root.join("fresh.txt"), false), Some(Status::Untracked));
        assert_eq!(s.of(&root.join("build"), true), Some(Status::Ignored));
        assert_eq!(s.of(&root.join(".gitignore"), false), None);
    }
}
