//! Sites from other apps, and forkz's own export file:
//!
//! - Cyberduck's bookmarks (`.duck` files: property lists).
//! - WinSCP's sessions (`WinSCP.ini`, for those coming from Windows).
//! - A forkz export (`Export Sites...`): the sites, without passwords.
//!
//! Passwords are not brought over: Cyberduck keeps them in the Keychain,
//! WinSCP scrambles them, and exports leave them out on purpose.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use percent_encoding::percent_decode_str;
use roxmltree::Document;
use serde::{Deserialize, Serialize};

use super::{Protocol, Site, new_id};
use crate::vfs::sftp::AuthMethod;

/// Sites found, and what could not be brought over.
#[derive(Debug, Default)]
pub struct Found {
    pub sites: Vec<Site>,
    pub notes: Vec<String>,
}

// Cyberduck.

/// Where Cyberduck keeps its bookmarks (the App Store and direct versions,
/// then older versions).
pub fn cyberduck_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    [
        "Library/Group Containers/G69SCX94XU.duck/Library/Application Support/duck/Bookmarks",
        "Library/Application Support/Cyberduck/Bookmarks",
    ]
    .iter()
    .map(|relative| home.join(relative))
    .find(|dir| dir.is_dir())
}

/// Reads Cyberduck bookmarks: `.duck` files, or folders of them.
pub fn cyberduck(paths: &[PathBuf]) -> Found {
    let mut found = Found::default();
    let mut files: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut inside: Vec<PathBuf> = std::fs::read_dir(path)
                .map(|items| items.filter_map(Result::ok).map(|i| i.path()).collect())
                .unwrap_or_default();
            inside.retain(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("duck")));
            inside.sort();
            files.extend(inside);
        } else {
            files.push(path.clone());
        }
    }
    for file in files {
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match plist_xml(&file).and_then(|xml| cyberduck_bookmark(&xml)) {
            Ok(Some(site)) => found.sites.push(site),
            Ok(None) => found.notes.push(format!("{name}: a kind of server forkz does not connect to")),
            Err(e) => found.notes.push(format!("{name}: {e}")),
        }
    }
    found
}

