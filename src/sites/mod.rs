//! Saved servers ("sites"), in `~/Library/Application Support/forkz/sites.toml`.
//! Passwords are not in that file: they are in the login Keychain.

pub mod filezilla;
pub mod import;

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::vfs::sftp::{AuthMethod, ConnectParams, Proxy};
use crate::vfs::ssh_config::{HostConfig, ProxyDirective};

/// ProxyCommand tokens: %h host, %p port, %r user, %n the name as typed,
/// %% a percent sign.
fn proxy_tokens(command: &str, host: &str, port: u16, user: &str, alias: &str) -> String {
    let mut out = String::new();
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(host),
            Some('p') => out.push_str(&port.to_string()),
            Some('r') => out.push_str(user),
            Some('n') => out.push_str(alias),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// How a site is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    #[default]
    Sftp,
    /// FTP, encrypted (AUTH TLS) when the server offers it.
    Ftp,
    /// FTP with TLS required (explicit FTPS).
    Ftps,
    /// Plain FTP: nothing is encrypted.
    FtpPlain,
    Webdav,
    /// WebDAV over HTTPS.
    Webdavs,
    S3,
    /// S3 over plain HTTP (local MinIO and test servers).
    S3Http,
    /// Google Cloud Storage, through its S3-compatible API (HMAC keys).
    Gcs,
    /// Azure Blob Storage (account name and access key).
    Azure,
    /// Box (signing in in the browser).
    Box,
    /// Network shares, mounted by macOS.
    Smb,
    Afp,
    Nfs,
}

impl Protocol {
    pub const ALL: [Protocol; 14] = [
        Protocol::Sftp,
        Protocol::Ftp,
        Protocol::Ftps,
        Protocol::FtpPlain,
        Protocol::Webdavs,
        Protocol::Webdav,
        Protocol::S3,
        Protocol::S3Http,
        Protocol::Gcs,
        Protocol::Azure,
        Protocol::Box,
        Protocol::Smb,
        Protocol::Afp,
        Protocol::Nfs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Protocol::Sftp => "SFTP",
            Protocol::Ftp => "FTP (TLS if available)",
            Protocol::Ftps => "FTPS (TLS required)",
            Protocol::FtpPlain => "FTP (unencrypted)",
            Protocol::Webdav => "WebDAV (HTTP, unencrypted)",
            Protocol::Webdavs => "WebDAV (HTTPS)",
            Protocol::S3 => "Amazon S3 or compatible",
            Protocol::S3Http => "S3 (HTTP, unencrypted)",
            Protocol::Gcs => "Google Cloud Storage",
            Protocol::Azure => "Azure Blob Storage",
            Protocol::Box => "Box",
            Protocol::Smb => "SMB (Windows and Mac sharing)",
            Protocol::Afp => "AFP (older Mac sharing)",
            Protocol::Nfs => "NFS",
        }
    }

    /// The scheme in addresses ("ftp://user@host").
    pub fn scheme(self) -> &'static str {
        match self {
            Protocol::Sftp => "sftp",
            Protocol::Ftp | Protocol::FtpPlain => "ftp",
            Protocol::Ftps => "ftps",
            Protocol::Webdav => "dav",
            Protocol::Webdavs => "davs",
            Protocol::S3 | Protocol::S3Http => "s3",
            Protocol::Gcs => "gcs",
            Protocol::Azure => "azure",
            Protocol::Box => "box",
            Protocol::Smb => "smb",
            Protocol::Afp => "afp",
            Protocol::Nfs => "nfs",
        }
    }

    pub fn from_scheme(scheme: &str) -> Option<Protocol> {
        Some(match scheme.to_ascii_lowercase().as_str() {
            "sftp" | "ssh" => Protocol::Sftp,
            "ftp" => Protocol::Ftp,
            "ftps" | "ftpes" => Protocol::Ftps,
            "dav" | "webdav" | "http" => Protocol::Webdav,
            "davs" | "webdavs" | "https" => Protocol::Webdavs,
            "s3" => Protocol::S3,
            "gcs" | "gs" => Protocol::Gcs,
            "azure" | "azblob" => Protocol::Azure,
            "box" => Protocol::Box,
            "smb" | "cifs" => Protocol::Smb,
            "afp" => Protocol::Afp,
            "nfs" => Protocol::Nfs,
            _ => return None,
        })
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Sftp => 22,
            Protocol::Ftp | Protocol::Ftps | Protocol::FtpPlain => 21,
            Protocol::Webdav | Protocol::S3Http => 80,
            Protocol::Webdavs | Protocol::S3 | Protocol::Gcs | Protocol::Azure | Protocol::Box => 443,
            Protocol::Smb => 445,
            Protocol::Afp => 548,
            Protocol::Nfs => 2049,
        }
    }

    pub fn is_sftp(self) -> bool {
        self == Protocol::Sftp
    }

    /// Cloud object storage: S3, Google Cloud Storage, or Azure (keys, not
    /// a user and password; buckets or containers at the top).
    pub fn is_object_storage(self) -> bool {
        matches!(self, Protocol::S3 | Protocol::S3Http | Protocol::Gcs | Protocol::Azure)
    }

    /// The server a site uses when it names none (the cloud services').
    pub fn default_host(self, user: &str) -> Option<String> {
        match self {
            Protocol::Gcs => Some("storage.googleapis.com".into()),
            Protocol::Box => Some("box.com".into()),
            Protocol::Azure if !user.trim().is_empty() => Some(crate::vfs::azure::account_host(user.trim())),
            _ => None,
        }
    }

    /// A cloud drive signed in to in the browser (Box): no server, user,
    /// or password in the site.
    pub fn signs_in_with_browser(self) -> bool {
        self == Protocol::Box
    }

    /// A network share that macOS mounts (SMB, AFP, NFS).
    pub fn is_share(self) -> bool {
        matches!(self, Protocol::Smb | Protocol::Afp | Protocol::Nfs)
    }
}

