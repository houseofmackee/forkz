//! What the HTTP-based file systems (WebDAV, S3) share: the client (with
//! macOS's TLS), URL path encoding, UTC timestamps, and an upload pipe that
//! streams a file to the server as it is written, so progress is real.

use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

pub use ureq::Agent;

/// A client that reports HTTP errors as responses (to read their status),
/// allows WebDAV methods, and uses macOS's TLS and trust settings.
pub fn agent(timeout: Duration) -> Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    // The system's roots and trust settings (Keychain), not a bundled list.
    let tls = TlsConfig::builder().provider(TlsProvider::NativeTls).root_certs(RootCerts::PlatformVerifier).build();
    let config = Agent::config_builder()
        .http_status_as_error(false)
        .allow_non_standard_methods(true)
        .timeout_connect(Some(timeout))
        .tls_config(tls)
        .build();
    Agent::new_with_config(config)
}

/// Characters left as they are in a path segment (RFC 3986 unreserved).
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// "/a b/c" -> "/a%20b/c": each segment encoded, slashes kept.
pub fn encode_path(path: &str) -> String {
    path.split('/').map(|segment| utf8_percent_encode(segment, SEGMENT).to_string()).collect::<Vec<_>>().join("/")
}

/// A query value, fully encoded (as S3 signing requires).
pub fn encode_value(value: &str) -> String {
    utf8_percent_encode(value, SEGMENT).to_string()
}

pub fn decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text).decode_utf8_lossy().into_owned()
}

/// (YYYYMMDD, YYYYMMDDTHHMMSSZ) in UTC.
pub fn utc_stamp(time: SystemTime) -> (String, String) {
    let seconds = time.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let date = format!("{year:04}{month:02}{day:02}");
    let time = format!("{date}T{:02}{:02}{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60);
    (date, time)
}

/// The other end of an upload pipe: what the request body reads.
pub struct PipeReader {
    chunks: Receiver<Vec<u8>>,
    current: Vec<u8>,
    position: usize,
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.position >= self.current.len() {
            match self.chunks.recv() {
                Ok(chunk) => {
                    self.current = chunk;
                    self.position = 0;
                }
                // The writer is done.
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.current.len() - self.position);
        buf[..n].copy_from_slice(&self.current[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }
}

/// Writes go to a request running on its own thread; flush ends the body
/// and returns the request's result.
pub struct PipeWriter {
    chunks: Option<SyncSender<Vec<u8>>>,
    request: Option<JoinHandle<io::Result<()>>>,
}

/// Starts `send` (which reads the body from the reader it is given) on a
/// thread, and returns the writer that feeds it.
pub fn upload_pipe(send: impl FnOnce(PipeReader) -> io::Result<()> + Send + 'static) -> PipeWriter {
    // A few chunks in flight: writes wait for the network, so progress
    // follows what was actually sent.
    let (sender, receiver) = sync_channel(4);
    let reader = PipeReader { chunks: receiver, current: Vec::new(), position: 0 };
    let request = std::thread::spawn(move || send(reader));
    PipeWriter { chunks: Some(sender), request: Some(request) }
}

impl PipeWriter {
    fn finish(&mut self) -> io::Result<()> {
        self.chunks.take();
        match self.request.take() {
            Some(request) => request.join().unwrap_or_else(|_| Err(io::Error::other("the upload stopped"))),
            None => Ok(()),
        }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(chunks) = &self.chunks else { return Err(io::Error::other("the upload is finished")) };
        if chunks.send(buf.to_vec()).is_err() {
            // The request ended early: its error says why.
            return Err(self.finish().err().unwrap_or_else(|| io::Error::other("the server closed the upload")));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.finish()
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// An error for an HTTP status, with what the server said.
pub fn status_error(status: u16, body: &str) -> io::Error {
    let kind = match status {
        401 | 403 => io::ErrorKind::PermissionDenied,
        404 => io::ErrorKind::NotFound,
        409 | 412 => io::ErrorKind::AlreadyExists,
        _ => io::ErrorKind::Other,
    };
    let detail = body.trim();
    let message = match status {
        401 => "The user name or password was not accepted.".to_string(),
        403 => "Access denied.".to_string(),
        404 => "No such file or folder.".to_string(),
        _ if detail.is_empty() => format!("The server answered {status}."),
        _ => format!("The server answered {status}: {}", detail.chars().take(300).collect::<String>()),
    };
    io::Error::new(kind, message)
}

pub fn to_io(error: ureq::Error) -> io::Error {
    match error {
        ureq::Error::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_stamps() {
        assert_eq!(encode_path("/my folder/ä+b.txt"), "/my%20folder/%C3%A4%2Bb.txt");
        assert_eq!(decode("/my%20folder/%C3%A4%2Bb.txt"), "/my folder/ä+b.txt");
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_440_938_160);
        assert_eq!(utc_stamp(time), ("20150830".into(), "20150830T123600Z".into()));
    }

    /// The system's trust store verifies a public server (needs the
    /// network: run with --ignored).
    #[test]
    #[ignore = "needs the network"]
    fn system_trust_verifies_a_public_server() {
        let response = agent(Duration::from_secs(10)).get("https://s3.amazonaws.com/").call().unwrap();
        // Whatever it answers, it answered over a verified connection.
        assert!(response.status().as_u16() < 500, "{}", response.status());
        let error = agent(Duration::from_secs(10)).get("https://self-signed.badssl.com/").call().unwrap_err();
        assert!(error.to_string().to_lowercase().contains("cert") || error.to_string().contains("trust"), "{error}");
    }

    #[test]
    fn pipe_streams_and_reports_the_request_result() {
        let mut writer = upload_pipe(|mut reader| {
            let mut all = Vec::new();
            reader.read_to_end(&mut all)?;
            if all == b"hello world" { Ok(()) } else { Err(io::Error::other("wrong body")) }
        });
        writer.write_all(b"hello ").unwrap();
        writer.write_all(b"world").unwrap();
        writer.flush().unwrap();

        let mut failing = upload_pipe(|_reader| Err(io::Error::other("refused")));
        let error = (0..100).find_map(|_| failing.write_all(&[0u8; 1000]).err()).or_else(|| failing.flush().err());
        assert_eq!(error.unwrap().to_string(), "refused");
    }
}
