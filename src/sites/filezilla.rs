//! Reads FileZilla's configuration (`~/.config/filezilla`): the Site
//! Manager's SFTP sites (with folders, passwords, key files, and default
//! folders), trusted host keys, and the settings forkz has an equivalent
//! for. Nothing is changed in FileZilla's files.

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use roxmltree::{Document, Node};

use super::{Protocol, Site};
use crate::fs::ops::Resolution;
use crate::vfs::sftp::AuthMethod;

/// FileZilla's configuration folder on macOS.
pub fn default_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config").join("filezilla"))
}

#[derive(Debug, Default)]
pub struct Import {
    /// Sites with their passwords, if FileZilla saved them readably.
    pub sites: Vec<(Site, Option<String>)>,
    /// Host keys FileZilla trusts: host, port, key blob.
    pub host_keys: Vec<(String, u16, Vec<u8>)>,
    pub settings: Settings,
    /// What could not be brought over, for the summary.
    pub notes: Vec<String>,
}

/// FileZilla settings that forkz has an equivalent for.
#[derive(Debug, Default, PartialEq)]
pub struct Settings {
    pub show_hidden: Option<bool>,
    pub preserve_timestamps: Option<bool>,
    /// What to do when a downloaded file exists (None: ask).
    pub download_conflict: Option<Option<Resolution>>,
    pub upload_conflict: Option<Option<Resolution>>,
    pub timeout_seconds: Option<u64>,
    pub key_files: Vec<PathBuf>,
    /// "Number of Transfers": how many run at once.
    pub max_transfers: Option<usize>,
}

/// Reads everything there is in `dir`. Missing files are fine.
pub fn read(dir: &Path) -> io::Result<Import> {
    let mut import = Import::default();
    let sites = dir.join("sitemanager.xml");
    if sites.exists() {
        let text = std::fs::read_to_string(&sites)?;
        parse_sites(&text, &mut import).map_err(|e| io::Error::other(format!("sitemanager.xml: {e}")))?;
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("hostkeys.xml")) {
        import.host_keys = parse_host_keys(&text);
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("filezilla.xml")) {
        import.settings = parse_settings(&text);
    }
    Ok(import)
}

pub fn parse_sites(text: &str, import: &mut Import) -> Result<(), roxmltree::Error> {
    let doc = Document::parse(text)?;
    if let Some(servers) = doc.descendants().find(|n| n.has_tag_name("Servers")) {
        walk(servers, &[], import);
    }
    Ok(())
}

fn walk(node: Node, folders: &[String], import: &mut Import) {
    for child in node.children().filter(Node::is_element) {
        match child.tag_name().name() {
            "Folder" => {
                // The folder's name is its own text, before the child elements.
                let name = child.children().filter(|c| c.is_text()).filter_map(|c| c.text()).collect::<String>();
                let mut path = folders.to_vec();
                path.push(name.trim().to_string());
                walk(child, &path, import);
            }
            "Server" => server(child, folders, import),
            _ => {}
        }
    }
}

fn text_of<'a>(node: Node<'a, '_>, tag: &str) -> Option<&'a str> {
    node.children().find(|c| c.has_tag_name(tag)).and_then(|c| c.text())
}

