//! SFTP through libssh2: connecting (host key check, then password, key
//! file, or SSH agent), browsing, and transfers. A dropped connection is
//! reopened on the next call with the credentials that worked.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use ssh2::{ErrorCode, Session, Sftp};

use super::known_hosts::{self, Status};
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthMethod {
    #[default]
    Password,
    /// A private key file (asks for its passphrase if it has one).
    Key,
    /// Keys from the SSH agent.
    Agent,
}

#[derive(Clone)]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    pub key_file: Option<PathBuf>,
    /// A saved password (or key passphrase); asked for when missing or wrong.
    pub password: Option<String>,
    pub timeout: Duration,
    /// More private keys to try (from ~/.ssh/config, and FileZilla's
    /// "SFTP keyfiles").
    pub extra_keys: Vec<PathBuf>,
    /// IdentitiesOnly: offer only `extra_keys` (and the key file), not the
    /// agent's keys or the default ones in ~/.ssh.
    pub identities_only: bool,
    /// Reach the server through a command or jump hosts instead of a
    /// direct connection.
    pub proxy: Option<Proxy>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Proxy {
    /// A shell command (tokens already replaced) whose input and output
    /// carry the connection.
    Command(String),
    /// Jump hosts as in ProxyJump ("bastion" or "user@a:22,b"), through
    /// the system's ssh, which handles their keys and host checks.
    Jump(String),
}

impl Proxy {
    /// The shell command that carries the connection to `host`:`port`.
    pub fn command(&self, host: &str, port: u16) -> String {
        match self {
            Proxy::Command(command) => command.clone(),
            Proxy::Jump(jumps) => {
                let hops: Vec<&str> = jumps.split(',').map(str::trim).filter(|h| !h.is_empty()).collect();
                let (last, rest) = hops.split_last().map_or(("", &[][..]), |(last, rest)| (*last, rest));
                let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
                let mut command = String::from("/usr/bin/ssh -o BatchMode=yes");
                if !rest.is_empty() {
                    command.push_str(&format!(" -J {}", shell_quote(&rest.join(","))));
                }
                // "host:port" is fine in -J but not as the destination,
                // where ssh wants ssh://host:port.
                let last = if last.contains(':') && !last.contains("://") {
                    format!("ssh://{last}")
                } else {
                    last.to_string()
                };
                command.push_str(&format!(" -W {} {}", shell_quote(&target), shell_quote(&last)));
                command
            }
        }
    }
}

/// Single-quotes text for /bin/sh.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The proxy command of a connection; stopped with it.
struct ProxyProcess {
    child: std::process::Child,
}

impl ProxyProcess {
    /// What the command printed on errors (after it is stopped).
    fn stop(mut self) -> String {
        let _ = self.child.kill();
        let mut text = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut text);
        }
        let _ = self.child.wait();
        text.trim().to_string()
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// User and group names on the server, by number (to show owners).
#[derive(Debug, Default, Clone)]
pub struct Names {
    users: std::collections::HashMap<u32, String>,
    groups: std::collections::HashMap<u32, String>,
}

const NAMES_SEPARATOR: &str = "--forkz-groups--";

impl Names {
    /// The output of `getent passwd`, the separator, then `getent group`.
    pub fn parse(text: &str) -> Names {
        let (passwd, group) = text.split_once(NAMES_SEPARATOR).unwrap_or((text, ""));
        let table = |text: &str| {
            text.lines()
                .filter_map(|line| {
                    let mut fields = line.split(':');
                    let name = fields.next()?.trim();
                    let id = fields.nth(1)?.trim().parse().ok()?;
                    (!name.is_empty() && !name.starts_with('#')).then(|| (id, name.to_string()))
                })
                .collect()
        };
        Names { users: table(passwd), groups: table(group) }
    }

    fn user(&self, id: Option<u32>) -> Option<String> {
        id.map(|id| self.users.get(&id).cloned().unwrap_or_else(|| id.to_string()))
    }

    fn group(&self, id: Option<u32>) -> Option<String> {
        id.map(|id| self.groups.get(&id).cloned().unwrap_or_else(|| id.to_string()))
    }
}

/// Asks the server for its user and group names. Servers that only allow
/// SFTP refuse: then owners show as numbers.
fn load_names(session: &Session) -> Names {
    let script = format!(
        "(getent passwd || cat /etc/passwd) 2>/dev/null; echo {NAMES_SEPARATOR}; (getent group || cat /etc/group) 2>/dev/null"
    );
    session.set_timeout(5_000);
    let result = (|| -> Result<String, ssh2::Error> {
        let mut channel = session.channel_session()?;
        channel.exec(&script)?;
        let mut text = String::new();
        let _ = (&mut channel).take(4 * 1024 * 1024).read_to_string(&mut text);
        let _ = channel.wait_close();
        Ok(text)
    })();
    session.set_timeout(120_000);
    match result {
        Ok(text) => Names::parse(&text),
        Err(e) => {
            tracing::debug!("no user names from the server: {e}");
            Names::default()
        }
    }
}

