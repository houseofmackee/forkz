//! FTP and FTPS (explicit TLS, AUTH TLS) through suppaftp, with macOS's TLS.
//! Certificates the system trusts are accepted; others are shown to the user
//! (SHA-256 fingerprint) once and remembered, like SSH host keys.

use std::io::{self, Read, Write};
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use suppaftp::list::{File as FtpFile, ListParser};
use suppaftp::types::FileType;
use suppaftp::{FtpError, NativeTlsConnector, NativeTlsFtpStream, TlsConnector};

/// suppaftp's TLS stream (its type is not exported by name).
type TlsStream = <NativeTlsConnector as TlsConnector>::Stream;

use super::known_hosts::{self, Status};
use super::sftp::Prompter;
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

/// A listed file, with its owner and group if the listing had them.
type Listed = (FtpFile, Option<(String, String)>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Security {
    /// AUTH TLS if the server offers it, plain FTP otherwise.
    TlsIfAvailable,
    /// AUTH TLS required.
    Tls,
    /// Plain FTP: passwords and files travel unencrypted.
    Plain,
}

#[derive(Clone)]
pub struct FtpParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub security: Security,
    pub timeout: Duration,
}

/// The TLS connector: macOS's TLS, recording each peer certificate's
/// fingerprint and, when one is expected, refusing any other.
#[derive(Clone)]
struct Connector {
    inner: native_tls::TlsConnector,
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl Connector {
    fn new(accept_any: bool, expected: Option<String>) -> io::Result<Connector> {
        let inner = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(accept_any)
            .danger_accept_invalid_hostnames(accept_any)
            .build()
            .map_err(io::Error::other)?;
        Ok(Connector { inner, expected, seen: Arc::new(Mutex::new(None)) })
    }

    fn seen(&self) -> Option<String> {
        self.seen.lock().ok().and_then(|s| s.clone())
    }
}

/// "AB:CD:..." of a certificate (DER).
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Connector(expected: {:?})", self.expected)
    }
}

impl TlsConnector for Connector {
    type Stream = TlsStream;

    fn connect(&self, domain: &str, stream: std::net::TcpStream) -> suppaftp::FtpResult<Self::Stream> {
        let stream = self.inner.connect(domain, stream).map_err(|e| FtpError::SecureError(e.to_string()))?;
        let seen = stream.peer_certificate().ok().flatten().and_then(|c| c.to_der().ok()).map(|der| fingerprint(&der));
        if let Some(expected) = &self.expected
            && seen.as_ref() != Some(expected)
        {
            return Err(FtpError::SecureError("the server's certificate changed".into()));
        }
        if let Ok(mut slot) = self.seen.lock() {
            *slot = seen;
        }
        Ok(stream.into())
    }
}

type Stream = NativeTlsFtpStream;

struct Conn {
    ftp: Stream,
}

pub struct FtpFs {
    params: FtpParams,
    conn: Mutex<Option<Conn>>,
    home: PathBuf,
    secret: Option<String>,
    /// Whether the connection is encrypted (for the label).
    secure: bool,
}

fn to_io(error: FtpError) -> io::Error {
    match error {
        FtpError::ConnectionError(e) => e,
        FtpError::UnexpectedResponse(response) => {
            let text = String::from_utf8_lossy(&response.body).trim().to_string();
            let kind = match response.status.code() {
                550 => io::ErrorKind::NotFound,
                530 | 532 => io::ErrorKind::PermissionDenied,
                _ => io::ErrorKind::Other,
            };
            io::Error::new(
                kind,
                if text.is_empty() { format!("The server answered {}.", response.status.code()) } else { text },
            )
        }
        other => io::Error::other(other.to_string()),
    }
}

fn is_connection_error(error: &FtpError) -> bool {
    matches!(error, FtpError::ConnectionError(_))
        || matches!(error, FtpError::UnexpectedResponse(r) if r.status.code() == 421)
}

