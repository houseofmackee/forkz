//! Git status for the Git column: the repository a folder is in, and what
//! `git status` says about each item. A folder shows the most important
//! status of anything inside it.
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