impl std::fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}:{} ({:?})", self.user, self.host, self.port, self.auth)
    }
}

/// Questions while connecting; called from the connecting thread, and
/// blocks until answered.
pub trait Prompter: Send + Sync {
    /// Trust this host key? `status` is Unknown or Changed.
    fn host_key(&self, host: &str, port: u16, key_type: &str, fingerprint: &str, status: Status) -> bool;
    /// A password or passphrase; None if cancelled.
    fn secret(&self, message: &str, informative: &str) -> Option<String>;
}

struct Conn {
    session: Session,
    sftp: Sftp,
    /// The proxy command, if any; it lives as long as the connection.
    _proxy: Option<ProxyProcess>,
}

pub struct SftpFs {
    params: ConnectParams,
    conn: Mutex<Option<Conn>>,
    home: PathBuf,
    /// The password or passphrase that worked, to reconnect and to offer
    /// saving it.
    secret: Option<String>,
    names: Names,
}

impl std::fmt::Debug for SftpFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SftpFs({:?})", self.params)
    }
}

/// Connects, asking `prompter` about unknown host keys and missing
/// passwords. Runs on a background thread.
pub fn connect(params: ConnectParams, prompter: &dyn Prompter) -> io::Result<Arc<SftpFs>> {
    let mut secret = params.password.clone();
    let conn = open(&params, Some(prompter), &mut secret)?;
    let home = conn.sftp.realpath(Path::new(".")).unwrap_or_else(|_| PathBuf::from("/"));
    let names = load_names(&conn.session);
    let fs = Arc::new(SftpFs { params, conn: Mutex::new(Some(conn)), home, secret, names });
    keep_alive(Arc::downgrade(&fs));
    Ok(fs)
}

impl SftpFs {
    /// The password (or passphrase) that worked.
    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    pub fn params(&self) -> &ConnectParams {
        &self.params
    }

    /// Runs `f` with the connection, reconnecting once if it was dropped.
    fn with<T>(&self, f: impl Fn(&Sftp) -> Result<T, ssh2::Error>) -> io::Result<T> {
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if guard.is_none() {
                let mut secret = self.secret.clone();
                *guard = Some(open(&self.params, None, &mut secret)?);
            }
            let conn = guard.as_ref().expect("connected above");
            match f(&conn.sftp) {
                Ok(value) => return Ok(value),
                Err(e) if attempt == 0 && is_connection_error(&e) => {
                    tracing::info!("reconnecting to {:?}: {e}", self.params);
                    *guard = None;
                }
                Err(e) => return Err(to_io(e)),
            }
        }
        Err(io::Error::other("the connection was lost"))
    }
}

fn is_connection_error(error: &ssh2::Error) -> bool {
    use ssh2::ErrorCode::Session as S;
    // LIBSSH2_ERROR_SOCKET_NONE, SOCKET_SEND, SOCKET_DISCONNECT, SOCKET_RECV,
    // SOCKET_TIMEOUT, TIMEOUT, CHANNEL_CLOSED, CHANNEL_EOF_SENT.
    matches!(error.code(), S(-1) | S(-7) | S(-13) | S(-43) | S(-30) | S(-9) | S(-26) | S(-27))
}

/// libssh2's errors with clearer messages for the common SFTP codes.
fn to_io(error: ssh2::Error) -> io::Error {
    match error.code() {
        ErrorCode::SFTP(2) | ErrorCode::SFTP(10) => io::Error::new(io::ErrorKind::NotFound, "No such file or folder."),
        ErrorCode::SFTP(3) => io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied."),
        ErrorCode::SFTP(11) => io::Error::new(io::ErrorKind::AlreadyExists, "The item already exists."),
        ErrorCode::SFTP(4) => io::Error::other("The server refused (the folder may not be empty)."),
        ErrorCode::SFTP(18) => io::Error::other("The folder is not empty."),
        _ => io::Error::other(error.message().to_string()),
    }
}

