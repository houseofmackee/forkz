//! Network (in the sidebar): the file servers on the local network that
//! announce themselves with Bonjour (SMB, AFP, SFTP, and WebDAV), as a
//! folder of servers. Opening one connects to it, as Go to Folder does
//! with its address.
//!
//! Uses the system's dns-sd tool (through a pseudo-terminal, so it writes
//! each line as it finds things), for a moment each time the list is read.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

/// Service types looked for, in order of preference for a server that
/// announces several, with the address scheme for each.
const SERVICES: &[(&str, &str)] = &[
    ("_smb._tcp", "smb"),
    ("_afpovertcp._tcp", "afp"),
    ("_sftp-ssh._tcp", "sftp"),
    ("_webdavs._tcp", "davs"),
    ("_webdav._tcp", "dav"),
];

/// How long servers have to answer.
const BROWSE: Duration = Duration::from_millis(1800);
const RESOLVE: Duration = Duration::from_millis(1500);

#[derive(Default)]
pub struct NetworkFs {
    /// Server names to their addresses, from the last listing.
    found: Mutex<HashMap<String, String>>,
}

pub fn new() -> FsRef {
    Arc::new(NetworkFs::default())
}

/// Runs dns-sd for a while and returns what it wrote.
fn dns_sd(args: &[&str], wait: Duration) -> String {
    let child = Command::new("/usr/bin/script")
        .args(["-q", "/dev/null", "/usr/bin/dns-sd"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return String::new() };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_string(&mut text);
        }
        text
    });
    std::thread::sleep(wait);
    let _ = child.kill();
    let _ = child.wait();
    reader.join().unwrap_or_default()
}

/// Instance names from `dns-sd -B` output.
pub fn parse_browse(text: &str, service: &str) -> Vec<String> {
    let column = format!("{service}.");
    let mut names: Vec<String> = text
        .lines()
        .filter(|line| line.split_whitespace().nth(1) == Some("Add"))
        .filter_map(|line| line.find(&column).map(|at| line[at + column.len()..].trim().to_string()))
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The host and port from `dns-sd -L` output.
pub fn parse_resolve(text: &str) -> Option<(String, u16)> {
    let line = text.lines().find(|l| l.contains("can be reached at "))?;
    let rest = &line[line.find("can be reached at ")? + "can be reached at ".len()..];
    let address = rest.split_whitespace().next()?;
    let (host, port) = address.rsplit_once(':')?;
    Some((host.trim_end_matches('.').to_string(), port.parse().ok()?))
}

/// The address to connect with.
fn address(scheme: &str, host: &str, port: u16) -> String {
    let usual = match scheme {
        "smb" => 445,
        "afp" => 548,
        "sftp" => 22,
        "davs" => 443,
        _ => 80,
    };
    if port == usual { format!("{scheme}://{host}") } else { format!("{scheme}://{host}:{port}") }
}

/// The servers announced now: name to address.
fn discover() -> HashMap<String, String> {
    // Every type at once, then every server found at once.
    let browsing: Vec<_> = SERVICES
        .iter()
        .map(|(service, scheme)| {
            let service = service.to_string();
            let scheme = scheme.to_string();
            std::thread::spawn(move || {
                let text = dns_sd(&["-B", &service, "local."], BROWSE);
                parse_browse(&text, &service)
                    .into_iter()
                    .map(|name| (name, service.clone(), scheme.clone()))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut chosen: Vec<(String, String, String)> = Vec::new();
    for found in browsing.into_iter().filter_map(|t| t.join().ok()) {
        for (name, service, scheme) in found {
            // The first (preferred) kind of each server.
            if !chosen.iter().any(|(n, _, _)| *n == name) {
                chosen.push((name, service, scheme));
            }
        }
    }
    let resolving: Vec<_> = chosen
        .into_iter()
        .map(|(name, service, scheme)| {
            std::thread::spawn(move || {
                let text = dns_sd(&["-L", &name, &service, "local."], RESOLVE);
                parse_resolve(&text).map(|(host, port)| (name, address(&scheme, &host, port)))
            })
        })
        .collect();
    resolving.into_iter().filter_map(|t| t.join().ok().flatten()).collect()
}

fn server_entry(name: &str) -> Entry {
    Entry {
        name: name.to_string(),
        path: Path::new("/").join(name),
        kind: EntryKind::Folder,
        size: 0,
        modified: None::<SystemTime>,
        hidden: false,
        package: false,
        permissions: None,
        owner: None,
        group: None,
    }
}

fn not_here() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "Open a server to work with its files.")
}

impl Vfs for NetworkFs {
    fn key(&self) -> String {
        "network:".into()
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        "Network".into()
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        if dir != Path::new("/") {
            return Err(not_here());
        }
        let found = discover();
        let mut names: Vec<&String> = found.keys().collect();
        names.sort_by_key(|n| n.to_lowercase());
        let entries = names.iter().map(|n| server_entry(n)).collect();
        if let Ok(mut known) = self.found.lock() {
            *known = found;
        }
        Ok(entries)
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        if path == Path::new("/") {
            return Ok(server_entry(""));
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(server_entry(&name))
    }

    fn create_dir(&self, _path: &Path) -> io::Result<()> {
        Err(not_here())
    }

    fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(not_here())
    }

    fn remove_file(&self, _path: &Path) -> io::Result<()> {
        Err(not_here())
    }

    fn remove_dir(&self, _path: &Path) -> io::Result<()> {
        Err(not_here())
    }

    fn open_read(&self, _path: &Path) -> io::Result<Box<dyn Read + Send>> {
        Err(not_here())
    }

    fn open_write(&self, _path: &Path) -> io::Result<Box<dyn Write + Send>> {
        Err(not_here())
    }

    fn set_attributes(&self, _path: &Path, _modified: Option<SystemTime>, _permissions: Option<u32>) -> io::Result<()> {
        Err(not_here())
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        let found = self.found.lock().map(|f| f.clone()).unwrap_or_default();
        Ok(Arc::new(NetworkFs { found: Mutex::new(found) }))
    }

    fn display_path(&self, path: &Path) -> String {
        if path == Path::new("/") { "Network".into() } else { format!("Network{}", path.display()) }
    }

    fn read_only(&self) -> bool {
        true
    }

    fn link_target(&self, path: &Path) -> Option<String> {
        let name = path.file_name()?.to_string_lossy().into_owned();
        self.found.lock().ok()?.get(&name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_dns_sd_output() {
        let browse = "Browsing for _smb._tcp.local.\nDATE: ---Sat 07 Nov 2026---\n13:44:05.181  ...STARTING...\n\
Timestamp     A/R    Flags  if Domain               Service Type         Instance Name\n\
13:44:05.182  Add        3   1 local.               _smb._tcp.           Office NAS\n\
13:44:05.182  Add        2  14 local.               _smb._tcp.           Office NAS\n\
13:44:06.000  Add        2  14 local.               _smb._tcp.           Studio Mac\n\
13:44:07.000  Rmv        0  14 local.               _smb._tcp.           Gone\n";
        assert_eq!(parse_browse(browse, "_smb._tcp"), ["Office NAS", "Studio Mac"]);
        let resolve = "13:44:07.228  Office\\032NAS._smb._tcp.local. can be reached at office-nas.local.:445 (interface 14) Flags: 1\n";
        assert_eq!(parse_resolve(resolve), Some(("office-nas.local".to_string(), 445)));
        assert_eq!(parse_resolve("nothing here"), None);
        assert_eq!(address("smb", "nas.local", 445), "smb://nas.local");
        assert_eq!(address("sftp", "box.local", 2222), "sftp://box.local:2222");
    }
}