/// Connects and logs in, asking about untrusted certificates and missing
/// passwords. Runs on a background thread.
pub fn connect(params: FtpParams, prompter: &dyn Prompter) -> io::Result<Arc<FtpFs>> {
    let mut secret = params.password.clone();
    let (conn, secure) = open(&params, Some(prompter), &mut secret)?;
    let mut conn = conn;
    let home = conn.ftp.pwd().map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"));
    Ok(Arc::new(FtpFs { params, conn: Mutex::new(Some(conn)), home, secret, secure }))
}

fn tcp(params: &FtpParams) -> io::Result<Stream> {
    let address = (params.host.as_str(), params.port).to_socket_addrs()?.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, format!("\u{201c}{}\u{201d} could not be found.", params.host))
    })?;
    let ftp = Stream::connect_timeout(address, params.timeout).map_err(to_io)?;
    ftp.get_ref().set_read_timeout(Some(Duration::from_secs(120))).ok();
    Ok(ftp)
}

fn open(params: &FtpParams, prompter: Option<&dyn Prompter>, secret: &mut Option<String>) -> io::Result<(Conn, bool)> {
    let (mut ftp, secure) = secure_channel(params, prompter)?;
    // Passwords: the saved one, then asking (up to three times).
    for attempt in 0..4 {
        let password = secret.clone().unwrap_or_default();
        match ftp.login(params.user.as_str(), password.as_str()) {
            Ok(()) => {
                ftp.transfer_type(FileType::Binary).map_err(to_io)?;
                return Ok((Conn { ftp }, secure));
            }
            Err(FtpError::UnexpectedResponse(r)) if r.status.code() == 530 => {
                let Some(prompter) = prompter.filter(|_| attempt < 3) else { break };
                let message = format!("Enter the password for {}@{}", params.user, params.host);
                let informative = if secret.is_some() { "The password was not accepted." } else { "" };
                match prompter.secret(&message, informative) {
                    Some(password) => *secret = Some(password),
                    None => return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled")),
                }
                // A rejected login may close the connection: start again.
                let (fresh, _) = secure_channel(params, prompter.into())?;
                ftp = fresh;
            }
            Err(e) => return Err(to_io(e)),
        }
    }
    Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} refused the login.", params.host)))
}

/// The control connection, with TLS as the settings ask, checking the
/// certificate. Returns whether it is encrypted.
fn secure_channel(params: &FtpParams, prompter: Option<&dyn Prompter>) -> io::Result<(Stream, bool)> {
    if params.security == Security::Plain {
        return Ok((tcp(params)?, false));
    }
    let remembered = known_hosts::trusted_certificate(&params.host, params.port);
    // First as the system would: a trusted certificate needs no questions.
    let system = Connector::new(false, None)?;
    match tcp(params)?.into_secure(system, &params.host) {
        Ok(ftp) => return Ok((ftp, true)),
        Err(FtpError::UnexpectedResponse(r))
            if params.security == Security::TlsIfAvailable && r.status.code() >= 500 =>
        {
            // No AUTH TLS on this server.
            tracing::info!("{} has no TLS; using plain FTP", params.host);
            return Ok((tcp(params)?, false));
        }
        Err(FtpError::SecureError(e)) => tracing::info!("{}: certificate not trusted by the system: {e}", params.host),
        Err(e) => return Err(to_io(e)),
    }
    // Not trusted by the system: a certificate trusted in forkz, or asking.
    let connector = Connector::new(true, remembered.clone())?;
    let watch = connector.clone();
    let ftp = match tcp(params)?.into_secure(connector, &params.host) {
        Ok(ftp) => ftp,
        Err(FtpError::SecureError(e)) if remembered.is_some() => {
            // A different certificate than the one trusted before.
            let seen = Connector::new(true, None)?;
            let probe = seen.clone();
            let ftp = tcp(params)?.into_secure(seen, &params.host).map_err(to_io)?;
            let fingerprint = probe.seen().unwrap_or_default();
            tracing::warn!("{}: {e}", params.host);
            ask_certificate(params, prompter, &fingerprint, Status::Changed)?;
            return Ok((ftp, true));
        }
        Err(e) => return Err(to_io(e)),
    };
    if remembered.is_none() {
        let fingerprint = watch.seen().unwrap_or_default();
        ask_certificate(params, prompter, &fingerprint, Status::Unknown)?;
    }
    Ok((ftp, true))
}

