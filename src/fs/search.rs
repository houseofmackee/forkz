//! Searching a folder and everything in it, by name or by contents: with
//! Spotlight on the Mac (fast, for indexed folders), or by walking the
//! folders through any file system (servers and archives too).

use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::Entry;
use super::glob::Patterns;
use super::ops::CancelToken;
use crate::vfs::FsRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Names containing the text, or matching it if it has wildcards.
    Name,
    /// Files whose contents contain the text (ignoring case).
    Contents,
}

#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub mode: Mode,
    pub include_hidden: bool,
    /// Only items of this kind.
    pub kind: Kind,
    /// Only items with this Finder tag (on the Mac).
    pub tag: Option<String>,
}

/// Kinds of items to search for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Any,
    Folders,
    Files,
    Images,
    Movies,
    Audio,
    Documents,
    Archives,
    Code,
}

impl Kind {
    pub const ALL: [(Kind, &'static str); 9] = [
        (Kind::Any, "Any kind"),
        (Kind::Folders, "Folders"),
        (Kind::Files, "Files"),
        (Kind::Images, "Images"),
        (Kind::Movies, "Movies"),
        (Kind::Audio, "Music and audio"),
        (Kind::Documents, "Documents"),
        (Kind::Archives, "Archives"),
        (Kind::Code, "Source code"),
    ];

    fn extensions(self) -> &'static [&'static str] {
        match self {
            Kind::Images => &[
                "jpg", "jpeg", "png", "gif", "heic", "heif", "tif", "tiff", "bmp", "webp", "svg", "raw", "cr2", "cr3",
                "nef", "arw", "dng", "psd", "ico", "avif",
            ],
            Kind::Movies => &["mov", "mp4", "m4v", "avi", "mkv", "webm", "wmv", "mpg", "mpeg", "3gp"],
            Kind::Audio => &["mp3", "m4a", "aac", "wav", "aif", "aiff", "flac", "ogg", "opus", "alac", "caf"],
            Kind::Documents => &[
                "pdf", "doc", "docx", "pages", "txt", "rtf", "md", "odt", "xls", "xlsx", "numbers", "csv", "ppt",
                "pptx", "key", "epub", "html", "htm",
            ],
            Kind::Archives => &["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "dmg", "iso", "zst"],
            Kind::Code => &[
                "rs", "py", "js", "ts", "tsx", "jsx", "swift", "c", "h", "cpp", "hpp", "m", "mm", "go", "java", "kt",
                "rb", "php", "sh", "zsh", "toml", "yaml", "yml", "json", "css", "scss", "sql", "lua", "cs",
            ],
            _ => &[],
        }
    }

    /// Whether an item is of this kind (by its extension).
    pub fn matches(self, entry: &Entry) -> bool {
        match self {
            Kind::Any => true,
            Kind::Folders => entry.is_folder_like(),
            Kind::Files => !entry.is_folder_like(),
            kind => {
                !entry.is_folder_like()
                    && Path::new(&entry.name)
                        .extension()
                        .is_some_and(|e| kind.extensions().contains(&e.to_string_lossy().to_lowercase().as_str()))
            }
        }
    }

    /// The Spotlight condition (None: none needed).
    fn spotlight(self) -> Option<&'static str> {
        Some(match self {
            Kind::Any => return None,
            Kind::Folders => "kMDItemContentType == \"public.folder\"",
            Kind::Files => "kMDItemContentType != \"public.folder\"",
            Kind::Images => "kMDItemContentTypeTree == \"public.image\"",
            Kind::Movies => "kMDItemContentTypeTree == \"public.movie\"",
            Kind::Audio => "kMDItemContentTypeTree == \"public.audio\"",
            Kind::Documents => {
                "(kMDItemContentTypeTree == \"public.composite-content\" || kMDItemContentTypeTree == \"public.text\")"
            }
            Kind::Archives => "kMDItemContentTypeTree == \"public.archive\"",
            Kind::Code => "kMDItemContentTypeTree == \"public.source-code\"",
        })
    }
}

impl Query {
    /// A search needs some text or a filter.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.kind == Kind::Any && self.tag.is_none()
    }

    /// The kind and tag filters (the text is checked separately).
    fn filters_match(&self, fs: &FsRef, entry: &Entry) -> bool {
        if !self.kind.matches(entry) {
            return false;
        }
        match &self.tag {
            None => true,
            Some(tag) => {
                fs.is_local() && super::tags::read(&entry.path).iter().any(|t| t.name.eq_ignore_ascii_case(tag))
            }
        }
    }
}

/// Results stop here.
pub const LIMIT: usize = 10_000;