fn is_sftp(protocol: &Protocol) -> bool {
    protocol.is_sftp()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Site {
    pub id: String,
    pub name: String,
    /// Group in the sidebar ("" for none), like FileZilla's folders.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    #[serde(default, skip_serializing_if = "is_sftp")]
    pub protocol: Protocol,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub auth: AuthMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<PathBuf>,
    /// The remote folder to open (the login folder if None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_dir: Option<String>,
    /// A local folder to show in the other pane when connecting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_dir: Option<PathBuf>,
    /// The S3 region (from the server name if None, else us-east-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}

fn default_port() -> u16 {
    22
}

impl Site {
    pub fn new(host: &str, port: u16, user: &str) -> Site {
        Site {
            id: new_id(),
            name: host.to_string(),
            folder: String::new(),
            protocol: Protocol::Sftp,
            host: host.to_string(),
            port,
            user: user.to_string(),
            auth: AuthMethod::Password,
            key_file: None,
            remote_dir: None,
            local_dir: None,
            region: None,
        }
    }

    /// "user@host" (with ":port" if not the protocol's usual one, and
    /// "ftp://" and so on for protocols other than SFTP).
    pub fn address(&self) -> String {
        let port = if self.port == self.protocol.default_port() { String::new() } else { format!(":{}", self.port) };
        let scheme = if self.protocol.is_sftp() { String::new() } else { format!("{}://", self.protocol.scheme()) };
        if self.user.is_empty() {
            format!("{scheme}{}{port}", self.host)
        } else {
            format!("{scheme}{}@{}{port}", self.user, self.host)
        }
    }

    /// The S3 region: the site's, or the one in an AWS server name
    /// ("s3.eu-west-1.amazonaws.com", "bucket.s3-us-west-2.amazonaws.com"),
    /// or us-east-1.
    pub fn s3_region(&self) -> String {
        if let Some(region) = self.region.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            return region.to_string();
        }
        let host = self.host.to_ascii_lowercase();
        if let Some(rest) = host.strip_suffix(".amazonaws.com") {
            let parts: Vec<&str> = rest.split('.').collect();
            for (i, part) in parts.iter().enumerate() {
                if let Some(region) = part.strip_prefix("s3-").filter(|r| !r.is_empty()) {
                    return region.to_string();
                }
                if *part == "s3"
                    && let Some(next) = parts.get(i + 1)
                {
                    return next.to_string();
                }
            }
        }
        "us-east-1".to_string()
    }

    /// How to connect, with what `~/.ssh/config` says about the host: its
    /// HostName, its User and Port unless the site sets them (22 counts
    /// as not set), and its IdentityFiles to try first.
    pub fn params(
        &self,
        ssh: &HostConfig,
        password: Option<String>,
        timeout: Duration,
        extra_keys: Vec<PathBuf>,
    ) -> ConnectParams {
        let user = match (&self.user, &ssh.user) {
            (user, _) if !user.is_empty() => user.clone(),
            (_, Some(user)) => user.clone(),
            _ => whoami(),
        };
        let port = if self.port == 22 { ssh.port.unwrap_or(22) } else { self.port };
        let mut keys = ssh.identity_files.clone();
        keys.extend(extra_keys);
        ConnectParams {
            host: ssh.host_name.clone().unwrap_or_else(|| self.host.clone()),
            port,
            user: user.clone(),
            auth: self.auth,
            key_file: self.key_file.clone(),
            password,
            timeout,
            extra_keys: keys,
            identities_only: ssh.identities_only,
            proxy: ssh.proxy.as_ref().map(|proxy| match proxy {
                ProxyDirective::Jump(jumps) => Proxy::Jump(jumps.clone()),
                ProxyDirective::Command(command) => {
                    let host = ssh.host_name.clone().unwrap_or_else(|| self.host.clone());
                    Proxy::Command(proxy_tokens(command, &host, port, &user, &self.host))
                }
            }),
        }
    }

    /// Same server and account (to avoid importing duplicates).
    pub fn same_account(&self, other: &Site) -> bool {
        self.protocol == other.protocol
            && self.host.eq_ignore_ascii_case(&other.host)
            && self.port == other.port
            && self.user == other.user
    }
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_default()
}

/// A unique id: time plus a counter (no randomness needed).
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{nanos:x}{:04x}", COUNTER.fetch_add(1, Ordering::Relaxed) & 0xffff)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SiteStore {
    #[serde(default, rename = "site")]
    pub sites: Vec<Site>,
}