/// Keeps an idle connection open (servers drop silent ones), until the
/// file system is dropped.
fn keep_alive(fs: Weak<SftpFs>) {
    let _ = std::thread::Builder::new().name("sftp keepalive".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(20));
            let Some(fs) = fs.upgrade() else { return };
            // Skip while busy: the connection is in use, so it is alive.
            if let Ok(guard) = fs.conn.try_lock()
                && let Some(conn) = guard.as_ref()
            {
                let _ = conn.session.keepalive_send();
            }
        }
    });
}

/// Opens a connection. Without a prompter (reconnecting), only already
/// trusted host keys and known credentials are used.
fn open(params: &ConnectParams, prompter: Option<&dyn Prompter>, secret: &mut Option<String>) -> io::Result<Conn> {
    let mut session = Session::new().map_err(to_io)?;
    let mut proxy = None;
    match &params.proxy {
        None => session.set_tcp_stream(tcp_connect(&params.host, params.port, params.timeout)?),
        Some(how) => {
            let (stream, process) = spawn_proxy(&how.command(&params.host, params.port))?;
            session.set_tcp_stream(stream);
            proxy = Some(process);
        }
    }
    session.set_timeout(params.timeout.as_millis().min(u32::MAX as u128) as u32);
    if let Err(e) = session.handshake() {
        // The proxy's own error says more (for example a jump host refusing).
        let detail = proxy.map(ProxyProcess::stop).filter(|t| !t.is_empty());
        let message = match detail {
            Some(detail) => format!("The SSH connection through the proxy failed: {detail}"),
            None => format!("The SSH connection failed: {}", e.message()),
        };
        return Err(io::Error::other(message));
    }
    verify_host(&session, params, prompter)?;
    authenticate(&session, params, prompter, secret)?;
    session.set_keepalive(false, 20);
    // Transfers can take a while per call on slow links.
    session.set_timeout(120_000);
    let sftp = session.sftp().map_err(|e| io::Error::other(format!("The server has no SFTP: {}", e.message())))?;
    Ok(Conn { session, sftp, _proxy: proxy })
}

/// Runs a proxy command with one end of a socket pair as its input and
/// output; the other end is the connection.
fn spawn_proxy(command: &str) -> io::Result<(std::os::unix::net::UnixStream, ProxyProcess)> {
    use std::os::fd::OwnedFd;
    use std::process::{Command, Stdio};
    let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    let input = OwnedFd::from(theirs.try_clone()?);
    let output = OwnedFd::from(theirs);
    let child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| io::Error::other(format!("The proxy command could not start: {e}")))?;
    tracing::debug!("proxy: {command}");
    Ok((ours, ProxyProcess { child }))
}

fn tcp_connect(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    let addresses = (host, port).to_socket_addrs().map_err(|e| {
        io::Error::new(io::ErrorKind::NotFound, format!("\u{201c}{host}\u{201d} could not be found: {e}"))
    })?;
    let mut last = io::Error::new(io::ErrorKind::NotFound, format!("\u{201c}{host}\u{201d} has no addresses."));
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(io::Error::new(last.kind(), format!("Could not connect to {host}:{port}: {last}")))
}

fn verify_host(session: &Session, params: &ConnectParams, prompter: Option<&dyn Prompter>) -> io::Result<()> {
    let (key, _) = session.host_key().ok_or_else(|| io::Error::other("The server sent no host key."))?;
    let key = key.to_vec();
    let status = known_hosts::check(session, &params.host, params.port, &key);
    if status == Status::Trusted {
        return Ok(());
    }
    let refused =
        || io::Error::new(io::ErrorKind::PermissionDenied, format!("The host key of {} is not trusted.", params.host));
    let Some(prompter) = prompter else { return Err(refused()) };
    let fingerprint = session
        .host_key_hash(ssh2::HashType::Sha256)
        .map(|hash| format!("SHA256:{}", STANDARD_NO_PAD.encode(hash)))
        .unwrap_or_default();
    let key_type = known_hosts::key_type_of(&key).unwrap_or_else(|| "unknown".into());
    if !prompter.host_key(&params.host, params.port, &key_type, &fingerprint, status) {
        // The user said no: nothing to report.
        return Err(cancelled());
    }
    known_hosts::trust(&params.host, params.port, &key)
}