/// Files larger than this are not searched for contents (when walking).
const CONTENT_LIMIT: u64 = 20 * 1024 * 1024;

/// Searches by walking the folders. `cancel` stops it early (with what
/// was found so far).
pub fn walk(fs: &FsRef, root: &Path, query: &Query, cancel: &CancelToken) -> io::Result<Vec<Entry>> {
    let patterns = Patterns::new(&query.text);
    let needle = query.text.to_lowercase();
    let mut found = Vec::new();
    let mut folders = VecDeque::from([root.to_path_buf()]);
    let mut first = true;
    while let Some(folder) = folders.pop_front() {
        if cancel.is_cancelled() || found.len() >= LIMIT {
            break;
        }
        let entries = match fs.list(&folder) {
            Ok(entries) => entries,
            // The folder searched must be readable; others are skipped.
            Err(e) if first => return Err(e),
            Err(_) => continue,
        };
        first = false;
        for entry in entries {
            if entry.hidden && !query.include_hidden {
                continue;
            }
            if cancel.is_cancelled() {
                break;
            }
            let text_hit = match query.mode {
                Mode::Name => patterns.is_empty() || patterns.matches(&entry.name),
                Mode::Contents => !entry.is_folder_like() && (needle.is_empty() || contains(fs, &entry, &needle)),
            };
            let hit = text_hit && query.filters_match(fs, &entry);
            if entry.is_folder_like() {
                folders.push_back(entry.path.clone());
            }
            if hit {
                found.push(entry);
            }
        }
    }
    Ok(found)
}

/// Whether a file contains the (lowercase) text.
fn contains(fs: &FsRef, entry: &Entry, needle: &str) -> bool {
    if needle.is_empty() || entry.size > CONTENT_LIMIT {
        return false;
    }
    let mut data = Vec::new();
    let Ok(mut reader) = fs.open_read(&entry.path) else { return false };
    if reader.read_to_end(&mut data).is_err() {
        return false;
    }
    String::from_utf8_lossy(&data).to_lowercase().contains(needle)
}

/// The Spotlight query for `mdfind` (a raw query expression).
pub fn spotlight_query(query: &Query) -> String {
    let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    let mut parts = Vec::new();
    if !query.text.trim().is_empty() {
        parts.push(format!("({})", text_query(query)));
    }
    if let Some(kind) = query.kind.spotlight() {
        parts.push(kind.to_string());
    }
    if let Some(tag) = &query.tag {
        parts.push(format!("kMDItemUserTags == \"{}\"cd", escape(tag)));
    }
    if parts.len() == 1 && !query.text.trim().is_empty() {
        // Just the text: as before, without brackets.
        return text_query(query);
    }
    parts.join(" && ")
}

fn text_query(query: &Query) -> String {
    let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    match query.mode {
        Mode::Name => {
            let names: Vec<String> = query
                .text
                .split([';', ','])
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| if p.contains(['*', '?']) { p.to_string() } else { format!("*{p}*") })
                .map(|p| format!("kMDItemFSName == \"{}\"cd", escape(&p)))
                .collect();
            names.join(" || ")
        }
        Mode::Contents => format!("kMDItemTextContent == \"*{}*\"cd", escape(query.text.trim())),
    }
}

/// Everything Spotlight knows with this Finder tag, on every volume.
pub fn spotlight_tag(tag: &str, cancel: &CancelToken) -> io::Result<Vec<PathBuf>> {
    let query = format!("kMDItemUserTags == \"{}\"cd", tag.replace('\\', "\\\\").replace('"', "\\\""));
    let text = mdfind(&[query], cancel)?;
    Ok(text.lines().filter(|l| !l.is_empty()).map(PathBuf::from).take(LIMIT).collect())
}

