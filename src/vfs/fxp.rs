//! FXP: copying a file straight from one FTP server to another. The target
//! server is told to wait for a data connection (PASV) and the source server
//! to make it (PORT); then one sends (RETR) and the other receives (STOR),
//! and the file never passes through the Mac.
//!
//! Many servers refuse this (a PORT to an address other than the client's is
//! how "FTP bounce" attacks work), so it is tried once per pair of servers;
//! when refused, copies stream through the Mac as usual. Encrypted
//! connections need one of the servers to support SSCN (it then acts as the
//! TLS client on the data connection). With only one end encrypted, FXP is
//! not used, as the file would travel unencrypted.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::ftp::FtpParams;
use super::known_hosts;

/// How to reach and log in to one server.
#[derive(Clone)]
pub struct Endpoint {
    pub params: FtpParams,
    pub password: Option<String>,
    /// forkz's connection to it is encrypted (FXP's must be too).
    pub secure: bool,
}

impl Endpoint {
    /// The server and how forkz reaches it (FXP may work without TLS but
    /// not with it).
    fn key(&self) -> String {
        let scheme = if self.secure { "ftps" } else { "ftp" };
        format!("{scheme}://{}@{}:{}", self.params.user, self.params.host, self.params.port)
    }
}

/// Why a file was not copied by FXP.
#[derive(Debug)]
pub enum Error {
    /// These servers cannot do FXP with each other (remembered for the
    /// session): copy through the Mac.
    Unsupported(String),
    /// FXP could not be set up this time (a connection or login problem).
    Unavailable(String),
    /// The transfer started but did not complete.
    Failed(io::Error),
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported(why) | Error::Unavailable(why) => f.write_str(why),
            Error::Failed(e) => write!(f, "{e}"),
            Error::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Failed(e)
    }
}

/// Server pairs (source, target) that cannot do FXP, this session.
static REFUSED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

fn refused(src: &Endpoint, dst: &Endpoint) -> bool {
    let pair = (src.key(), dst.key());
    REFUSED.lock().is_ok_and(|list| list.contains(&pair))
}

fn remember_refusal(src: &Endpoint, dst: &Endpoint) {
    if let Ok(mut list) = REFUSED.lock() {
        list.push((src.key(), dst.key()));
    }
}

/// How long a reply may take while no transfer is running.
const WAIT: Duration = Duration::from_secs(60);

enum Channel {
    Plain(TcpStream),
    Tls(Box<native_tls::TlsStream<TcpStream>>),
}

impl Channel {
    fn socket(&self) -> &TcpStream {
        match self {
            Channel::Plain(s) => s,
            Channel::Tls(s) => s.get_ref(),
        }
    }
}

impl Read for Channel {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Channel::Plain(s) => s.read(buf),
            Channel::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Channel {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Channel::Plain(s) => s.write(buf),
            Channel::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Channel::Plain(s) => s.flush(),
            Channel::Tls(s) => s.flush(),
        }
    }
}

/// A control connection, logged in, in binary mode.
struct Control {
    io: BufReader<Channel>,
    /// The start of a reply line that has not fully arrived.
    partial: Vec<u8>,
    /// The server's address (for PASV answers with a private address).
    peer: IpAddr,
    features: Vec<String>,
}

fn unavailable(host: &str) -> impl Fn(io::Error) -> Error + '_ {
    move |e| Error::Unavailable(format!("{host}: {e}"))
}

impl Control {
    fn open(end: &Endpoint) -> Result<Control, Error> {
        let p = &end.params;
        let fail = unavailable(&p.host);
        let address = (p.host.as_str(), p.port)
            .to_socket_addrs()
            .map_err(&fail)?
            .next()
            .ok_or_else(|| Error::Unavailable(format!("{} could not be found", p.host)))?;
        let tcp = TcpStream::connect_timeout(&address, p.timeout).map_err(&fail)?;
        tcp.set_read_timeout(Some(WAIT)).map_err(&fail)?;
        let mut control = Control {
            io: BufReader::new(Channel::Plain(tcp)),
            partial: Vec::new(),
            peer: address.ip(),
            features: Vec::new(),
        };
        control.expect("", &[220]).map_err(|e| Error::Unavailable(e.to_string()))?;
        if end.secure {
            control.expect("AUTH TLS", &[234]).map_err(|e| Error::Unavailable(e.to_string()))?;
            control = control.into_tls(p)?;
        }
        control.log_in(end).map_err(|e| Error::Unavailable(e.to_string()))?;
        Ok(control)
    }

