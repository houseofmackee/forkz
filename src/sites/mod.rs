//! Saved servers ("sites"), in `~/Library/Application Support/forkz/sites.toml`.
//! Passwords are not in that file: they are in the login Keychain.

pub mod filezilla;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Site {
    pub id: String,
    pub name: String,
    /// Group in the sidebar ("" for none), like FileZilla's folders.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
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
            host: host.to_string(),
            port,
            user: user.to_string(),
            auth: AuthMethod::Password,
            key_file: None,
            remote_dir: None,
            local_dir: None,
        }
    }

    /// "user@host" (with ":port" if not 22).
    pub fn address(&self) -> String {
        let port = if self.port == 22 { String::new() } else { format!(":{}", self.port) };
        if self.user.is_empty() { format!("{}{port}", self.host) } else { format!("{}@{}{port}", self.user, self.host) }
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
        self.host.eq_ignore_ascii_case(&other.host) && self.port == other.port && self.user == other.user
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