fn ask_certificate(
    params: &FtpParams,
    prompter: Option<&dyn Prompter>,
    fingerprint: &str,
    status: Status,
) -> io::Result<()> {
    let refused = || {
        io::Error::new(io::ErrorKind::PermissionDenied, format!("The certificate of {} is not trusted.", params.host))
    };
    let Some(prompter) = prompter else { return Err(refused()) };
    if !prompter.host_key(&params.host, params.port, "TLS certificate", &format!("SHA256:{fingerprint}"), status) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
    }
    known_hosts::trust_certificate(&params.host, params.port, fingerprint)
}

impl FtpFs {
    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    fn with<T>(&self, f: impl Fn(&mut Stream) -> Result<T, FtpError>) -> io::Result<T> {
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        for attempt in 0..2 {
            if guard.is_none() {
                let mut secret = self.secret.clone();
                *guard = Some(open(&self.params, None, &mut secret)?.0);
            }
            let conn = guard.as_mut().expect("connected above");
            match f(&mut conn.ftp) {
                Ok(value) => return Ok(value),
                Err(e) if attempt == 0 && is_connection_error(&e) => {
                    tracing::info!("reconnecting to {}: {e}", self.params.host);
                    *guard = None;
                }
                Err(e) => return Err(to_io(e)),
            }
        }
        Err(io::Error::other("the connection was lost"))
    }

    fn path_text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// The folder's items, from MLSD (exact) or LIST (parsed).
    fn read_dir(ftp: &mut Stream, dir: &Path) -> Result<Vec<Listed>, FtpError> {
        let text = Self::path_text(dir);
        if let Ok(lines) = ftp.mlsd(Some(&text)) {
            return Ok(lines.iter().filter_map(|l| ListParser::parse_mlsd(l).ok()).map(|f| (f, None)).collect());
        }
        let lines = ftp.list(Some(&text))?;
        Ok(lines
            .iter()
            .filter_map(|line| {
                let file = ListParser::parse_posix(line).or_else(|_| ListParser::parse_dos(line)).ok()?;
                // Owner and group names, from "drwxr-xr-x 2 owner group ...".
                let fields: Vec<&str> = line.split_whitespace().collect();
                let names = (fields.len() > 4 && line.starts_with(['-', 'd', 'l']))
                    .then(|| (fields[2].to_string(), fields[3].to_string()));
                Some((file, names))
            })
            .collect())
    }

    fn entry(&self, ftp: &mut Stream, dir: &Path, file: &FtpFile, names: Option<(String, String)>) -> Entry {
        let name = file.name().to_string();
        let path = dir.join(&name);
        let kind = if file.is_directory() {
            EntryKind::Folder
        } else if file.is_symlink() {
            // A link to a folder can be entered.
            let target_is_folder = ftp.cwd(Self::path_text(&path)).is_ok();
            EntryKind::Symlink { target_is_folder }
        } else {
            EntryKind::File
        };
        let permissions = {
            use suppaftp::list::PosixPexQuery::{Group, Others, Owner};
            let bits = |who| {
                (u32::from(file.can_read(who)) << 2)
                    | (u32::from(file.can_write(who)) << 1)
                    | u32::from(file.can_execute(who))
            };
            let mode = (bits(Owner) << 6) | (bits(Group) << 3) | bits(Others);
            (mode != 0).then_some(mode)
        };
        let (owner, group) = match names {
            Some((owner, group)) => (Some(owner), Some(group)),
            None => (file.uid().map(|u| u.to_string()), file.gid().map(|g| g.to_string())),
        };
        Entry {
            hidden: name.starts_with('.'),
            size: if kind == EntryKind::File { file.size() as u64 } else { 0 },
            modified: Some(file.modified()),
            name,
            path,
            kind,
            package: false,
            permissions,
            owner,
            group,
        }
    }
}

/// Reads a download; the transfer completes when it is dropped.
struct Download(suppaftp::TransferStream<TlsStream>);