fn server(node: Node, folders: &[String], import: &mut Import) {
    let host = text_of(node, "Host").unwrap_or_default().trim().to_string();
    let name = text_of(node, "Name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&host).to_string();
    // 0 FTP, 1 SFTP, 2 HTTP, 3 FTPS (implicit), 4 FTPES, 5 FTP (insecure),
    // 6 S3, 8 WebDAV, and more in FileZilla Pro.
    let number = text_of(node, "Protocol").and_then(|p| p.trim().parse::<u32>().ok()).unwrap_or(0);
    let port = text_of(node, "Port").and_then(|p| p.trim().parse::<u16>().ok());
    let protocol = match number {
        0 => Protocol::Ftp,
        1 => Protocol::Sftp,
        4 => Protocol::Ftps,
        5 => Protocol::FtpPlain,
        6 => Protocol::S3,
        8 if port == Some(80) => Protocol::Webdav,
        8 => Protocol::Webdavs,
        3 => {
            import.notes.push(format!(
                "\u{201c}{name}\u{201d} uses implicit FTPS (port 990), which forkz does not support; most servers also offer explicit FTPS on port 21."
            ));
            return;
        }
        _ => {
            import.notes.push(format!("\u{201c}{name}\u{201d} uses a protocol forkz does not support yet."));
            return;
        }
    };
    if host.is_empty() {
        return;
    }
    let port = port.filter(|&p| p > 0).unwrap_or(protocol.default_port());
    let user = text_of(node, "User").unwrap_or_default().to_string();
    let mut site = Site::new(&host, port, &user);
    site.protocol = protocol;
    site.name = name.clone();
    site.folder = folders.join("/");
    // 0 anonymous, 1 normal, 2 ask for password, 3 interactive, 4 account,
    // 5 key file, 6 profile.
    let logon = text_of(node, "Logontype").and_then(|l| l.trim().parse::<u32>().ok()).unwrap_or(1);
    if logon == 5 && protocol.is_sftp() {
        site.auth = AuthMethod::Key;
        site.key_file = text_of(node, "Keyfile").map(PathBuf::from);
    }
    site.remote_dir = text_of(node, "RemoteDir").and_then(parse_server_path);
    site.local_dir = text_of(node, "LocalDir").map(str::trim).filter(|d| !d.is_empty()).map(PathBuf::from);

    let mut password = None;
    if let Some(pass) = node.children().find(|c| c.has_tag_name("Pass")) {
        let raw = pass.text().unwrap_or_default();
        match pass.attribute("encoding") {
            Some("base64") => password = STANDARD.decode(raw.trim()).ok().and_then(|b| String::from_utf8(b).ok()),
            Some("crypt") => import.notes.push(format!(
                "The password of \u{201c}{name}\u{201d} is protected by FileZilla\u{2019}s master password; you will be asked for it when connecting."
            )),
            _ => password = Some(raw.to_string()).filter(|p| !p.is_empty()),
        }
    }
    import.sites.push((site, password));
}

/// FileZilla's "safe path": `<type> <prefix length> [prefix] (<length>
/// <segment>)*`, for example "1 0 4 home 4 user" for /home/user. Only
/// Unix paths (type 1) are understood.
pub fn parse_server_path(text: &str) -> Option<String> {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut position = 0;
    let number = |position: &mut usize| -> Option<usize> {
        while chars.get(*position) == Some(&' ') {
            *position += 1;
        }
        let start = *position;
        while chars.get(*position).is_some_and(char::is_ascii_digit) {
            *position += 1;
        }
        chars[start..*position].iter().collect::<String>().parse().ok()
    };
    let kind = number(&mut position)?;
    if kind != 1 {
        return None;
    }
    let prefix = number(&mut position)?;
    position += if prefix > 0 { 1 + prefix } else { 0 };
    let mut segments = Vec::new();
    while position < chars.len() {
        let length = number(&mut position)?;
        position += 1; // the space
        let segment: String = chars.get(position..position + length)?.iter().collect();
        segments.push(segment);
        position += length;
    }
    Some(format!("/{}", segments.join("/")))
}

pub fn parse_host_keys(text: &str) -> Vec<(String, u16, Vec<u8>)> {
    let Ok(doc) = Document::parse(text) else { return Vec::new() };
    doc.descendants()
        .filter(|n| n.has_tag_name("Server"))
        .filter_map(|n| {
            let host = n.attribute("Host")?.to_string();
            let port = n.attribute("Port").and_then(|p| p.parse().ok()).unwrap_or(22);
            let key = STANDARD.decode(text_of(n, "Hostkey")?.trim()).ok()?;
            Some((host, port, key))
        })
        .collect()
}

pub fn parse_settings(text: &str) -> Settings {
    let mut settings = Settings::default();
    let Ok(doc) = Document::parse(text) else { return settings };
    let setting = |name: &str| -> Option<String> {
        doc.descendants()
            .find(|n| n.has_tag_name("Setting") && n.attribute("name") == Some(name))
            .map(|n| n.text().unwrap_or_default().trim().to_string())
    };
    let flag = |name: &str| setting(name).map(|v| v == "1");
    settings.show_hidden = flag("View hidden files");
    settings.preserve_timestamps = flag("Preserve timestamps");
    // 0 ask, 1 overwrite, 2 overwrite if newer, 3 resume, 4 rename, 5 skip,
    // 6 and 7 overwrite if the size differs (closest: ask).
    let action = |name: &str| {
        setting(name).and_then(|v| v.parse::<u32>().ok()).map(|v| match v {
            1 => Some(Resolution::Replace),
            2 => Some(Resolution::ReplaceOlder),
            4 => Some(Resolution::KeepBoth),
            5 => Some(Resolution::Skip),
            _ => None,
        })
    };
    settings.download_conflict = action("File exists action download");
    settings.upload_conflict = action("File exists action upload");
    settings.timeout_seconds = setting("Timeout").and_then(|v| v.parse().ok()).filter(|&t| t > 0);
    settings.max_transfers = setting("Number of Transfers").and_then(|v| v.parse().ok()).filter(|&n| n > 0);
    settings.key_files = setting("SFTP keyfiles")
        .map(|v| v.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default();
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITES: &str = r#"<?xml version="1.0"?>
<FileZilla3 version="3.70.6" platform="mac">
  <Servers>
    <Server>
      <Host>top.example</Host><Port>2222</Port><Protocol>1</Protocol>
      <User>me</User><Pass encoding="base64">c2VjcmV0IHBhc3M=</Pass><Logontype>1</Logontype>
      <Name>Top</Name><RemoteDir>1 0 3 var 3 www 6 my sit</RemoteDir><LocalDir>/Users/me/site</LocalDir>
    </Server>
    <Folder expanded="1">Work
      <Folder>Clients
        <Server>
          <Host>key.example</Host><Port>22</Port><Protocol>1</Protocol><User>deploy</User>
          <Logontype>5</Logontype><Keyfile>/Users/me/.ssh/deploy</Keyfile><Name>Keyed</Name>
        </Server>
      </Folder>
      <Server>
        <Host>locked.example</Host><Protocol>1</Protocol><User>u</User>
        <Pass encoding="crypt">AbCdEf</Pass><Logontype>1</Logontype><Name>Locked</Name>
      </Server>
      <Server><Host>ftp.example</Host><Protocol>0</Protocol><Name>Old FTP</Name></Server>
      <Server><Host>tls.example</Host><Port>0</Port><Protocol>4</Protocol><User>u</User><Name>TLS</Name></Server>
      <Server><Host>implicit.example</Host><Protocol>3</Protocol><Name>Implicit</Name></Server>
    </Folder>
  </Servers>
</FileZilla3>"#;

    #[test]
    fn sites_with_folders_passwords_and_keys() {
        let mut import = Import::default();
        parse_sites(SITES, &mut import).unwrap();
        assert_eq!(import.sites.len(), 5);
        let (top, password) = &import.sites[0];
        assert_eq!(
            (top.name.as_str(), top.host.as_str(), top.port, top.user.as_str()),
            ("Top", "top.example", 2222, "me")
        );
        assert_eq!(password.as_deref(), Some("secret pass"));
        assert_eq!(top.remote_dir.as_deref(), Some("/var/www/my sit"));
        assert_eq!(top.local_dir.as_deref(), Some(Path::new("/Users/me/site")));
        assert_eq!(top.folder, "");

        let (keyed, password) = &import.sites[1];
        assert_eq!(keyed.folder, "Work/Clients");
        assert_eq!(keyed.auth, AuthMethod::Key);
        assert_eq!(keyed.key_file.as_deref(), Some(Path::new("/Users/me/.ssh/deploy")));
        assert_eq!(*password, None);

        let (locked, password) = &import.sites[2];
        assert_eq!((locked.folder.as_str(), locked.port), ("Work", 22));
        assert_eq!(*password, None);
        let (ftp, _) = &import.sites[3];
        assert_eq!((ftp.protocol, ftp.port, ftp.folder.as_str()), (Protocol::Ftp, 21, "Work"));
        let (tls, _) = &import.sites[4];
        assert_eq!((tls.protocol, tls.port), (Protocol::Ftps, 21));
        // The master password, and implicit FTPS.
        assert_eq!(import.notes.len(), 2, "{:?}", import.notes);
    }

    #[test]
    fn server_paths() {
        assert_eq!(parse_server_path("1 0 4 home 4 user").as_deref(), Some("/home/user"));
        assert_eq!(parse_server_path("1 0").as_deref(), Some("/"));
        assert_eq!(parse_server_path("1 0 5 a b c").as_deref(), Some("/a b c"));
        assert_eq!(parse_server_path("8 3 C:\\ 4 Temp"), None, "Windows paths are not understood");
        assert_eq!(parse_server_path("1 0 9 short"), None, "truncated");
    }

    #[test]
    fn host_keys_and_settings() {
        let keys = parse_host_keys(
            r#"<FileZilla3><Server Host="a.example" Port="2222"><Hostkey>AAAAC3NzaC1lZDI1NTE5</Hostkey></Server></FileZilla3>"#,
        );
        assert_eq!(keys.len(), 1);
        assert_eq!((keys[0].0.as_str(), keys[0].1), ("a.example", 2222));
        assert_eq!(crate::vfs::known_hosts::key_type_of(&keys[0].2).as_deref(), Some("ssh-ed25519"));

        let settings = parse_settings(
            r#"<FileZilla3><Settings>
                <Setting name="View hidden files">1</Setting>
                <Setting name="Preserve timestamps">0</Setting>
                <Setting name="File exists action download">2</Setting>
                <Setting name="File exists action upload">0</Setting>
                <Setting name="Timeout">20</Setting>
                <Setting name="Number of Transfers">3</Setting>
                <Setting name="SFTP keyfiles">/k/one
/k/two</Setting>
            </Settings></FileZilla3>"#,
        );
        assert_eq!(
            settings,
            Settings {
                show_hidden: Some(true),
                preserve_timestamps: Some(false),
                download_conflict: Some(Some(Resolution::ReplaceOlder)),
                upload_conflict: Some(None),
                timeout_seconds: Some(20),
                key_files: vec!["/k/one".into(), "/k/two".into()],
                max_transfers: Some(3),
            }
        );
    }

    /// Reads the real FileZilla folder, printing names and counts only
    /// (never passwords). Run with --ignored --nocapture.
    #[test]
    #[ignore]
    fn reads_this_macs_filezilla() {
        let Some(dir) = default_dir().filter(|d| d.is_dir()) else { return };
        let import = read(&dir).unwrap();
        for (site, password) in &import.sites {
            println!(
                "site {:?} folder {:?} {} auth {:?} remote {:?} local {:?} password saved: {}",
                site.name,
                site.folder,
                site.address(),
                site.auth,
                site.remote_dir,
                site.local_dir,
                password.is_some()
            );
        }
        for (host, port, key) in &import.host_keys {
            println!("host key {host}:{port} {:?}", crate::vfs::known_hosts::key_type_of(key));
        }
        println!("settings {:?}", import.settings);
        println!("notes {:?}", import.notes);
    }
}