/// Runs mdfind; `cancel` stops it (an Interrupted error).
fn mdfind(args: &[String], cancel: &CancelToken) -> io::Result<String> {
    let mut child = Command::new("/usr/bin/mdfind")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    // Read as it comes (a full pipe would block mdfind).
    let mut stdout = child.stdout.take().ok_or_else(|| io::Error::other("no output from mdfind"))?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).map(|_| text)
    });
    loop {
        if let Some(status) = child.try_wait()? {
            let text = reader.join().map_err(|_| io::Error::other("mdfind output"))??;
            if !status.success() {
                let mut error = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut error);
                }
                return Err(io::Error::other(error.trim().to_string()));
            }
            return Ok(text);
        }
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Searches with Spotlight (only what it has indexed). Returns paths.
pub fn spotlight(root: &Path, query: &Query, cancel: &CancelToken) -> io::Result<Vec<PathBuf>> {
    let args = ["-onlyin".to_string(), root.to_string_lossy().into_owned(), spotlight_query(query)];
    let text = mdfind(&args, cancel)?;
    Ok(text
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .filter(|p| p != root)
        .filter(|p| {
            query.include_hidden
                || !p
                    .strip_prefix(root)
                    .unwrap_or(p)
                    .components()
                    .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        })
        .take(LIMIT)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/deep")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("notes.txt"), "Remember the Milk").unwrap();
        std::fs::write(root.join("a/report.pdf"), "binary-ish").unwrap();
        std::fs::write(root.join("a/deep/Notes 2.md"), "nothing here").unwrap();
        std::fs::write(root.join(".hidden/notes.txt"), "milk").unwrap();
        dir
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        let mut names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        names.sort();
        names
    }

    #[test]
    fn walks_by_name_and_contents() {
        let dir = tree();
        let fs = crate::vfs::local();
        let cancel = CancelToken::default();
        let by_name =
            Query { text: "notes".into(), mode: Mode::Name, include_hidden: false, kind: Kind::Any, tag: None };
        assert_eq!(names(&walk(&fs, dir.path(), &by_name, &cancel).unwrap()), ["Notes 2.md", "notes.txt"]);
        let hidden = Query { include_hidden: true, ..by_name.clone() };
        assert_eq!(walk(&fs, dir.path(), &hidden, &cancel).unwrap().len(), 3);
        let pattern =
            Query { text: "*.pdf; deep".into(), mode: Mode::Name, include_hidden: false, kind: Kind::Any, tag: None };
        assert_eq!(names(&walk(&fs, dir.path(), &pattern, &cancel).unwrap()), ["deep", "report.pdf"]);
        let contents =
            Query { text: "MILK".into(), mode: Mode::Contents, include_hidden: false, kind: Kind::Any, tag: None };
        assert_eq!(names(&walk(&fs, dir.path(), &contents, &cancel).unwrap()), ["notes.txt"]);
        assert!(walk(&fs, &dir.path().join("missing"), &by_name, &cancel).is_err());
        cancel.cancel();
        assert!(walk(&fs, dir.path(), &by_name, &cancel).unwrap().is_empty());
    }

    #[test]
    fn filters_by_kind_and_tag() {
        let dir = tree();
        std::fs::write(dir.path().join("photo.JPG"), "x").unwrap();
        std::fs::write(dir.path().join("a/song.mp3"), "x").unwrap();
        crate::fs::tags::set(&dir.path().join("notes.txt"), &["Red".into()]).unwrap();
        let fs = crate::vfs::local();
        let cancel = CancelToken::default();
        let query = |kind, tag: Option<&str>| Query {
            text: String::new(),
            mode: Mode::Name,
            include_hidden: false,
            kind,
            tag: tag.map(str::to_string),
        };
        assert_eq!(names(&walk(&fs, dir.path(), &query(Kind::Images, None), &cancel).unwrap()), ["photo.JPG"]);
        assert_eq!(names(&walk(&fs, dir.path(), &query(Kind::Audio, None), &cancel).unwrap()), ["song.mp3"]);
        assert_eq!(names(&walk(&fs, dir.path(), &query(Kind::Folders, None), &cancel).unwrap()), ["a", "deep"]);
        assert_eq!(names(&walk(&fs, dir.path(), &query(Kind::Any, Some("red")), &cancel).unwrap()), ["notes.txt"]);
        let docs_named = Query { text: "notes".into(), ..query(Kind::Documents, None) };
        assert_eq!(names(&walk(&fs, dir.path(), &docs_named, &cancel).unwrap()), ["Notes 2.md", "notes.txt"]);
        assert!(query(Kind::Any, None).is_empty());
        let spotlight = spotlight_query(&Query { text: "trip".into(), ..query(Kind::Images, Some("Blue")) });
        assert_eq!(
            spotlight,
            r#"(kMDItemFSName == "*trip*"cd) && kMDItemContentTypeTree == "public.image" && kMDItemUserTags == "Blue"cd"#
        );
    }

    #[test]
    fn spotlight_queries() {
        let q =
            Query { text: "*.jpg; notes".into(), mode: Mode::Name, include_hidden: false, kind: Kind::Any, tag: None };
        assert_eq!(spotlight_query(&q), r#"kMDItemFSName == "*.jpg"cd || kMDItemFSName == "*notes*"cd"#);
        let q = Query {
            text: r#"say "hi""#.into(),
            mode: Mode::Contents,
            include_hidden: false,
            kind: Kind::Any,
            tag: None,
        };
        assert_eq!(spotlight_query(&q), r#"kMDItemTextContent == "*say \"hi\"*"cd"#);
    }
}