    /// TLS on the control connection, trusting what forkz trusts: the
    /// system's certificates, or the one the user accepted for this server.
    fn into_tls(self, p: &FtpParams) -> Result<Control, Error> {
        let Channel::Plain(tcp) = self.io.into_inner() else { unreachable!("TLS is set up once") };
        let remembered = known_hosts::trusted_certificate(&p.host, p.port);
        let connector = native_tls::TlsConnector::builder()
            .danger_accept_invalid_certs(remembered.is_some())
            .danger_accept_invalid_hostnames(remembered.is_some())
            .build()
            .map_err(|e| Error::Unavailable(e.to_string()))?;
        let tls = connector.connect(&p.host, tcp).map_err(|e| Error::Unavailable(format!("{}: TLS: {e}", p.host)))?;
        if let Some(expected) = remembered {
            let seen = tls
                .peer_certificate()
                .ok()
                .flatten()
                .and_then(|c| c.to_der().ok())
                .map(|der| super::ftp::fingerprint(&der));
            if seen.as_ref() != Some(&expected) {
                return Err(Error::Unavailable(format!("{}: the certificate changed", p.host)));
            }
        }
        Ok(Control { io: BufReader::new(Channel::Tls(Box::new(tls))), partial: Vec::new(), ..self })
    }

    fn log_in(&mut self, end: &Endpoint) -> io::Result<()> {
        let (code, text) = self.command(&format!("USER {}", end.params.user))?;
        match code {
            230 => {}
            331 => {
                self.expect(&format!("PASS {}", end.password.as_deref().unwrap_or("")), &[230, 202])?;
            }
            _ => return Err(io::Error::other(text)),
        }
        if end.secure {
            self.expect("PBSZ 0", &[200])?;
            self.expect("PROT P", &[200])?;
        }
        self.expect("TYPE I", &[200])?;
        let (code, text) = self.command("FEAT")?;
        if code == 211 {
            self.features = text.lines().skip(1).map(|l| l.trim().to_uppercase()).collect();
        }
        Ok(())
    }

    fn has_feature(&self, name: &str) -> bool {
        self.features.iter().any(|f| f.split_whitespace().next() == Some(name))
    }

