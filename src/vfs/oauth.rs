//! Signing in to cloud drives in the browser (OAuth): forkz opens the
//! service's sign-in page, the service sends the browser back to forkz on
//! 127.0.0.1 with a code, and forkz trades the code for tokens. PKCE (a
//! secret made for each sign-in) proves the code came back to the forkz
//! that asked for it.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use super::http;

/// How long to wait for signing in in the browser.
pub const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

/// Random hex (from /dev/urandom).
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buffer)).is_err() {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        buffer = Sha256::digest(nanos.to_le_bytes()).to_vec();
    }
    buffer.iter().map(|b| format!("{b:02x}")).collect()
}

/// A PKCE pair: the verifier (kept) and its challenge (sent ahead).
pub fn pkce() -> (String, String) {
    let verifier = random_hex(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// What a sign-in in the browser brought back.
pub struct Code {
    pub code: String,
    /// The address the browser was sent back to (the token request repeats
    /// it).
    pub redirect: String,
    pub verifier: String,
}

/// Opens the sign-in page (`page(redirect, state, challenge)`) and waits
/// for the browser to come back on `port` (0: any free port, for services
/// that allow that). `service` names it in messages ("Dropbox").
pub fn sign_in(service: &str, port: u16, page: impl Fn(&str, &str, &str) -> String) -> io::Result<Code> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| io::Error::new(e.kind(), format!("forkz could not wait for {service} on port {port} ({e}).")))?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://127.0.0.1:{port}");
    let state = random_hex(16);
    let (verifier, challenge) = pkce();
    let mut open = std::process::Command::new("/usr/bin/open");
    // Debug builds: FORKZ_TEST_BROWSER=Safari signs in in another browser
    // than the default (another account, for testing).
    if cfg!(debug_assertions)
        && let Some(browser) = std::env::var_os("FORKZ_TEST_BROWSER")
    {
        open.arg("-a").arg(browser);
    }
    let opened = open.arg(page(&redirect, &state, &challenge)).status()?;
    if !opened.success() {
        return Err(io::Error::other(format!("The browser could not be opened to sign in to {service}.")));
    }
    let code = wait_for_code(service, &listener, &state, SIGN_IN_WAIT)?;
    Ok(Code { code, redirect, verifier })
}

/// The query of a request line ("GET /?code=x&state=y HTTP/1.1").
pub fn callback_query(request_line: &str) -> HashMap<String, String> {
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let query = target.split_once('?').map_or("", |(_, q)| q);
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (http::decode(k), http::decode(&v.replace('+', " "))))
        .collect()
}

fn reply_page(stream: &mut TcpStream, status: &str, text: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>forkz</title><body style=\"font: 16px -apple-system, sans-serif; text-align: center; margin-top: 20vh\"><h2>{text}</h2>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

/// The query of a request to the redirect. All of the request is read
/// (closing with some of it unread would reset the connection, and the
/// browser would not show the page), but only its first line matters.
fn read_query(stream: &mut TcpStream) -> io::Result<HashMap<String, String>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < 16_384 && !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
    }
    let text = String::from_utf8_lossy(&head);
    Ok(callback_query(text.lines().next().unwrap_or("")))
}

/// After the code came: the browser may load the redirect again (Safari
/// can load it twice), so the same page is shown for a while instead of
/// "cannot connect".
fn keep_answering(listener: &TcpListener, state: &str, page: String) {
    let Ok(listener) = listener.try_clone() else { return };
    let state = state.to_string();
    std::thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let Ok(query) = read_query(&mut stream) else { continue };
                    if query.get("state") == Some(&state) {
                        reply_page(&mut stream, "200 OK", &page);
                    } else {
                        reply_page(&mut stream, "404 Not Found", "Not found");
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return,
            }
        }
    });
}

/// Waits for the browser to come back with the authorization code.
pub fn wait_for_code(service: &str, listener: &TcpListener, state: &str, wait: Duration) -> io::Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e),
        };
        let query = read_query(&mut stream)?;
        if query.get("state").map(String::as_str) != Some(state) {
            // Something else (the browser asking for an icon).
            reply_page(&mut stream, "404 Not Found", "Not found");
            continue;
        }
        if let Some(code) = query.get("code") {
            let signed_in = format!("Signed in to {service}. You can close this tab and go back to forkz.");
            reply_page(&mut stream, "200 OK", &signed_in);
            keep_answering(listener, state, signed_in);
            return Ok(code.clone());
        }
        let error = query.get("error").cloned().unwrap_or_default();
        reply_page(&mut stream, "200 OK", &format!("forkz was not allowed to use {service}. You can close this tab."));
        return Err(if error == "access_denied" {
            io::Error::new(io::ErrorKind::Interrupted, "Cancelled")
        } else {
            io::Error::new(io::ErrorKind::PermissionDenied, format!("{service} refused the sign-in ({error})."))
        });
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("Signing in to {service} took too long. Try connecting again."),
    ))
}

/// A token request's form, answered with JSON (access_token, maybe
/// refresh_token and expires_in). A refusal is PermissionDenied (sign in
/// again).
pub fn token_request(
    agent: &http::Agent,
    service: &str,
    url: &str,
    form: &[(&str, &str)],
) -> io::Result<serde_json::Value> {
    let response = agent.post(url).send_form(form.to_vec()).map_err(http::to_io)?;
    let status = response.status().as_u16();
    let body = response.into_body().read_to_string().map_err(http::to_io)?;
    let reply: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    if status != 200 {
        let detail = reply["error_description"].as_str().or(reply["error"].as_str()).unwrap_or(body.trim()).to_string();
        let kind = if matches!(status, 400 | 401) { io::ErrorKind::PermissionDenied } else { io::ErrorKind::Other };
        return Err(io::Error::new(kind, format!("{service} did not accept the sign-in ({detail}).")));
    }
    Ok(reply)
}

/// A built-in or environment setting (the services' app IDs): built into
/// releases, or from the environment for builds of your own.
pub fn app_setting(name: &str, built: Option<&'static str>) -> Option<String> {
    std::env::var(name).ok().or(built.map(str::to_string)).filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_pkce_pairs() {
        let (verifier, challenge) = pkce();
        assert_eq!(verifier.len(), 64);
        assert_eq!(challenge, URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())));
        assert_ne!(pkce().0, verifier);
        // RFC 7636's example.
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn the_redirect_answers_again_after_the_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let get = move |query: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stream, "GET /?{query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        };
        let browser = std::thread::spawn(move || {
            let icon = get("x=1");
            let first = get("code=abc&state=s1");
            // Loaded again (Safari can): the same page, not "cannot connect".
            let again = get("code=abc&state=s1");
            (icon, first, again)
        });
        let code = wait_for_code("Test", &listener, "s1", Duration::from_secs(10)).unwrap();
        assert_eq!(code, "abc");
        let (icon, first, again) = browser.join().unwrap();
        assert!(icon.starts_with("HTTP/1.1 404"), "{icon}");
        assert!(first.contains("Signed in to Test"), "{first}");
        assert!(again.starts_with("HTTP/1.1 200") && again.contains("Signed in to Test"), "{again}");
    }
}