impl SiteStore {
    pub fn path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("forkz").join("sites.toml"))
    }

    pub fn load() -> SiteStore {
        let Some(path) = SiteStore::path() else { return SiteStore::default() };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("ignoring unreadable sites {}: {e}", path.display());
                SiteStore::default()
            }),
            Err(_) => SiteStore::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = SiteStore::path() else { return };
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("toml.tmp");
            std::fs::write(&temporary, toml::to_string_pretty(self)?)?;
            std::fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("could not save sites: {e:#}");
        }
    }

    pub fn get(&self, id: &str) -> Option<&Site> {
        self.sites.iter().find(|s| s.id == id)
    }

    /// Adds a site, or replaces the one with the same id.
    pub fn upsert(&mut self, site: Site) {
        match self.sites.iter_mut().find(|s| s.id == site.id) {
            Some(existing) => *existing = site,
            None => self.sites.push(site),
        }
        self.sort();
    }

    pub fn remove(&mut self, id: &str) {
        self.sites.retain(|s| s.id != id);
    }

    /// By folder, then name.
    pub fn sort(&mut self) {
        self.sites.sort_by(|a, b| {
            crate::fs::natural_cmp(&a.folder, &b.folder).then_with(|| crate::fs::natural_cmp(&a.name, &b.name))
        });
    }
}

/// Site passwords in the login Keychain.
pub mod keychain {
    use security_framework::passwords;

    const SERVICE: &str = "io.github.houseofmackee.forkz.sftp";

    pub fn password(site_id: &str) -> Option<String> {
        passwords::get_generic_password(SERVICE, site_id).ok().and_then(|bytes| String::from_utf8(bytes).ok())
    }

    pub fn save_password(site_id: &str, password: &str) {
        if let Err(e) = passwords::set_generic_password(SERVICE, site_id, password.as_bytes()) {
            tracing::warn!("could not save the password in the Keychain: {e}");
        }
    }