    fn send(&mut self, line: &str) -> io::Result<()> {
        if line.contains(['\r', '\n']) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a name has a line break"));
        }
        let channel = self.io.get_mut();
        channel.write_all(format!("{line}\r\n").as_bytes())?;
        channel.flush()
    }

    /// Sends a command (none if empty: just reads the next reply) and
    /// returns the reply's code and text.
    fn command(&mut self, line: &str) -> io::Result<(u16, String)> {
        if !line.is_empty() {
            self.send(line)?;
        }
        self.reply()
    }

    /// A command whose reply must have one of these codes.
    fn expect(&mut self, line: &str, codes: &[u16]) -> io::Result<String> {
        let (code, text) = self.command(line)?;
        if codes.contains(&code) { Ok(text) } else { Err(io::Error::other(text)) }
    }

    /// One reply, all lines of it.
    fn reply(&mut self) -> io::Result<(u16, String)> {
        let first = self.line()?;
        let code: u16 = first
            .get(..3)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("unexpected reply: {first}")))?;
        let mut text = first.clone();
        if first.as_bytes().get(3) == Some(&b'-') {
            // Multi-line: until a line starting with the code and a space.
            loop {
                let line = self.line()?;
                text.push('\n');
                text.push_str(&line);
                if line.get(..3) == first.get(..3) && line.as_bytes().get(3) != Some(&b'-') {
                    break;
                }
            }
        }
        Ok((code, text))
    }

    fn line(&mut self) -> io::Result<String> {
        loop {
            let n = self.io.read_until(b'\n', &mut self.partial)?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed the connection"));
            }
            if self.partial.ends_with(b"\n") {
                let line = String::from_utf8_lossy(&self.partial).trim_end().to_string();
                self.partial.clear();
                return Ok(line);
            }
        }
    }

    /// Whether a reply has started to arrive, waiting up to `wait`.
    fn ready(&mut self, wait: Duration) -> io::Result<bool> {
        if !self.io.buffer().is_empty() || !self.partial.is_empty() {
            return Ok(true);
        }
        let socket = self.io.get_ref().socket();
        socket.set_read_timeout(Some(wait))?;
        let result = socket.peek(&mut [0u8; 1]);
        socket.set_read_timeout(Some(WAIT))?;
        match result {
            // Data, or closed (which reading the reply reports).
            Ok(_) => Ok(true),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Stops a transfer, reading what the server answers (best effort).
    fn abort(&mut self) {
        if self.send("ABOR").is_ok() {
            for _ in 0..2 {
                if !self.ready(Duration::from_secs(3)).unwrap_or(false) || self.reply().is_err() {
                    break;
                }
            }
        }
    }
}

/// Control connections to two servers that agreed to FXP with each other.
pub struct Pair {
    src: Control,
    dst: Control,
}

impl Pair {
    /// Connects to both servers and checks what FXP needs. Pairs that
    /// cannot do it are remembered, and not tried again this session.
    pub fn connect(src: &Endpoint, dst: &Endpoint) -> Result<Pair, Error> {
        if refused(src, dst) {
            return Err(Error::Unsupported("these servers refused FXP before".into()));
        }
        let result = Self::check(src, dst);
        if let Err(Error::Unsupported(_)) = &result {
            remember_refusal(src, dst);
        }
        result
    }

    fn check(src: &Endpoint, dst: &Endpoint) -> Result<Pair, Error> {
        if src.secure != dst.secure {
            return Err(Error::Unsupported("only one of the servers is encrypted".into()));
        }
        let mut pair = Pair { src: Control::open(src)?, dst: Control::open(dst)? };
        if src.secure {
            // One server makes the data connection's TLS as the client.
            let side = if pair.src.has_feature("SSCN") {
                &mut pair.src
            } else if pair.dst.has_feature("SSCN") {
                &mut pair.dst
            } else {
                return Err(Error::Unsupported("neither server supports SSCN, which encrypted FXP needs".into()));
            };
            side.expect("SSCN ON", &[200]).map_err(|e| Error::Unsupported(format!("SSCN: {e}")))?;
        }
        Ok(pair)
    }

    /// Copies `from` (on the source) to `to` (on the target), appending
    /// from `offset` if it is not 0 (and the source can resume). `tick` is
    /// called once both servers have started, then about once a second.
    pub fn copy(
        &mut self,
        from: &Path,
        to: &Path,
        offset: u64,
        cancelled: &dyn Fn() -> bool,
        tick: &mut dyn FnMut(),
    ) -> Result<(), Error> {
        // The target listens...
        let (code, text) = self.dst.command("PASV")?;
        if code != 227 {
            return Err(Error::Unsupported(format!("the target server refused PASV: {text}")));
        }
        let (ip, port) =
            parse_pasv(&text).ok_or_else(|| Error::Unsupported(format!("unexpected PASV reply: {text}")))?;
        // A private address from a server reached at a public one is behind
        // NAT: its public address is the one to use.
        let ip = match self.dst.peer {
            IpAddr::V4(peer) if is_private(ip) && !is_private(peer) => peer,
            IpAddr::V4(_) => ip,
            IpAddr::V6(_) => return Err(Error::Unsupported("FXP over IPv6 is not supported".into())),
        };
        // ...and the source connects to it.
        let (code, text) = self.src.command(&format!("PORT {}", port_argument(ip, port)))?;
        if code != 200 {
            return Err(Error::Unsupported(format!("the source server refused PORT: {text}")));
        }
        let mut offset = offset;
        if offset > 0 && self.src.command(&format!("REST {offset}"))?.0 != 350 {
            offset = 0;
        }
        let store = if offset > 0 { "APPE" } else { "STOR" };
        self.dst.send(&format!("{store} {}", to.to_string_lossy()))?;
        self.src.send(&format!("RETR {}", from.to_string_lossy()))?;
        let (code, text) = self.src.reply()?;
        if !matches!(code, 125 | 150) {
            // The target waits for a connection that will not come.
            self.dst.abort();
            return Err(Error::Failed(io::Error::other(text)));
        }
        let (code, text) = self.dst.reply()?;
        if !matches!(code, 125 | 150) {
            self.src.abort();
            return Err(Error::Failed(io::Error::other(text)));
        }
        tick();
        let mut src_done = None;
        let mut dst_done = None;
        let mut last_tick = Instant::now();
        while src_done.is_none() || dst_done.is_none() {
            if cancelled() {
                self.src.abort();
                self.dst.abort();
                return Err(Error::Cancelled);
            }
            if src_done.is_none() && self.src.ready(Duration::from_millis(150))? {
                src_done = Some(self.src.reply()?);
            }
            if dst_done.is_none() && self.dst.ready(Duration::from_millis(150))? {
                dst_done = Some(self.dst.reply()?);
            }
            if last_tick.elapsed() >= Duration::from_secs(1) {
                tick();
                last_tick = Instant::now();
            }
        }
        for (code, text) in [src_done, dst_done].into_iter().flatten() {
            if !matches!(code, 226 | 250) {
                return Err(Error::Failed(io::Error::other(text)));
            }
        }
        Ok(())
    }
}

/// The address and port in "227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)".
fn parse_pasv(text: &str) -> Option<(Ipv4Addr, u16)> {
    let rest = text.get(3..)?;
    rest.split(|c: char| !c.is_ascii_digit() && c != ',').find_map(|chunk| {
        let numbers: Vec<u8> = chunk.split(',').map(|n| n.parse().ok()).collect::<Option<_>>()?;
        match numbers[..] {
            [a, b, c, d, high, low] => Some((Ipv4Addr::new(a, b, c, d), u16::from(high) << 8 | u16::from(low))),
            _ => None,
        }
    })
}

fn port_argument(ip: Ipv4Addr, port: u16) -> String {
    let [a, b, c, d] = ip.octets();
    format!("{a},{b},{c},{d},{},{}", port >> 8, port & 0xff)
}

fn is_private(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasv_replies() {
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        assert_eq!(parse_pasv("227 Entering Passive Mode (192,168,1,20,117,48)."), Some((ip, 30000)));
        assert_eq!(parse_pasv("227 Entering Passive Mode 192,168,1,20,117,48"), Some((ip, 30000)));
        assert_eq!(parse_pasv("227 =192,168,1,20,4,1"), Some((ip, 1025)));
        assert_eq!(parse_pasv("227 Entering Passive Mode"), None);
        assert_eq!(parse_pasv("227 (300,1,1,1,1,1)"), None);
        assert_eq!(port_argument(ip, 30000), "192,168,1,20,117,48");
        assert!(is_private(Ipv4Addr::new(10, 0, 0, 1)) && !is_private(Ipv4Addr::new(93, 184, 216, 34)));
    }

    #[test]
    fn refusals_are_remembered_per_direction() {
        let end = |host: &str| Endpoint {
            params: FtpParams {
                host: host.into(),
                port: 21,
                user: "u".into(),
                password: None,
                security: super::super::ftp::Security::Plain,
                timeout: Duration::from_secs(1),
            },
            password: None,
            secure: false,
        };
        let (a, b) = (end("fxp-a.invalid"), end("fxp-b.invalid"));
        assert!(!refused(&a, &b));
        remember_refusal(&a, &b);
        assert!(refused(&a, &b) && !refused(&b, &a));
        // Mixed encryption is refused without connecting.
        let secure = Endpoint { secure: true, ..end("fxp-c.invalid") };
        assert!(matches!(Pair::connect(&a, &secure), Err(Error::Unsupported(_))));
        assert!(refused(&a, &secure));
    }
}