/// A property list as XML (binary ones are converted by plutil).
fn plist_xml(file: &Path) -> io::Result<String> {
    let text = std::fs::read(file)?;
    if text.starts_with(b"<?xml") || text.starts_with(b"<plist") {
        return Ok(String::from_utf8_lossy(&text).into_owned());
    }
    let output = Command::new("/usr/bin/plutil").args(["-convert", "xml1", "-o", "-"]).arg(file).output()?;
    if !output.status.success() {
        return Err(io::Error::other("not a bookmark file"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The top dictionary of a property list: keys with their string (or
/// number) values.
fn plist_strings(xml: &str) -> io::Result<Vec<(String, String)>> {
    // Property lists start with a DOCTYPE.
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..roxmltree::ParsingOptions::default() };
    let document = Document::parse_with_options(xml, options).map_err(|e| io::Error::other(e.to_string()))?;
    let dict = document
        .root_element()
        .children()
        .find(|n| n.has_tag_name("dict"))
        .ok_or_else(|| io::Error::other("not a bookmark file"))?;
    let mut pairs = Vec::new();
    let mut key: Option<String> = None;
    for node in dict.children().filter(|n| n.is_element()) {
        match node.tag_name().name() {
            "key" => key = Some(node.text().unwrap_or_default().to_string()),
            "string" | "integer" | "real" => {
                if let Some(k) = key.take() {
                    pairs.push((k, node.text().unwrap_or_default().to_string()));
                }
            }
            _ => key = None,
        }
    }
    Ok(pairs)
}

/// One bookmark (None: a protocol forkz does not have).
pub fn cyberduck_bookmark(xml: &str) -> io::Result<Option<Site>> {
    let pairs = plist_strings(xml)?;
    let get =
        |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty());
    let host = get("Hostname").ok_or_else(|| io::Error::other("no server name"))?;
    let protocol = match get("Protocol").as_deref().map(str::to_lowercase).as_deref() {
        Some("sftp") | None => Protocol::Sftp,
        Some("ftp") => Protocol::Ftp,
        Some("ftps") => Protocol::Ftps,
        Some("dav") => Protocol::Webdav,
        Some("davs") => Protocol::Webdavs,
        Some("s3") => Protocol::S3,
        Some("smb") => Protocol::Smb,
        Some(_) => return Ok(None),
    };
    let mut site = Site::new(&host, protocol.default_port(), &get("Username").unwrap_or_default());
    site.protocol = protocol;
    if let Some(port) = get("Port").and_then(|p| p.parse::<u16>().ok()).filter(|p| *p > 0) {
        site.port = port;
    }
    site.name = get("Nickname").unwrap_or(host);
    site.remote_dir = get("Path").filter(|p| p != "/");
    if protocol == Protocol::Sftp
        && let Some(key) = get("Private Key File")
    {
        site.key_file = Some(expand_home(&key));
        site.auth = AuthMethod::Key;
    }
    Ok(Some(site))
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

// WinSCP.

/// Reads the sessions in a WinSCP.ini (folders come from "Folder/Name").
pub fn winscp(text: &str) -> Found {
    let mut found = Found::default();
    let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
    let mut section: Option<(String, Vec<(String, String)>)> = None;
    let mut sessions: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if let Some(done) = section.take() {
                sessions.push(done);
            }
            if let Some(name) = header.strip_prefix("Sessions\\") {
                section = Some((decode(name), Vec::new()));
            }
        } else if let (Some((_, values)), Some((key, value))) = (section.as_mut(), line.split_once('=')) {
            values.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    if let Some(done) = section.take() {
        sessions.push(done);
    }
    for (full_name, values) in sessions {
        if full_name == "Default Settings" {
            continue;
        }
        let get = |key: &str| values.iter().find(|(k, _)| k == key).map(|(_, v)| decode(v)).filter(|v| !v.is_empty());
        let Some(host) = get("HostName") else {
            found.notes.push(format!("{full_name}: no server name"));
            continue;
        };
        let ftps = get("Ftps").and_then(|v| v.parse::<u8>().ok()).unwrap_or(0);
        let protocol = match get("FSProtocol").and_then(|v| v.parse::<u8>().ok()).unwrap_or(2) {
            0..=2 => Protocol::Sftp,
            5 if ftps == 0 => Protocol::Ftp,
            5 => Protocol::Ftps,
            6 if ftps == 0 => Protocol::Webdav,
            6 => Protocol::Webdavs,
            7 => Protocol::S3,
            other => {
                found.notes.push(format!("{full_name}: a kind of server forkz does not connect to ({other})"));
                continue;
            }
        };
        if protocol == Protocol::Ftps && ftps == 1 {
            found.notes.push(format!("{full_name}: implicit FTPS is brought over as explicit FTPS"));
        }
        let mut site = Site::new(&host, protocol.default_port(), &get("UserName").unwrap_or_default());
        site.protocol = protocol;
        if let Some(port) = get("PortNumber").and_then(|p| p.parse::<u16>().ok()).filter(|p| *p > 0) {
            site.port = port;
        }
        let (folder, name) = match full_name.rsplit_once('/') {
            Some((folder, name)) => (folder.replace('/', " \u{203a} "), name.to_string()),
            None => (String::new(), full_name.clone()),
        };
        site.name = name;
        site.folder = folder;
        site.remote_dir = get("RemoteDirectory").filter(|d| d != "/");
        found.sites.push(site);
    }
    found
}

// forkz's own export.

/// What Export Sites writes: the sites, without passwords.
#[derive(Debug, Serialize, Deserialize)]
pub struct Export {
    /// "forkz sites"
    pub format: String,
    #[serde(default)]
    pub sites: Vec<Site>,
}

const FORMAT: &str = "forkz sites";

/// The export file's text.
pub fn export(sites: &[Site]) -> String {
    let export = Export { format: FORMAT.into(), sites: sites.to_vec() };
    let body = toml::to_string_pretty(&export).unwrap_or_default();
    format!(
        "# Sites exported from forkz (passwords are not included).\n# Import with File > Import Sites > From forkz Export.\n\n{body}"
    )
}

/// Reads an export file. The sites get new identities, so importing on
/// the Mac they came from adds copies instead of changing them.
pub fn read_export(text: &str) -> io::Result<Found> {
    let export: Export = toml::from_str(text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    if export.format != FORMAT {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "This is not a forkz sites file."));
    }
    let sites = export
        .sites
        .into_iter()
        .map(|mut site| {
            site.id = new_id();
            site
        })
        .collect();
    Ok(Found { sites, notes: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cyberduck_bookmarks() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Protocol</key>
	<string>sftp</string>
	<key>Nickname</key>
	<string>Web server</string>
	<key>UUID</key>
	<string>8E2D7F43-1A21-4B39-9C2B-AB12CD34EF56</string>
	<key>Hostname</key>
	<string>example.com</string>
	<key>Port</key>
	<string>2222</string>
	<key>Username</key>
	<string>deploy</string>
	<key>Path</key>
	<string>/var/www</string>
	<key>Private Key File</key>
	<string>~/.ssh/id_ed25519</string>
	<key>Access Timestamp</key>
	<string>1700000000000</string>
</dict>
</plist>"#;
        let site = cyberduck_bookmark(xml).unwrap().unwrap();
        assert_eq!(
            (site.name.as_str(), site.host.as_str(), site.port, site.user.as_str()),
            ("Web server", "example.com", 2222, "deploy")
        );
        assert_eq!(site.protocol, Protocol::Sftp);
        assert_eq!(site.remote_dir.as_deref(), Some("/var/www"));
        assert_eq!(site.auth, AuthMethod::Key);
        assert!(site.key_file.unwrap().ends_with(".ssh/id_ed25519"));

        let dav = r#"<plist version="1.0"><dict><key>Protocol</key><string>davs</string><key>Hostname</key><string>dav.example.org</string></dict></plist>"#;
        let site = cyberduck_bookmark(dav).unwrap().unwrap();
        assert_eq!((site.protocol, site.port, site.name.as_str()), (Protocol::Webdavs, 443, "dav.example.org"));
        let other = r#"<plist version="1.0"><dict><key>Protocol</key><string>gdrive</string><key>Hostname</key><string>x</string></dict></plist>"#;
        assert!(cyberduck_bookmark(other).unwrap().is_none());
        assert!(cyberduck_bookmark("<plist><dict></dict></plist>").is_err());
    }

    #[test]
    fn reads_cyberduck_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let one = r#"<plist version="1.0"><dict><key>Protocol</key><string>ftp</string><key>Hostname</key><string>ftp.example.com</string><key>Nickname</key><string>Uploads</string></dict></plist>"#;
        std::fs::write(dir.path().join("a.duck"), one).unwrap();
        std::fs::write(dir.path().join("b.duck"), "not a plist").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        let found = cyberduck(&[dir.path().to_path_buf()]);
        assert_eq!(found.sites.len(), 1);
        assert_eq!(found.sites[0].name, "Uploads");
        assert_eq!(found.notes.len(), 1, "{:?}", found.notes);
    }

    #[test]
    fn reads_winscp_sessions() {
        let ini = "[Configuration\\Interface]\nTheme=0\n\n\
[Sessions\\Default%20Settings]\nHostName=\n\n\
[Sessions\\Work/Production%20Web]\nHostName=prod.example.com\nPortNumber=2200\nUserName=admin\nRemoteDirectory=/srv/my%20site\nPassword=A35C4E5A\n\n\
[Sessions\\ftp%20box]\nHostName=ftp.example.net\nFSProtocol=5\nFtps=3\n\n\
[Sessions\\dav]\nHostName=files.example.org\nFSProtocol=6\nFtps=1\n\n\
[Sessions\\bucket]\nHostName=s3.amazonaws.com\nFSProtocol=7\n\n\
[Sessions\\nohost]\nUserName=x\n";
        let found = winscp(ini);
        let summary: Vec<(String, String, Protocol, u16)> =
            found.sites.iter().map(|s| (s.folder.clone(), s.name.clone(), s.protocol, s.port)).collect();
        assert_eq!(
            summary,
            [
                ("Work".to_string(), "Production Web".to_string(), Protocol::Sftp, 2200),
                (String::new(), "ftp box".to_string(), Protocol::Ftps, 21),
                (String::new(), "dav".to_string(), Protocol::Webdavs, 443),
                (String::new(), "bucket".to_string(), Protocol::S3, 443),
            ]
        );
        assert_eq!(found.sites[0].remote_dir.as_deref(), Some("/srv/my site"));
        assert_eq!(found.sites[0].user, "admin");
        assert_eq!(found.notes, ["nohost: no server name"]);
    }

    #[test]
    fn exports_and_reads_back() {
        let mut site = Site::new("example.com", 22, "me");
        site.name = "Example".into();
        site.folder = "Clients".into();
        site.remote_dir = Some("/home/me".into());
        let text = export(std::slice::from_ref(&site));
        assert!(!text.to_lowercase().contains("password ="));
        let found = read_export(&text).unwrap();
        assert_eq!(found.sites.len(), 1);
        let back = &found.sites[0];
        assert_ne!(back.id, site.id, "a new identity");
        assert_eq!(
            (back.name.as_str(), back.folder.as_str(), back.remote_dir.as_deref()),
            ("Example", "Clients", Some("/home/me"))
        );
        assert!(read_export("format = \"something else\"").is_err());
        assert!(read_export("not toml at all [").is_err());
    }
}
