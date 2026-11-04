//! Reads `~/.ssh/config` the way OpenSSH does for the options forkz uses:
//! HostName, User, Port, IdentityFile, and ProxyJump/ProxyCommand (to say
//! they are not supported). Host patterns may use `*`, `?`, and `!`; the
//! first value found for an option wins; Include is followed.

use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct SshConfig {
    blocks: Vec<Block>,
}

#[derive(Debug, Clone)]
struct Block {
    /// None for options before the first Host line (they apply to all).
    patterns: Option<Vec<String>>,
    options: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProxyDirective {
    /// Jump hosts, comma-separated, as written.
    Jump(String),
    /// A command whose input and output carry the SSH connection.
    Command(String),
}

/// What the config says about one host.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HostConfig {
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_files: Vec<PathBuf>,
    /// ProxyJump or ProxyCommand (tokens not yet replaced).
    pub proxy: Option<ProxyDirective>,
    /// Only the IdentityFiles are offered (not the agent's other keys).
    pub identities_only: bool,
}

impl SshConfig {
    /// `~/.ssh/config`, or an empty config if there is none.
    pub fn load() -> SshConfig {
        let Some(ssh) = dirs::home_dir().map(|h| h.join(".ssh")) else { return SshConfig::default() };
        let mut config = SshConfig::default();
        if let Ok(text) = std::fs::read_to_string(ssh.join("config")) {
            config.add(&text, &ssh, 0);
        }
        config
    }

    pub fn parse(text: &str) -> SshConfig {
        let mut config = SshConfig::default();
        config.add(text, Path::new("/nonexistent"), 0);
        config
    }

    fn add(&mut self, text: &str, ssh_dir: &Path, depth: usize) {
        for line in text.lines() {
            let Some((key, value)) = split_line(line) else { continue };
            match key.as_str() {
                "host" => self.blocks.push(Block {
                    patterns: Some(value.split_whitespace().map(|p| p.to_lowercase()).collect()),
                    options: Vec::new(),
                }),
                // Match blocks are not evaluated: their options are skipped
                // (a pattern that matches nothing).
                "match" => self.blocks.push(Block {
                    patterns: Some(if value.trim().eq_ignore_ascii_case("all") {
                        vec!["*".into()]
                    } else {
                        Vec::new()
                    }),
                    options: Vec::new(),
                }),
                "include" if depth < 8 => {
                    for pattern in value.split_whitespace() {
                        for file in include_files(pattern, ssh_dir) {
                            if let Ok(text) = std::fs::read_to_string(&file) {
                                self.add(&text, ssh_dir, depth + 1);
                            }
                        }
                    }
                }
                _ => {
                    if self.blocks.is_empty() {
                        self.blocks.push(Block { patterns: None, options: Vec::new() });
                    }
                    self.blocks.last_mut().expect("added above").options.push((key, value));
                }
            }
        }
    }

    /// The settings for `alias` (as typed by the user).
    pub fn resolve(&self, alias: &str) -> HostConfig {
        let alias_lower = alias.to_lowercase();
        let mut host = HostConfig::default();
        let mut seen_identities_only = false;
        for block in self.blocks.iter().filter(|b| b.patterns.as_ref().is_none_or(|p| matches(p, &alias_lower))) {
            for (key, value) in &block.options {
                match key.as_str() {
                    "hostname" if host.host_name.is_none() => host.host_name = Some(value.clone()),
                    "user" if host.user.is_none() => host.user = Some(value.clone()),
                    "port" if host.port.is_none() => host.port = value.parse().ok(),
                    "identitiesonly" if !seen_identities_only => {
                        seen_identities_only = true;
                        host.identities_only = value.eq_ignore_ascii_case("yes");
                    }
                    // IdentityFile adds up rather than first-wins.
                    "identityfile" => host.identity_files.push(PathBuf::from(value)),
                    "proxyjump" | "proxycommand" if host.proxy.is_none() && !value.eq_ignore_ascii_case("none") => {
                        host.proxy = Some(if key == "proxyjump" {
                            ProxyDirective::Jump(value.clone())
                        } else {
                            ProxyDirective::Command(value.clone())
                        });
                    }
                    _ => {}
                }
            }
        }
        // Tokens in HostName and IdentityFile.
        let local_user = std::env::var("USER").unwrap_or_default();
        let host_name = host.host_name.clone().map(|h| h.replace("%h", alias)).unwrap_or_else(|| alias.to_string());
        let remote_user = host.user.clone().unwrap_or_else(|| local_user.clone());
        host.host_name = host.host_name.map(|h| h.replace("%h", alias));
        host.identity_files = host
            .identity_files
            .iter()
            .map(|file| expand(&file.to_string_lossy(), &host_name, &remote_user, &local_user))
            .collect();
        host
    }

    /// Host names written without wildcards, to offer in the Connect form.
    pub fn aliases(&self) -> Vec<String> {
        let mut aliases: Vec<String> = Vec::new();
        for block in &self.blocks {
            for pattern in block.patterns.iter().flatten() {
                if !pattern.contains(['*', '?', '!']) && !aliases.contains(pattern) {
                    aliases.push(pattern.clone());
                }
            }
        }
        aliases
    }
}