fn authenticate(
    session: &Session,
    params: &ConnectParams,
    prompter: Option<&dyn Prompter>,
    secret: &mut Option<String>,
) -> io::Result<()> {
    let user = params.user.as_str();
    // Also starts the "none" method; some servers let us in right away.
    let methods = session.auth_methods(user).map(str::to_string).unwrap_or_default();
    if session.authenticated() {
        return Ok(());
    }
    let allows = |m: &str| methods.split(',').any(|x| x == m);

    let try_agent = || -> bool {
        if params.identities_only {
            return false;
        }
        let Ok(mut agent) = session.agent() else { return false };
        if agent.connect().is_err() || agent.list_identities().is_err() {
            return false;
        }
        let identities = agent.identities().unwrap_or_default();
        identities.iter().any(|identity| agent.userauth(user, identity).is_ok())
    };

    match params.auth {
        AuthMethod::Key => {
            let key = params.key_file.clone().ok_or_else(|| io::Error::other("No key file is set for this site."))?;
            let key = expand_tilde(&key);
            for attempt in 0..4 {
                if session.userauth_pubkey_file(user, None, &key, secret.as_deref()).is_ok() {
                    return Ok(());
                }
                // Probably a passphrase is needed (or it was wrong).
                let Some(prompter) = prompter.filter(|_| attempt < 3) else { break };
                let message = format!("Enter the passphrase for \u{201c}{}\u{201d}", key.display());
                let informative = if attempt == 0 { "" } else { "The passphrase was not accepted." };
                match prompter.secret(&message, informative) {
                    Some(passphrase) => *secret = Some(passphrase),
                    None => return Err(cancelled()),
                }
            }
            Err(denied(&params.host, "The key was not accepted."))
        }
        AuthMethod::Agent => {
            if allows("publickey") && try_agent() {
                return Ok(());
            }
            if try_default_keys(session, user, &params.extra_keys, params.identities_only) {
                return Ok(());
            }
            Err(denied(&params.host, "No key from the SSH agent or ~/.ssh was accepted."))
        }
        AuthMethod::Password => {
            // Nothing saved: a key from the agent or ~/.ssh may do, like
            // FileZilla does with the agent, before asking for a password.
            if secret.is_none()
                && allows("publickey")
                && (try_agent() || try_default_keys(session, user, &params.extra_keys, params.identities_only))
            {
                return Ok(());
            }
            for attempt in 0..4 {
                if let Some(password) = secret.clone() {
                    if allows("password") && session.userauth_password(user, &password).is_ok() {
                        return Ok(());
                    }
                    if allows("keyboard-interactive") {
                        let mut answer = AnswerWith(password);
                        if session.userauth_keyboard_interactive(user, &mut answer).is_ok() {
                            return Ok(());
                        }
                    }
                }
                if !allows("password") && !allows("keyboard-interactive") && allows("publickey") {
                    // A key-only server: try the agent and default keys.
                    if try_agent() || try_default_keys(session, user, &params.extra_keys, params.identities_only) {
                        return Ok(());
                    }
                    return Err(denied(&params.host, "The server only accepts keys, and none was accepted."));
                }
                let Some(prompter) = prompter.filter(|_| attempt < 3) else { break };
                let message = format!("Enter the password for {user}@{}", params.host);
                let informative = if secret.is_some() { "The password was not accepted." } else { "" };
                match prompter.secret(&message, informative) {
                    Some(password) => *secret = Some(password),
                    None => return Err(cancelled()),
                }
            }
            Err(denied(&params.host, "The password was not accepted."))
        }
    }
}

/// The usual keys in ~/.ssh, then the extra ones; only keys without a
/// passphrase.
fn try_default_keys(session: &Session, user: &str, extra: &[PathBuf], only_extra: bool) -> bool {
    let ssh = dirs::home_dir().map(|h| h.join(".ssh")).unwrap_or_default();
    let defaults: &[&str] = if only_extra { &[] } else { &["id_ed25519", "id_ecdsa", "id_rsa"] };
    // The configured keys first, then the usual ones.
    extra
        .iter()
        .map(|k| expand_tilde(k))
        .chain(defaults.iter().map(|name| ssh.join(name)))
        .filter(|key| key.exists())
        .any(|key| session.userauth_pubkey_file(user, None, &key, None).is_ok())
}

fn expand_tilde(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        Err(_) => path.to_path_buf(),
    }
}

fn denied(host: &str, detail: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, format!("{host} refused the login. {detail}"))
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Cancelled")
}

/// Answers keyboard-interactive prompts with the password.
struct AnswerWith(String);

impl ssh2::KeyboardInteractivePrompt for AnswerWith {
    fn prompt<'a>(&mut self, _user: &str, _instructions: &str, prompts: &[ssh2::Prompt<'a>]) -> Vec<String> {
        prompts.iter().map(|_| self.0.clone()).collect()
    }
}

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