impl Read for Download {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

/// Writes an upload; flush completes it (and reports the server's answer).
struct Upload(Option<suppaftp::TransferStream<TlsStream>>);

impl Write for Upload {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.0.as_mut() {
            Some(stream) => stream.write(buf),
            None => Err(io::Error::other("the upload is finished")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.0.take() {
            Some(stream) => stream.finish().map_err(to_io),
            None => Ok(()),
        }
    }
}

impl Vfs for FtpFs {
    fn key(&self) -> String {
        let scheme = if self.secure { "ftps" } else { "ftp" };
        format!("{scheme}://{}@{}:{}", self.params.user, self.params.host, self.params.port)
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
        self.with(|ftp| {
            let items = Self::read_dir(ftp, dir)?;
            Ok(items
                .into_iter()
                .filter(|(f, _)| f.name() != "." && f.name() != "..")
                .map(|(f, names)| self.entry(ftp, dir, &f, names))
                .collect())
        })
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        let Some(parent) = path.parent() else {
            return Ok(Entry {
                name: "/".into(),
                path: path.to_path_buf(),
                kind: EntryKind::Folder,
                size: 0,
                modified: None,
                hidden: false,
                package: false,
                permissions: None,
                owner: None,
                group: None,
            });
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.list(parent)?
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such file or folder."))
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.with(|ftp| ftp.mkdir(Self::path_text(path)))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.with(|ftp| ftp.rename(Self::path_text(from), Self::path_text(to)))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.with(|ftp| ftp.rm(Self::path_text(path)))
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.with(|ftp| ftp.rmdir(Self::path_text(path)))
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let stream = self.with(|ftp| ftp.retr_as_stream(Self::path_text(path)))?;
        Ok(Box::new(Download(stream)))
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let stream = self.with(|ftp| {
            ftp.resume_transfer(offset as usize)?;
            ftp.retr_as_stream(Self::path_text(path))
        })?;
        Ok(Box::new(Download(stream)))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        let stream = self.with(|ftp| ftp.put_with_stream(Self::path_text(path)))?;
        Ok(Box::new(Upload(Some(stream))))
    }

    fn open_write_from(&self, path: &Path, _offset: u64) -> io::Result<Box<dyn Write + Send>> {
        // The partial file ends at the offset: append to it.
        let stream = self.with(|ftp| ftp.append_with_stream(Self::path_text(path)))?;
        Ok(Box::new(Upload(Some(stream))))
    }

    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, permissions: Option<u32>) -> io::Result<()> {
        let text = Self::path_text(path);
        // Both are extensions; servers without them leave things as they are.
        if let Some(mode) = permissions {
            let _ = self.with(|ftp| {
                ftp.custom_command(format!("SITE CHMOD {:o} {text}", mode & 0o7777), &[suppaftp::Status::CommandOk])
            });
        }
        if let Some(modified) = modified {
            let stamp = super::http::utc_stamp(modified).1.replace(['T', 'Z'], "");
            let _ = self.with(|ftp| ftp.custom_command(format!("MFMT {stamp} {text}"), &[suppaftp::Status::File]));
        }
        Ok(())
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        let mut secret = self.secret.clone();
        let (conn, secure) = open(&self.params, None, &mut secret)?;
        Ok(Arc::new(FtpFs {
            params: self.params.clone(),
            conn: Mutex::new(Some(conn)),
            home: self.home.clone(),
            secret,
            secure,
        }))
    }

    fn fxp_endpoint(&self) -> Option<super::fxp::Endpoint> {
        Some(super::fxp::Endpoint { params: self.params.clone(), password: self.secret.clone(), secure: self.secure })
    }

    fn display_path(&self, path: &Path) -> String {
        let scheme = if self.secure { "ftps" } else { "ftp" };
        let port = if self.params.port == 21 { String::new() } else { format!(":{}", self.params.port) };
        format!("{scheme}://{}@{}{port}{}", self.params.user, self.params.host, path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints() {
        assert_eq!(
            fingerprint(b"abc"),
            "BA:78:16:BF:8F:01:CF:EA:41:41:40:DE:5D:AE:22:23:B0:03:61:A3:96:17:7A:9C:B4:10:FF:61:F2:00:15:AD"
        );
    }
}