/// "Key value", "Key=value", or "Key = value"; comments and blank lines
/// are None. The key is lowercased; quotes around the value are removed.
fn split_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let split = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = line[..split].to_lowercase();
    let value = line[split..].trim_start_matches(|c: char| c.is_whitespace() || c == '=').trim();
    let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
    Some((key, value.to_string()))
}

/// OpenSSH's rule: a host matches if some pattern matches and no negated
/// pattern does.
fn matches(patterns: &[String], host: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        if let Some(negated) = pattern.strip_prefix('!') {
            if glob(negated, host) {
                return false;
            }
        } else if glob(pattern, host) {
            matched = true;
        }
    }
    matched
}

/// `*` and `?` wildcards.
pub fn glob(pattern: &str, text: &str) -> bool {
    fn inner(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => inner(&p[1..], t) || (!t.is_empty() && inner(p, &t[1..])),
            (Some('?'), Some(_)) => inner(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => inner(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    inner(&p, &t)
}

fn include_files(pattern: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = if let Some(rest) = pattern.strip_prefix("~/") {
        dirs::home_dir().unwrap_or_default().join(rest)
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        ssh_dir.join(pattern)
    };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let Some(dir) = path.parent() else { return Vec::new() };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().filter(|e| glob(&name, &e.file_name().to_string_lossy())).map(|e| e.path()).collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// `~`, `%d` (home), `%h` (host), `%r` (remote user), `%u` (local user),
/// `%%`.
fn expand(text: &str, host: &str, remote_user: &str, local_user: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default().display().to_string();
    let text = match text.strip_prefix('~') {
        Some(rest) => format!("{home}{rest}"),
        None => text.to_string(),
    };
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => out.push_str(&home),
            Some('h') => out.push_str(host),
            Some('r') => out.push_str(remote_user),
            Some('u') => out.push_str(local_user),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    PathBuf::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
# Defaults first: they apply to every host after more specific ones.
IdentityFile ~/.ssh/global_key

Host web prod-*
    HostName %h.example.com
    User deploy
    Port=2222
    IdentityFile "~/.ssh/web key"
    IdentitiesOnly yes

Host prod-db !prod-secret
    User dba

Host jump-target
    HostName 10.0.0.5
    ProxyJump bastion

Host *.internal
    User ops

Host *
    User fallback
    Port 22

Match host foo
    User never
"#;

    #[test]
    fn first_value_wins_and_identity_files_add_up() {
        let config = SshConfig::parse(CONFIG);
        let web = config.resolve("web");
        assert_eq!(web.host_name.as_deref(), Some("web.example.com"));
        assert_eq!(web.user.as_deref(), Some("deploy"));
        assert_eq!(web.port, Some(2222));
        let home = dirs::home_dir().unwrap();
        assert_eq!(web.identity_files, [home.join(".ssh/global_key"), home.join(".ssh/web key")]);
        assert_eq!(web.proxy, None);
        assert!(web.identities_only);
        assert!(!config.resolve("box.internal").identities_only);

        let db = config.resolve("prod-db");
        assert_eq!(db.user.as_deref(), Some("deploy"), "Host web prod-* comes first");
        assert_eq!(db.host_name.as_deref(), Some("prod-db.example.com"));

        let secret = config.resolve("PROD-SECRET");
        assert_eq!(secret.user.as_deref(), Some("deploy"), "case-insensitive; negation only affects its own block");
    }

    #[test]
    fn wildcards_proxies_and_defaults() {
        let config = SshConfig::parse(CONFIG);
        assert_eq!(config.resolve("box.internal").user.as_deref(), Some("ops"));
        let plain = config.resolve("plain.example.org");
        assert_eq!((plain.user.as_deref(), plain.port, plain.host_name), (Some("fallback"), Some(22), None));
        assert_eq!(config.resolve("jump-target").proxy.as_ref(), Some(&ProxyDirective::Jump("bastion".into())));
        assert_eq!(config.resolve("foo").user.as_deref(), Some("fallback"), "Match blocks are skipped");
        assert_eq!(config.aliases(), ["web", "prod-db", "jump-target"]);
    }

    #[test]
    fn globs_and_tokens() {
        assert!(glob("prod-*", "prod-db"));
        assert!(glob("h?st", "host"));
        assert!(!glob("h?st", "hoost"));
        assert!(glob("*", ""));
        assert_eq!(expand("/k/%r@%h_%%", "example.com", "me", "local"), PathBuf::from("/k/me@example.com_%"));
    }

    #[test]
    fn includes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("conf.d")).unwrap();
        std::fs::write(dir.path().join("conf.d/a.conf"), "Host inc\n  HostName included.example\n").unwrap();
        let mut config = SshConfig::default();
        config.add("Include conf.d/*.conf\nHost other\n  User x\n", dir.path(), 0);
        assert_eq!(config.resolve("inc").host_name.as_deref(), Some("included.example"));
        assert_eq!(config.resolve("other").user.as_deref(), Some("x"));
    }
}
