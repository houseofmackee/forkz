//! `forkz://` links, for Shortcuts, scripts, launchers, and web pages:
//!
//! - `forkz://open?path=~/Downloads` (a folder, or a file to select; with
//!   `pane=left|right|other` and `tab=new`)
//! - `forkz://connect?site=Staging` (a saved site, by name; `path=/var/www`)
//! - `forkz://search?path=~/Documents&text=invoice&kind=documents` (also
//!   `larger=100MB`, `smaller=1MB`, and `days=7`: modified in the last 7)
//! - `forkz://synclet?name=Website`, `forkz://smart-folder?name=Photos`,
//!   `forkz://workspace?name=Work`
//! - `forkz://command?name=ComparePanes` (a command, by its id; commands
//!   that delete or move items are not allowed)
//!
//! Values are percent-encoded (a space is `%20`).

use std::path::PathBuf;

use percent_encoding::percent_decode_str;

/// A search link's size and date conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Filters {
    pub larger: Option<u64>,
    pub smaller: Option<u64>,
    /// Modified in the last so many days.
    pub days: Option<u32>,
}

/// Which pane a link acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    Active,
    Other,
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Open { path: PathBuf, pane: Pane, new_tab: bool },
    Connect { site: String, path: Option<String>, pane: Pane },
    Search { path: PathBuf, text: String, kind: Option<String>, contents: bool, filters: Filters, pane: Pane },
    Synclet(String),
    SmartFolder { name: String, pane: Pane },
    Workspace(String),
    Command(String),
}

/// Commands a link may not run: they delete, move, or overwrite items, and
/// a web page can open links too.
pub const UNSAFE_COMMANDS: &[&str] =
    &["Trash", "DeleteImmediately", "MoveToOther", "MoveFilesHere", "PasteFiles", "Disconnect", "CloseTab"];

/// Reads a `forkz://` link. The error says what is wrong, for an alert.
pub fn parse(link: &str) -> Result<Link, String> {
    let rest = link
        .strip_prefix("forkz://")
        .or_else(|| link.strip_prefix("forkz:"))
        .ok_or_else(|| "It is not a forkz:// link.".to_string())?;
    let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
    let action = action.trim_matches('/').to_lowercase();
    let mut params: Vec<(String, String)> = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
        params.push((decode(key).to_lowercase(), decode(value)));
    }
    let get = |key: &str| params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
    let need = |key: &str| get(key).ok_or_else(|| format!("The link needs a {key}= value."));
    let pane = match get("pane").as_deref().map(str::to_lowercase).as_deref() {
        None | Some("active") => Pane::Active,
        Some("other") => Pane::Other,
        Some("left") | Some("top") => Pane::Left,
        Some("right") | Some("bottom") => Pane::Right,
        Some(other) => {
            return Err(format!("\u{201c}{other}\u{201d} is not a pane: use left, right, other, or active."));
        }
    };
    let yes = |key: &str| get(key).is_some_and(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "new"));
    Ok(match action.as_str() {
        "open" | "reveal" | "go" => Link::Open { path: expand(&need("path")?), pane, new_tab: yes("tab") },
        "connect" => Link::Connect { site: need("site")?, path: get("path"), pane },
        "search" => {
            let size = |key: &str| match get(key) {
                None => Ok(None),
                Some(v) => crate::fs::split::parse_size(&v)
                    .map(Some)
                    .ok_or_else(|| format!("\u{201c}{v}\u{201d} is not a size: write it as 100MB or 2GB.")),
            };
            let days = match get("days") {
                None => None,
                Some(v) => Some(
                    v.parse::<u32>()
                        .ok()
                        .filter(|d| *d > 0)
                        .ok_or_else(|| format!("\u{201c}{v}\u{201d} is not a number of days."))?,
                ),
            };
            Link::Search {
                path: expand(&need("path")?),
                text: get("text").unwrap_or_default(),
                kind: get("kind"),
                contents: yes("contents"),
                filters: Filters { larger: size("larger")?, smaller: size("smaller")?, days },
                pane,
            }
        }
        "synclet" | "sync" => Link::Synclet(need("name")?),
        "smart-folder" | "smartfolder" => Link::SmartFolder { name: need("name")?, pane },
        "workspace" => Link::Workspace(need("name")?),
        "command" | "run" => {
            let name = need("name")?;
            if UNSAFE_COMMANDS.iter().any(|c| c.eq_ignore_ascii_case(&name)) {
                return Err(format!("\u{201c}{name}\u{201d} cannot be run from a link."));
            }
            Link::Command(name)
        }
        "" => return Err("The link does not say what to do.".into()),
        other => return Err(format!("forkz does not know \u{201c}{other}\u{201d} links.")),
    })
}