    pub fn delete_password(site_id: &str) {
        let _ = passwords::delete_generic_password(SERVICE, site_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocols_regions_and_addresses() {
        let mut site = Site::new("s3.eu-west-1.amazonaws.com", 443, "AKIA");
        site.protocol = Protocol::S3;
        assert_eq!(site.s3_region(), "eu-west-1");
        assert_eq!(site.address(), "s3://AKIA@s3.eu-west-1.amazonaws.com");
        site.host = "photos.s3-us-west-2.amazonaws.com".into();
        assert_eq!(site.s3_region(), "us-west-2");
        site.host = "minio.local".into();
        assert_eq!(site.s3_region(), "us-east-1");
        site.region = Some("auto".into());
        assert_eq!(site.s3_region(), "auto");
        let mut ftp = Site::new("ftp.example.com", 2121, "me");
        ftp.protocol = Protocol::Ftps;
        assert_eq!(ftp.address(), "ftps://me@ftp.example.com:2121");
        // Old sites files have no protocol: SFTP. SFTP is not written out.
        let text = toml::to_string(&SiteStore { sites: vec![Site::new("h", 22, "u")] }).unwrap();
        assert!(!text.contains("protocol"));
        let back: SiteStore = toml::from_str(&text).unwrap();
        assert_eq!(back.sites[0].protocol, Protocol::Sftp);
        let text = toml::to_string(&SiteStore { sites: vec![ftp] }).unwrap();
        assert!(text.contains("protocol = \"ftps\""));
    }

    #[test]
    fn store_round_trip_sorted() {
        let mut store = SiteStore::default();
        let mut b = Site::new("b.example", 22, "me");
        b.folder = "Work".into();
        let a = Site::new("a.example", 2222, "");
        store.upsert(b.clone());
        store.upsert(a.clone());
        assert_eq!(store.sites[0].host, "a.example", "no folder sorts first");
        let text = toml::to_string_pretty(&store).unwrap();
        let back: SiteStore = toml::from_str(&text).unwrap();
        assert_eq!(back.sites, store.sites);
        assert_eq!(a.address(), "a.example:2222");
        assert_eq!(b.address(), "me@b.example");
        assert_ne!(a.id, b.id);
        b.name = "renamed".into();
        store.upsert(b.clone());
        assert_eq!(store.sites.len(), 2);
        assert_eq!(store.get(&b.id).unwrap().name, "renamed");
    }

    #[test]
    fn ssh_config_fills_in_what_the_site_leaves_open() {
        let ssh = HostConfig {
            host_name: Some("real.example.com".into()),
            user: Some("deploy".into()),
            port: Some(2200),
            identity_files: vec!["/k/config_key".into()],
            proxy: None,
            identities_only: false,
        };
        let timeout = Duration::from_secs(5);
        let alias = Site::new("web", 22, "");
        let params = alias.params(&ssh, None, timeout, vec!["/k/extra".into()]);
        assert_eq!((params.host.as_str(), params.port, params.user.as_str()), ("real.example.com", 2200, "deploy"));
        assert_eq!(params.extra_keys, [PathBuf::from("/k/config_key"), PathBuf::from("/k/extra")]);
        // The site's own user and port win.
        let explicit = Site::new("web", 2222, "me");
        let params = explicit.params(&ssh, None, timeout, Vec::new());
        assert_eq!((params.port, params.user.as_str()), (2222, "me"));
        let params = explicit.params(&HostConfig::default(), None, timeout, Vec::new());
        assert_eq!(params.host, "web");
        assert_eq!(params.proxy, None);
        let via = HostConfig {
            host_name: Some("10.1.1.1".into()),
            proxy: Some(ProxyDirective::Command("nc -X 5 %h %p # %r@%n %%".into())),
            ..HostConfig::default()
        };
        let params = Site::new("box", 22, "me").params(&via, None, timeout, Vec::new());
        assert_eq!(params.proxy, Some(Proxy::Command("nc -X 5 10.1.1.1 22 # me@box %".into())));
    }
}