fn entry_from(path: &Path, stat: &ssh2::FileStat, names: &Names, link_is_folder: impl FnOnce() -> bool) -> Entry {
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mode = stat.perm.unwrap_or(0);
    let kind = match mode & S_IFMT {
        S_IFDIR => EntryKind::Folder,
        S_IFLNK => EntryKind::Symlink { target_is_folder: link_is_folder() },
        _ => EntryKind::File,
    };
    Entry {
        hidden: name.starts_with('.'),
        name,
        path: path.to_path_buf(),
        kind,
        size: if kind == EntryKind::File { stat.size.unwrap_or(0) } else { 0 },
        modified: stat.mtime.map(|t| UNIX_EPOCH + Duration::from_secs(t)),
        // Servers have no app bundles: every folder opens as a folder.
        package: false,
        permissions: stat.perm.map(|p| p & 0o7777),
        owner: names.user(stat.uid),
        group: names.group(stat.gid),
    }
}

impl Vfs for SftpFs {
    fn key(&self) -> String {
        format!("sftp://{}@{}:{}", self.params.user, self.params.host, self.params.port)
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        format!("{}@{}", self.params.user, self.params.host)
    }

    fn home(&self) -> PathBuf {
        self.home.clone()
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        self.with(|sftp| {
            let items = sftp.readdir(dir)?;
            Ok(items
                .iter()
                .map(|(path, stat)| {
                    entry_from(path, stat, &self.names, || {
                        sftp.stat(path).ok().and_then(|s| s.perm).is_some_and(|p| p & S_IFMT == S_IFDIR)
                    })
                })
                .collect())
        })
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        self.with(|sftp| {
            let stat = sftp.lstat(path)?;
            Ok(entry_from(path, &stat, &self.names, || {
                sftp.stat(path).ok().and_then(|s| s.perm).is_some_and(|p| p & S_IFMT == S_IFDIR)
            }))
        })
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.with(|sftp| sftp.mkdir(path, 0o755))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.with(|sftp| sftp.rename(from, to, None))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.with(|sftp| sftp.unlink(path))
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.with(|sftp| sftp.rmdir(path))
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let file = self.with(|sftp| sftp.open(path))?;
        Ok(Box::new(file))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        let file = self.with(|sftp| sftp.create(path))?;
        Ok(Box::new(file))
    }

    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, permissions: Option<u32>) -> io::Result<()> {
        let mtime = modified.and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
        if mtime.is_none() && permissions.is_none() {
            return Ok(());
        }
        let stat = ssh2::FileStat { size: None, uid: None, gid: None, perm: permissions, atime: mtime, mtime };
        self.with(|sftp| sftp.setstat(path, stat.clone()))
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        let mut secret = self.secret.clone();
        let conn = open(&self.params, None, &mut secret)?;
        let fs = Arc::new(SftpFs {
            params: self.params.clone(),
            conn: Mutex::new(Some(conn)),
            home: self.home.clone(),
            secret,
            names: self.names.clone(),
        });
        Ok(fs)
    }

    fn display_path(&self, path: &Path) -> String {
        let port = if self.params.port == 22 { String::new() } else { format!(":{}", self.params.port) };
        format!("sftp://{}@{}{port}{}", self.params.user, self.params.host, path.display())
    }
}

impl Drop for SftpFs {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.conn.lock()
            && let Some(conn) = guard.take()
        {
            let _ = conn.session.disconnect(None, "bye", None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_commands() {
        let one = Proxy::Jump("bastion".into()).command("10.0.0.5", 22);
        assert_eq!(one, "/usr/bin/ssh -o BatchMode=yes -W '10.0.0.5:22' 'bastion'");
        let chain = Proxy::Jump("me@a:2200, b".into()).command("fe80::1", 2222);
        assert_eq!(chain, "/usr/bin/ssh -o BatchMode=yes -J 'me@a:2200' -W '[fe80::1]:2222' 'b'");
        let port = Proxy::Jump("me@jump:2299".into()).command("h", 22);
        assert_eq!(port, "/usr/bin/ssh -o BatchMode=yes -W 'h:22' 'ssh://me@jump:2299'");
        assert_eq!(Proxy::Command("nc %h".into()).command("x", 1), "nc %h");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn server_names() {
        let names = Names::parse(
            "root:x:0:0:root:/root:/bin/bash\nwww-data:x:33:33::/var/www:/usr/sbin/nologin\n--forkz-groups--\nroot:x:0:\nwww-data:x:33:\n",
        );
        assert_eq!(names.user(Some(33)).as_deref(), Some("www-data"));
        assert_eq!(names.group(Some(0)).as_deref(), Some("root"));
        assert_eq!(names.user(Some(1001)).as_deref(), Some("1001"), "unknown: the number");
        assert_eq!(names.user(None), None);
        assert_eq!(Names::parse("").user(Some(5)).as_deref(), Some("5"));
    }
}
