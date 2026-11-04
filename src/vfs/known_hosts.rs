//! Which servers' host keys are trusted: those in `~/.ssh/known_hosts`
//! (read only) and those trusted in forkz, kept in
//! `~/Library/Application Support/forkz/known_hosts` (OpenSSH format).

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Trusted,
    /// Never seen: ask before connecting.
    Unknown,
    /// A different key than before: possibly an attack.
    Changed,
}

/// `FORKZ_KNOWN_HOSTS` replaces this file and ignores `~/.ssh/known_hosts`
/// (for tests).
pub fn forkz_file() -> Option<PathBuf> {
    if let Some(file) = std::env::var_os("FORKZ_KNOWN_HOSTS") {
        return Some(PathBuf::from(file));
    }
    dirs::data_dir().map(|d| d.join("forkz").join("known_hosts"))
}

fn ssh_file() -> Option<PathBuf> {
    if std::env::var_os("FORKZ_KNOWN_HOSTS").is_some() {
        return None;
    }
    dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

/// The host as known_hosts writes it: "host" for port 22, else "[host]:port".
pub fn host_pattern(host: &str, port: u16) -> String {
    if port == 22 { host.to_string() } else { format!("[{host}]:{port}") }
}

/// The key type in an SSH public key blob ("ssh-ed25519", "ssh-rsa", ...).
pub fn key_type_of(blob: &[u8]) -> Option<String> {
    let len = u32::from_be_bytes(blob.get(..4)?.try_into().ok()?) as usize;
    let name = blob.get(4..4 + len)?;
    std::str::from_utf8(name).ok().map(str::to_string)
}

/// Checks a server's key against both files.
pub fn check(session: &ssh2::Session, host: &str, port: u16, key: &[u8]) -> Status {
    let files: Vec<PathBuf> = [ssh_file(), forkz_file()].into_iter().flatten().filter(|f| f.exists()).collect();
    check_files(session, &files, host, port, key)
}

pub fn check_files(session: &ssh2::Session, files: &[PathBuf], host: &str, port: u16, key: &[u8]) -> Status {
    let mut changed = false;
    for file in files {
        let Ok(mut known) = session.known_hosts() else { continue };
        if known.read_file(file, ssh2::KnownHostFileKind::OpenSSH).is_err() {
            // libssh2 stops at lines it does not understand; fall back to
            // our own reading of plain entries.
            match check_plain(file, host, port, key) {
                Status::Trusted => return Status::Trusted,
                Status::Changed => changed = true,
                Status::Unknown => {}
            }
            continue;
        }
        match known.check_port(host, port, key) {
            ssh2::CheckResult::Match => return Status::Trusted,
            ssh2::CheckResult::Mismatch => changed = true,
            _ => {}
        }
    }
    if changed { Status::Changed } else { Status::Unknown }
}

/// Plain (not hashed) entries only.
fn check_plain(file: &Path, host: &str, port: u16, key: &[u8]) -> Status {
    let Ok(text) = std::fs::read_to_string(file) else { return Status::Unknown };
    let pattern = host_pattern(host, port);
    let key_type = key_type_of(key).unwrap_or_default();
    let encoded = STANDARD.encode(key);
    let mut changed = false;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hosts), Some(kind), Some(blob)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if !hosts.split(',').any(|h| h == pattern) {
            continue;
        }
        if blob == encoded {
            return Status::Trusted;
        }
        if kind == key_type {
            changed = true;
        }
    }
    if changed { Status::Changed } else { Status::Unknown }
}

/// The file of TLS certificates trusted in forkz (FTPS servers whose
/// certificate macOS does not trust): "host:port FINGERPRINT" lines.
fn certificates_file() -> Option<PathBuf> {
    forkz_file().map(|f| f.with_file_name("trusted_certificates"))
}

/// The fingerprint of the certificate trusted for a server, if any.
pub fn trusted_certificate(host: &str, port: u16) -> Option<String> {
    let text = std::fs::read_to_string(certificates_file()?).ok()?;
    let wanted = format!("{host}:{port}");
    text.lines().find_map(|line| {
        let (server, fingerprint) = line.split_once(' ')?;
        (server == wanted).then(|| fingerprint.trim().to_string())
    })
}

/// Trusts a certificate (by fingerprint) for a server, replacing another.
pub fn trust_certificate(host: &str, port: u16, fingerprint: &str) -> io::Result<()> {
    let file = certificates_file().ok_or_else(|| io::Error::other("no Application Support folder"))?;
    let server = format!("{host}:{port}");
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    let mut lines: Vec<String> =
        existing.lines().filter(|l| l.split_once(' ').is_none_or(|(s, _)| s != server)).map(str::to_string).collect();
    lines.push(format!("{server} {fingerprint}"));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, lines.join("\n") + "\n")
}

/// Trusts `key` for the host from now on, replacing any key of the same
/// type trusted before.
pub fn trust(host: &str, port: u16, key: &[u8]) -> io::Result<()> {
    let file = forkz_file().ok_or_else(|| io::Error::other("no Application Support folder"))?;
    trust_in(&file, host, port, key)
}

pub fn trust_in(file: &Path, host: &str, port: u16, key: &[u8]) -> io::Result<()> {
    let key_type = key_type_of(key).ok_or_else(|| io::Error::other("unrecognized host key"))?;
    let pattern = host_pattern(host, port);
    let existing = std::fs::read_to_string(file).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|line| {
            let mut parts = line.split_whitespace();
            !(parts.next() == Some(pattern.as_str()) && parts.next() == Some(key_type.as_str()))
        })
        .map(str::to_string)
        .collect();
    lines.push(format!("{pattern} {key_type} {}", STANDARD.encode(key)));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(kind: &str, body: &[u8]) -> Vec<u8> {
        let mut blob = (kind.len() as u32).to_be_bytes().to_vec();
        blob.extend_from_slice(kind.as_bytes());
        blob.extend_from_slice(body);
        blob
    }

    #[test]
    fn key_types_and_patterns() {
        assert_eq!(key_type_of(&blob("ssh-ed25519", b"xyz")).as_deref(), Some("ssh-ed25519"));
        assert_eq!(key_type_of(b"\x00\x00"), None);
        assert_eq!(host_pattern("example.com", 22), "example.com");
        assert_eq!(host_pattern("example.com", 2222), "[example.com]:2222");
    }

    #[test]
    fn trust_replaces_old_key_and_checks_plain_entries() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known_hosts");
        let old = blob("ssh-ed25519", b"old");
        let new = blob("ssh-ed25519", b"new");
        trust_in(&file, "h.example", 22, &old).unwrap();
        assert_eq!(check_plain(&file, "h.example", 22, &old), Status::Trusted);
        assert_eq!(check_plain(&file, "h.example", 22, &new), Status::Changed);
        assert_eq!(check_plain(&file, "other.example", 22, &new), Status::Unknown);
        trust_in(&file, "h.example", 22, &new).unwrap();
        assert_eq!(check_plain(&file, "h.example", 22, &new), Status::Trusted);
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
    }
}