/// "~" and "~/..." are the home folder.
fn expand(path: &str) -> PathBuf {
    // A server address ("sftp://host/path") stays as it is.
    if path.contains("://") {
        return PathBuf::from(path);
    }
    let home = || dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    match path.strip_prefix('~') {
        Some("") => home(),
        Some(rest) if rest.starts_with('/') => home().join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_links() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            parse("forkz://open?path=~/My%20Files&pane=right&tab=new"),
            Ok(Link::Open { path: home.join("My Files"), pane: Pane::Right, new_tab: true })
        );
        assert_eq!(
            parse("forkz://open/?path=/tmp"),
            Ok(Link::Open { path: "/tmp".into(), pane: Pane::Active, new_tab: false })
        );
        // A server address (from the forkz command) stays whole.
        assert_eq!(
            parse("forkz://open?path=sftp%3A%2F%2Fme%40host%2Fvar%2Fwww"),
            Ok(Link::Open { path: "sftp://me@host/var/www".into(), pane: Pane::Active, new_tab: false })
        );
        assert_eq!(
            parse("forkz://connect?site=Staging%20Server&path=/var/www"),
            Ok(Link::Connect { site: "Staging Server".into(), path: Some("/var/www".into()), pane: Pane::Active })
        );
        assert_eq!(
            parse("forkz://search?path=~&text=invoice&kind=documents&contents=1&pane=other"),
            Ok(Link::Search {
                path: home.clone(),
                text: "invoice".into(),
                kind: Some("documents".into()),
                contents: true,
                filters: Filters::default(),
                pane: Pane::Other
            })
        );
        assert_eq!(
            parse("forkz://search?path=/tmp&larger=1GB&days=7"),
            Ok(Link::Search {
                path: "/tmp".into(),
                text: String::new(),
                kind: None,
                contents: false,
                filters: Filters { larger: Some(1 << 30), smaller: None, days: Some(7) },
                pane: Pane::Active
            })
        );
        assert!(parse("forkz://search?path=/tmp&larger=huge").is_err());
        assert!(parse("forkz://search?path=/tmp&days=0").is_err());
        assert_eq!(parse("forkz://synclet?name=Web"), Ok(Link::Synclet("Web".into())));
        assert_eq!(
            parse("forkz://smart-folder?name=Rust"),
            Ok(Link::SmartFolder { name: "Rust".into(), pane: Pane::Active })
        );
        assert_eq!(parse("forkz:workspace?name=Work"), Ok(Link::Workspace("Work".into())));
        assert_eq!(parse("forkz://command?name=ComparePanes"), Ok(Link::Command("ComparePanes".into())));
    }

    #[test]
    fn refuses_bad_links() {
        assert!(parse("https://example.com").is_err());
        assert!(parse("forkz://").is_err());
        assert!(parse("forkz://open").unwrap_err().contains("path="));
        assert!(parse("forkz://open?path=/tmp&pane=middle").is_err());
        assert!(parse("forkz://explode?x=1").is_err());
        // Nothing that deletes or moves.
        assert!(parse("forkz://command?name=trash").is_err());
        assert!(parse("forkz://command?name=DeleteImmediately").is_err());
    }
}
