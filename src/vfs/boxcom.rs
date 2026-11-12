//! Box (box.com), through its API. Signing in happens in the browser
//! (OAuth): Box sends the browser back to forkz on 127.0.0.1, and forkz
//! keeps the sign-in (a refresh token, which Box replaces each time it is
//! used) in the Keychain. Box finds items by ID, not by path, so forkz
//! remembers the IDs of the paths it has listed.

use std::collections::HashMap;
use std::io::{self, Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use super::http::{self, Agent};
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

const AUTHORIZE: &str = "https://account.box.com/api/oauth2/authorize";

/// Where Box sends the browser back after signing in (as registered with
/// forkz's Box app).
pub const REDIRECT_PORT: u16 = 53_791;

/// How long to wait for signing in in the browser.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

const FIELDS: &str = "id,type,name,size,modified_at,content_modified_at";

/// Saves the sign-in that replaces the last one.
pub type SaveToken = Arc<dyn Fn(&str) + Send + Sync>;

pub fn redirect_uri() -> String {
    format!("http://127.0.0.1:{REDIRECT_PORT}")
}

/// forkz's app at Box (client ID and secret): built into releases, or
/// from the environment (for builds of your own).
fn client() -> io::Result<(String, String)> {
    fn get(name: &str, built: Option<&'static str>) -> Option<String> {
        std::env::var(name).ok().or(built.map(str::to_string)).filter(|v| !v.trim().is_empty())
    }
    match (
        get("FORKZ_BOX_CLIENT_ID", option_env!("FORKZ_BOX_CLIENT_ID")),
        get("FORKZ_BOX_CLIENT_SECRET", option_env!("FORKZ_BOX_CLIENT_SECRET")),
    ) {
        (Some(id), Some(secret)) => Ok((id, secret)),
        _ => Err(io::Error::other(
            "This copy of forkz cannot sign in to Box: it was built without a Box app (FORKZ_BOX_CLIENT_ID and FORKZ_BOX_CLIENT_SECRET).",
        )),
    }
}

/// Where Box is (a stand-in server for tests).
#[derive(Clone)]
pub struct Service {
    pub api: String,
    pub upload: String,
    pub token: String,
    /// Larger files are uploaded in parts (Box's limit for one request).
    pub simple_upload: u64,
    /// The app's client ID and secret (none: forkz's own).
    pub client: Option<(String, String)>,
}

impl Default for Service {
    fn default() -> Self {
        Service {
            api: "https://api.box.com/2.0".into(),
            upload: "https://upload.box.com/api/2.0".into(),
            token: "https://api.box.com/oauth2/token".into(),
            simple_upload: 50 * 1024 * 1024,
            client: None,
        }
    }
}

#[derive(Clone)]
pub struct BoxParams {
    /// The last sign-in (none: sign in in the browser).
    pub refresh_token: Option<String>,
    /// Keeps each new sign-in (for a saved site).
    pub save_token: Option<SaveToken>,
    pub timeout: Duration,
    pub service: Service,
    /// Sign in in the browser when there is no sign-in or it expired (off
    /// in tests).
    pub browser: bool,
}

struct Tokens {
    access: String,
    refresh: String,
    /// When to renew the access token (a minute early).
    renew_at: Instant,
}

/// What all connections to the account share.
struct Shared {
    service: Service,
    client: (String, String),
    tokens: Mutex<Tokens>,
    save_token: Option<SaveToken>,
    /// Paths seen, with their IDs and whether they are folders.
    ids: Mutex<HashMap<PathBuf, (String, bool)>>,
}

#[derive(Clone)]
pub struct BoxFs {
    shared: Arc<Shared>,
    agent: Agent,
    /// For downloads (Box redirects them; the Range header must go along).
    direct: Agent,
    login: String,
    user_id: String,
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    name: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    modified_at: Option<String>,
    #[serde(default)]
    content_modified_at: Option<String>,
}

#[derive(Deserialize)]
struct Items {
    entries: Vec<Item>,
    #[serde(default)]
    next_marker: Option<String>,
}

/// Connects: renews the saved sign-in, or signs in in the browser.
pub fn connect(params: BoxParams) -> io::Result<Arc<BoxFs>> {
    let client = match params.service.client.clone() {
        Some(client) => client,
        None => client()?,
    };
    let token_url = params.service.token.clone();
    let agent = http::agent(params.timeout);
    let saved = params.refresh_token.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let tokens = match saved {
        Some(refresh) => {
            match token_request(
                &agent,
                &token_url,
                &client,
                &[("grant_type", "refresh_token"), ("refresh_token", refresh)],
            ) {
                Ok(tokens) => tokens,
                // Expired (after 60 days unused) or signed out at Box.
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied && params.browser => {
                    sign_in(&agent, &token_url, &client)?
                }
                Err(e) => return Err(e),
            }
        }
        None if params.browser => sign_in(&agent, &token_url, &client)?,
        None => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Not signed in to Box.")),
    };
    if let Some(save) = &params.save_token {
        save(&tokens.refresh);
    }
    let shared = Arc::new(Shared {
        service: params.service,
        client,
        tokens: Mutex::new(tokens),
        save_token: params.save_token,
        ids: Mutex::new(HashMap::new()),
    });
    let mut fs = BoxFs {
        shared,
        agent,
        direct: http::agent_without_redirects(params.timeout),
        login: String::new(),
        user_id: String::new(),
    };
    let me = fs.json("GET", &format!("{}/users/me?fields=id,login", fs.shared.service.api), None)?;
    fs.login = me["login"].as_str().unwrap_or_default().to_string();
    fs.user_id = me["id"].as_str().unwrap_or_default().to_string();
    Ok(Arc::new(fs))
}

fn token_request(agent: &Agent, url: &str, client: &(String, String), grant: &[(&str, &str)]) -> io::Result<Tokens> {
    let mut form: Vec<(&str, &str)> = grant.to_vec();
    form.push(("client_id", &client.0));
    form.push(("client_secret", &client.1));
    let response = agent.post(url).send_form(form).map_err(http::to_io)?;
    let status = response.status().as_u16();
    let body = response.into_body().read_to_string().map_err(http::to_io)?;
    if status != 200 {
        let reply: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let detail = reply["error_description"].as_str().unwrap_or(body.trim()).to_string();
        let kind = if matches!(status, 400 | 401) { io::ErrorKind::PermissionDenied } else { io::ErrorKind::Other };
        return Err(io::Error::new(kind, format!("Box did not accept the sign-in ({detail}).")));
    }
    let reply: TokenReply = serde_json::from_str(&body).map_err(io::Error::other)?;
    Ok(Tokens {
        access: reply.access_token,
        refresh: reply.refresh_token,
        renew_at: Instant::now() + Duration::from_secs(reply.expires_in.saturating_sub(60)),
    })
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buffer)).is_err() {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        buffer = Sha1::digest(nanos.to_le_bytes()).to_vec();
    }
    buffer.iter().map(|b| format!("{b:02x}")).collect()
}

/// The authorization page to open in the browser.
pub fn authorize_url(client_id: &str, state: &str) -> String {
    format!(
        "{AUTHORIZE}?response_type=code&client_id={}&redirect_uri={}&state={state}",
        http::encode_value(client_id),
        http::encode_value(&redirect_uri())
    )
}

/// Signs in in the browser and waits for Box to send it back.
fn sign_in(agent: &Agent, token_url: &str, client: &(String, String)) -> io::Result<Tokens> {
    let listener = TcpListener::bind(("127.0.0.1", REDIRECT_PORT)).map_err(|e| {
        io::Error::new(e.kind(), format!("forkz could not wait for Box on port {REDIRECT_PORT} ({e})."))
    })?;
    let state = random_hex(16);
    let opened = std::process::Command::new("/usr/bin/open").arg(authorize_url(&client.0, &state)).status()?;
    if !opened.success() {
        return Err(io::Error::other("The browser could not be opened to sign in to Box."));
    }
    let code = wait_for_code(&listener, &state, SIGN_IN_WAIT)?;
    let redirect = redirect_uri();
    token_request(
        agent,
        token_url,
        client,
        &[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect)],
    )
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

/// Waits for the browser to come back with the authorization code.
pub fn wait_for_code(listener: &TcpListener, state: &str, wait: Duration) -> io::Result<String> {
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
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        // All of the request is read (closing with some of it unread
        // would reset the connection, and the browser would not show the
        // page), but only its first line matters.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while head.len() < 16_384 && !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => break,
            }
        }
        let text = String::from_utf8_lossy(&head);
        let query = callback_query(text.lines().next().unwrap_or(""));
        if query.get("state").map(String::as_str) != Some(state) {
            // Something else (the browser asking for an icon).
            reply_page(&mut stream, "404 Not Found", "Not found");
            continue;
        }
        if let Some(code) = query.get("code") {
            reply_page(&mut stream, "200 OK", "Signed in to Box. You can close this tab and go back to forkz.");
            return Ok(code.clone());
        }
        let error = query.get("error").cloned().unwrap_or_default();
        reply_page(&mut stream, "200 OK", "forkz was not allowed to use Box. You can close this tab.");
        return Err(if error == "access_denied" {
            io::Error::new(io::ErrorKind::Interrupted, "Cancelled")
        } else {
            io::Error::new(io::ErrorKind::PermissionDenied, format!("Box refused the sign-in ({error})."))
        });
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "Signing in to Box took too long. Try connecting again."))
}

/// An error from Box's JSON ({"code": .., "message": ..}).
fn box_error(status: u16, body: &str) -> io::Error {
    let reply: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    match reply["code"].as_str() {
        Some("item_name_in_use") => {
            io::Error::new(io::ErrorKind::AlreadyExists, "An item with that name already exists there.")
        }
        Some("item_name_invalid") => io::Error::new(io::ErrorKind::InvalidInput, "Box does not allow that name."),
        Some("storage_limit_exceeded") => io::Error::new(io::ErrorKind::StorageFull, "The Box account is full."),
        Some("file_size_limit_exceeded") => {
            io::Error::other("The file is larger than the Box account allows for one file.")
        }
        _ => http::status_error(status, reply["message"].as_str().unwrap_or(body)),
    }
}

/// "/a/b" -> parent "/a", name "b".
fn parent_and_name(path: &Path) -> io::Result<(PathBuf, String)> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => Ok((parent.to_path_buf(), name.to_string_lossy().into_owned())),
        _ => Err(io::Error::other("All Files cannot be changed.")),
    }
}

fn root() -> Entry {
    entry(PathBuf::from("/"), EntryKind::Folder, 0, None)
}

fn entry(path: PathBuf, kind: EntryKind, size: u64, modified: Option<SystemTime>) -> Entry {
    let name = path.file_name().map_or_else(|| "/".to_string(), |n| n.to_string_lossy().into_owned());
    Entry {
        hidden: name.starts_with('.'),
        name,
        path,
        kind,
        size,
        modified,
        package: false,
        permissions: None,
        owner: None,
        group: None,
    }
}

impl Item {
    fn folder(&self) -> bool {
        self.kind == "folder"
    }

    fn entry(&self, path: PathBuf) -> Entry {
        let kind = if self.folder() { EntryKind::Folder } else { EntryKind::File };
        let size = if self.folder() { 0 } else { self.size.unwrap_or(0) };
        let modified = self.content_modified_at.as_deref().or(self.modified_at.as_deref()).and_then(http::parse_iso);
        entry(path, kind, size, modified)
    }
}

impl BoxFs {
    fn access_token(&self) -> io::Result<String> {
        let mut tokens = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if Instant::now() >= tokens.renew_at {
            self.renew_locked(&mut tokens)?;
        }
        Ok(tokens.access.clone())
    }

    fn renew_locked(&self, tokens: &mut Tokens) -> io::Result<()> {
        let refresh = tokens.refresh.clone();
        let renewed = token_request(
            &self.agent,
            &self.shared.service.token,
            &self.shared.client,
            &[("grant_type", "refresh_token"), ("refresh_token", &refresh)],
        )
        .map_err(|e| {
            if e.kind() == io::ErrorKind::PermissionDenied {
                io::Error::new(e.kind(), "The Box sign-in expired. Disconnect and connect again to sign in.")
            } else {
                e
            }
        })?;
        *tokens = renewed;
        if let Some(save) = &self.shared.save_token {
            save(&tokens.refresh);
        }
        Ok(())
    }

    /// Renews the access token after Box refused `used` (unless another
    /// connection already did).
    fn renew(&self, used: &str) -> io::Result<()> {
        let mut tokens = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if tokens.access == used { self.renew_locked(&mut tokens) } else { Ok(()) }
    }

    /// A request with the access token, renewed and retried once if Box
    /// refuses it, and retried when Box asks to slow down.
    fn call(
        &self,
        agent: &Agent,
        method: &str,
        url: &str,
        body: Option<&Value>,
        headers: &[(&str, String)],
    ) -> io::Result<ureq::http::Response<ureq::Body>> {
        let mut renewed = false;
        let mut waits = 0;
        loop {
            let started = Instant::now();
            let token = self.access_token()?;
            let mut builder = ureq::http::Request::builder()
                .method(method)
                .uri(url)
                .header("Authorization", format!("Bearer {token}"));
            for (name, value) in headers {
                builder = builder.header(*name, value.as_str());
            }
            let response = match body {
                Some(body) => {
                    let bytes = serde_json::to_vec(body).map_err(io::Error::other)?;
                    let request =
                        builder.header("Content-Type", "application/json").body(bytes).map_err(io::Error::other)?;
                    agent.run(request)
                }
                None => agent.run(builder.body(()).map_err(io::Error::other)?),
            }
            .map_err(http::to_io)?;
            let status = response.status().as_u16();
            tracing::debug!("box: {method} {url} answered {status} in {:?}", started.elapsed());
            if status == 401 && !renewed {
                renewed = true;
                self.renew(&token)?;
                continue;
            }
            if status == 429 && waits < 5 {
                waits += 1;
                std::thread::sleep(retry_after(&response).unwrap_or(Duration::from_secs(2)));
                continue;
            }
            if (200..400).contains(&status) {
                return Ok(response);
            }
            let text = response.into_body().read_to_string().unwrap_or_default();
            return Err(box_error(status, &text));
        }
    }

    fn json(&self, method: &str, url: &str, body: Option<&Value>) -> io::Result<Value> {
        let response = self.call(&self.agent, method, url, body, &[])?;
        let text = response.into_body().read_to_string().map_err(http::to_io)?;
        if text.trim().is_empty() { Ok(Value::Null) } else { serde_json::from_str(&text).map_err(io::Error::other) }
    }

    fn remember(&self, path: &Path, item: &Item) {
        self.shared
            .ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.to_path_buf(), (item.id.clone(), item.folder()));
    }

    /// Forgets a path and everything under it (moved or deleted).
    fn forget(&self, path: &Path) {
        self.shared.ids.lock().unwrap_or_else(|e| e.into_inner()).retain(|p, _| !p.starts_with(path));
    }

    /// The ID of a path, and whether it is a folder (listing the folders
    /// above it if they were not listed yet).
    fn id_of(&self, path: &Path) -> io::Result<(String, bool)> {
        if path.parent().is_none() {
            return Ok(("0".into(), true));
        }
        if let Some(found) = self.shared.ids.lock().unwrap_or_else(|e| e.into_inner()).get(path) {
            return Ok(found.clone());
        }
        let (parent, _) = parent_and_name(path)?;
        let (parent_id, folder) = self.id_of(&parent)?;
        if !folder {
            return Err(io::Error::new(io::ErrorKind::NotFound, "No such file or folder."));
        }
        self.items(&parent, &parent_id)?;
        self.shared
            .ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such file or folder."))
    }

    /// Lists a folder, and remembers the IDs in it.
    fn items(&self, dir: &Path, id: &str) -> io::Result<Vec<Entry>> {
        let mut found = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let api = &self.shared.service.api;
            let mut url = format!("{api}/folders/{id}/items?fields={FIELDS}&limit=1000&usemarker=true");
            if let Some(marker) = &marker {
                url.push_str(&format!("&marker={}", http::encode_value(marker)));
            }
            let page: Items = serde_json::from_value(self.json("GET", &url, None)?).map_err(io::Error::other)?;
            // Web links (bookmarks in Box) are not files.
            found.extend(page.entries.into_iter().filter(|item| item.kind == "file" || item.kind == "folder"));
            marker = page.next_marker.filter(|m| !m.is_empty());
            if marker.is_none() {
                break;
            }
        }
        let mut ids = self.shared.ids.lock().unwrap_or_else(|e| e.into_inner());
        ids.retain(|p, _| p.parent() != Some(dir));
        Ok(found
            .into_iter()
            .map(|item| {
                let path = dir.join(&item.name);
                ids.insert(path.clone(), (item.id.clone(), item.folder()));
                item.entry(path)
            })
            .collect())
    }

    fn copy_file(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (id, folder) = self.id_of(from)?;
        if folder {
            // The job copies folders item by item.
            return Err(io::Error::new(io::ErrorKind::Unsupported, "A folder."));
        }
        let (parent, name) = parent_and_name(to)?;
        let (parent_id, _) = self.id_of(&parent)?;
        let existing = match self.id_of(to) {
            Ok((_, true)) => return Err(io::Error::new(io::ErrorKind::IsADirectory, "A folder has that name.")),
            Ok((existing, false)) => Some(existing),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let copy_name = if existing.is_some() { format!(".{name}.forkz-copy") } else { name.clone() };
        let body = json!({ "parent": { "id": parent_id }, "name": copy_name });
        let api = &self.shared.service.api;
        let copied: Item = serde_json::from_value(self.json(
            "POST",
            &format!("{api}/files/{id}/copy?fields={FIELDS}"),
            Some(&body),
        )?)
        .map_err(io::Error::other)?;
        let Some(existing) = existing else {
            self.remember(to, &copied);
            return Ok(());
        };
        self.call(&self.agent, "DELETE", &self.item_url(&existing, false), None, &[])?;
        self.forget(to);
        let renamed: Item = serde_json::from_value(self.json(
            "PUT",
            &format!("{}?fields={FIELDS}", self.item_url(&copied.id, false)),
            Some(&json!({ "name": name })),
        )?)
        .map_err(io::Error::other)?;
        self.remember(to, &renamed);
        Ok(())
    }

    fn item_url(&self, id: &str, folder: bool) -> String {
        format!("{}/{}/{id}", self.shared.service.api, if folder { "folders" } else { "files" })
    }

    /// Uploads a file of a known size in one request (streamed).
    fn upload_simple(
        &self,
        path: &Path,
        existing: Option<&str>,
        size: u64,
        content: impl Read + Send + 'static,
    ) -> io::Result<()> {
        let (parent, name) = parent_and_name(path)?;
        let upload = &self.shared.service.upload;
        let (url, attributes) = match existing {
            // A new version of the file that is there.
            Some(id) => (format!("{upload}/files/{id}/content?fields={FIELDS}"), json!({ "name": name })),
            None => {
                let (parent_id, _) = self.id_of(&parent)?;
                (
                    format!("{upload}/files/content?fields={FIELDS}"),
                    json!({ "name": name, "parent": { "id": parent_id } }),
                )
            }
        };
        let boundary = format!("forkz{}", random_hex(12));
        let head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"attributes\"\r\n\r\n{attributes}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"upload\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        );
        let tail = format!("\r\n--{boundary}--\r\n");
        let length = head.len() as u64 + size + tail.len() as u64;
        let body = Cursor::new(head.into_bytes()).chain(content).chain(Cursor::new(tail.into_bytes()));
        let token = self.access_token()?;
        let request = ureq::http::Request::builder()
            .method("POST")
            .uri(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
            .header("Content-Length", length.to_string())
            .body(ureq::SendBody::from_owned_reader(body))
            .map_err(io::Error::other)?;
        let started = Instant::now();
        let response = self.agent.run(request).map_err(http::to_io)?;
        let status = response.status().as_u16();
        let text = response.into_body().read_to_string().unwrap_or_default();
        tracing::debug!("box: upload of {} answered {status} in {:?}", path.display(), started.elapsed());
        if !(200..300).contains(&status) {
            return Err(box_error(status, &text));
        }
        self.remember_uploaded(path, &text);
        Ok(())
    }

    /// Remembers the file an upload answered with ({"entries": [file]}).
    fn remember_uploaded(&self, path: &Path, text: &str) {
        if let Ok(reply) = serde_json::from_str::<Items>(text)
            && let Some(item) = reply.entries.first()
        {
            self.remember(path, item);
        }
    }

    /// Starts an upload in parts (for large files).
    fn upload_session(&self, path: &Path, existing: Option<&str>, size: u64) -> io::Result<PartWriter> {
        let (parent, name) = parent_and_name(path)?;
        let upload = &self.shared.service.upload;
        let session = match existing {
            Some(id) => {
                self.json("POST", &format!("{upload}/files/{id}/upload_sessions"), Some(&json!({ "file_size": size })))?
            }
            None => {
                let (parent_id, _) = self.id_of(&parent)?;
                let body = json!({ "folder_id": parent_id, "file_size": size, "file_name": name });
                self.json("POST", &format!("{upload}/files/upload_sessions"), Some(&body))?
            }
        };
        let id = session["id"].as_str().ok_or_else(|| io::Error::other("Box did not start the upload."))?.to_string();
        let part_size = session["part_size"].as_u64().unwrap_or(8 * 1024 * 1024) as usize;
        Ok(PartWriter {
            fs: self.clone(),
            path: path.to_path_buf(),
            session: id,
            part_size,
            total: size,
            buffer: Vec::with_capacity(part_size),
            offset: 0,
            parts: Vec::new(),
            whole: Sha1::new(),
            done: false,
        })
    }
}

fn retry_after(response: &ureq::http::Response<ureq::Body>) -> Option<Duration> {
    let seconds = response.headers().get("retry-after")?.to_str().ok()?.trim().parse::<u64>().ok()?;
    Some(Duration::from_secs(seconds.clamp(1, 30)))
}

/// Uploads a large file in parts as it is written (each part checked by
/// its SHA-1), then puts them together on flush.
struct PartWriter {
    fs: BoxFs,
    path: PathBuf,
    session: String,
    part_size: usize,
    total: u64,
    buffer: Vec<u8>,
    offset: u64,
    parts: Vec<Value>,
    whole: Sha1,
    done: bool,
}

impl PartWriter {
    fn send_part(&mut self, data: &[u8]) -> io::Result<()> {
        let end = self.offset + data.len() as u64 - 1;
        let digest = STANDARD.encode(Sha1::digest(data));
        let token = self.fs.access_token()?;
        let request = ureq::http::Request::builder()
            .method("PUT")
            .uri(format!("{}/files/upload_sessions/{}", self.fs.shared.service.upload, self.session))
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/octet-stream")
            .header("Content-Range", format!("bytes {}-{end}/{}", self.offset, self.total))
            .header("Digest", format!("sha={digest}"))
            .body(data.to_vec())
            .map_err(io::Error::other)?;
        let response = self.fs.agent.run(request).map_err(http::to_io)?;
        let status = response.status().as_u16();
        let text = response.into_body().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(box_error(status, &text));
        }
        let reply: Value = serde_json::from_str(&text).map_err(io::Error::other)?;
        self.parts.push(reply["part"].clone());
        self.whole.update(data);
        self.offset = end + 1;
        Ok(())
    }

    fn commit(&mut self) -> io::Result<()> {
        let digest = STANDARD.encode(self.whole.clone().finalize());
        let url =
            format!("{}/files/upload_sessions/{}/commit?fields={FIELDS}", self.fs.shared.service.upload, self.session);
        let body = json!({ "parts": self.parts });
        // Box may need a moment to put the parts together (202).
        for _ in 0..60 {
            let response =
                self.fs.call(&self.fs.agent, "POST", &url, Some(&body), &[("Digest", format!("sha={digest}"))])?;
            if response.status().as_u16() == 202 {
                std::thread::sleep(retry_after(&response).unwrap_or(Duration::from_secs(1)));
                continue;
            }
            let text = response.into_body().read_to_string().unwrap_or_default();
            self.fs.remember_uploaded(&self.path, &text);
            return Ok(());
        }
        Err(io::Error::other("Box did not finish putting the uploaded file together."))
    }
}

impl Write for PartWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.done {
            return Err(io::Error::other("the upload is finished"));
        }
        self.buffer.extend_from_slice(buf);
        while self.buffer.len() >= self.part_size {
            let part: Vec<u8> = self.buffer.drain(..self.part_size).collect();
            self.send_part(&part)?;
        }
        Ok(buf.len())
    }

    /// Ends the upload (the last part, then the commit).
    fn flush(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        if !self.buffer.is_empty() {
            let part = std::mem::take(&mut self.buffer);
            self.send_part(&part)?;
        }
        self.commit()
    }
}

/// For writes of an unknown size: kept in memory, uploaded on flush.
struct BufferedUpload {
    fs: BoxFs,
    path: PathBuf,
    data: Vec<u8>,
    done: bool,
}

impl Write for BufferedUpload {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.data.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        let data = std::mem::take(&mut self.data);
        let mut writer = self.fs.open_write_sized(&self.path, Some(data.len() as u64))?;
        writer.write_all(&data)?;
        writer.flush()
    }
}

impl Vfs for BoxFs {
    fn key(&self) -> String {
        format!("box://{}", self.user_id)
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        if self.login.is_empty() { "Box".into() } else { format!("Box: {}", self.login) }
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let (id, folder) = self.id_of(dir)?;
        if !folder {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "Not a folder."));
        }
        self.items(dir, &id)
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        if path.parent().is_none() {
            return Ok(root());
        }
        let (id, folder) = self.id_of(path)?;
        let url = format!("{}?fields={FIELDS}", self.item_url(&id, folder));
        match self.json("GET", &url, None) {
            Ok(value) => {
                let item: Item = serde_json::from_value(value).map_err(io::Error::other)?;
                Ok(item.entry(path.to_path_buf()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Gone since it was listed: look again.
                self.forget(path);
                let (parent, _) = parent_and_name(path)?;
                self.list(&parent)?
                    .into_iter()
                    .find(|e| e.path == path)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such file or folder."))
            }
            Err(e) => Err(e),
        }
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let (parent, name) = parent_and_name(path)?;
        let (parent_id, _) = self.id_of(&parent)?;
        let body = json!({ "name": name, "parent": { "id": parent_id } });
        let created: Item = serde_json::from_value(self.json(
            "POST",
            &format!("{}/folders?fields={FIELDS}", self.shared.service.api),
            Some(&body),
        )?)
        .map_err(io::Error::other)?;
        self.remember(path, &created);
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (id, folder) = self.id_of(from)?;
        let (from_parent, _) = parent_and_name(from)?;
        let (to_parent, name) = parent_and_name(to)?;
        let mut body = json!({ "name": name });
        if from_parent != to_parent {
            let (parent_id, _) = self.id_of(&to_parent)?;
            body["parent"] = json!({ "id": parent_id });
        }
        let moved: Item = serde_json::from_value(self.json(
            "PUT",
            &format!("{}?fields={FIELDS}", self.item_url(&id, folder)),
            Some(&body),
        )?)
        .map_err(io::Error::other)?;
        self.forget(from);
        self.remember(to, &moved);
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let (id, _) = self.id_of(path)?;
        self.call(&self.agent, "DELETE", &self.item_url(&id, false), None, &[])?;
        self.forget(path);
        Ok(())
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        if path.parent().is_none() {
            return Err(io::Error::other("All Files cannot be deleted."));
        }
        let (id, _) = self.id_of(path)?;
        // Into Box's trash, with anything forkz does not list (web links).
        self.call(&self.agent, "DELETE", &format!("{}?recursive=true", self.item_url(&id, true)), None, &[])?;
        self.forget(path);
        Ok(())
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.open_read_from(path, 0)
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let (id, folder) = self.id_of(path)?;
        if folder {
            return Err(io::Error::new(io::ErrorKind::IsADirectory, "Not a file."));
        }
        let range: Vec<(&str, String)> =
            if offset > 0 { vec![("Range", format!("bytes={offset}-"))] } else { Vec::new() };
        let url = format!("{}/files/{id}/content", self.shared.service.api);
        let mut response = None;
        // Box answers 202 while a new upload is not ready to download.
        for _ in 0..30 {
            let answer = self.call(&self.direct, "GET", &url, None, &range)?;
            if answer.status().as_u16() == 202 {
                std::thread::sleep(retry_after(&answer).unwrap_or(Duration::from_secs(1)));
                continue;
            }
            response = Some(answer);
            break;
        }
        let mut response = response.ok_or_else(|| io::Error::other("Box did not have the file ready to download."))?;
        // Usually a redirect to a short-lived download address (no token).
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| io::Error::other("Box did not say where to download the file."))?
                .to_string();
            let mut request = ureq::http::Request::builder().method("GET").uri(location);
            for (name, value) in &range {
                request = request.header(*name, value.as_str());
            }
            response = self.agent.run(request.body(()).map_err(io::Error::other)?).map_err(http::to_io)?;
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                return Err(http::status_error(status, &response.into_body().read_to_string().unwrap_or_default()));
            }
        }
        let partial = response.status().as_u16() == 206;
        let mut reader: Box<dyn Read + Send> = Box::new(response.into_body().into_reader());
        if offset > 0 && !partial {
            // The whole file came: skip to the offset.
            io::copy(&mut (&mut reader).take(offset), &mut io::sink())?;
        }
        Ok(reader)
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.open_write_sized(path, None)
    }

    fn open_write_sized(&self, path: &Path, size: Option<u64>) -> io::Result<Box<dyn Write + Send>> {
        let existing = match self.id_of(path) {
            Ok((_, true)) => return Err(io::Error::new(io::ErrorKind::IsADirectory, "A folder has that name.")),
            Ok((id, false)) => Some(id),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        match size {
            None => Ok(Box::new(BufferedUpload {
                fs: self.clone(),
                path: path.to_path_buf(),
                data: Vec::new(),
                done: false,
            })),
            Some(size) if size > self.shared.service.simple_upload => {
                Ok(Box::new(self.upload_session(path, existing.as_deref(), size)?))
            }
            Some(size) => {
                let fs = self.clone();
                let path = path.to_path_buf();
                Ok(Box::new(http::upload_pipe(move |reader| {
                    fs.upload_simple(&path, existing.as_deref(), size, reader)
                })))
            }
        }
    }

    fn set_attributes(&self, _path: &Path, _modified: Option<SystemTime>, _permissions: Option<u32>) -> io::Result<()> {
        // Box keeps the upload's date.
        Ok(())
    }

    fn removes_folders_whole(&self) -> bool {
        true
    }

    /// Box's copy, on its servers. Box does not copy over an existing
    /// file: the copy takes a temporary name, then replaces it.
    fn copy_on_server(&self, from: &Path, to: &Path) -> Option<io::Result<()>> {
        Some(self.copy_file(from, to))
    }

    fn deleted_items_go(&self) -> Option<&'static str> {
        Some("Box\u{2019}s trash")
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        // The same sign-in (Box allows one renewal at a time) and IDs.
        Ok(Arc::new(self.clone()))
    }

    fn display_path(&self, path: &Path) -> String {
        format!("box://box.com{}", path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_browser_coming_back() {
        let query = callback_query("GET /?code=abc%2B1&state=xyz HTTP/1.1\r\n");
        assert_eq!(query.get("code").map(String::as_str), Some("abc+1"));
        assert_eq!(query.get("state").map(String::as_str), Some("xyz"));
        assert!(callback_query("GET /favicon.ico HTTP/1.1").is_empty());
        let url = authorize_url("my id", "s1");
        assert!(url.starts_with("https://account.box.com/api/oauth2/authorize?response_type=code&client_id=my%20id"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A53791&state=s1"));
    }

    #[test]
    fn waits_for_the_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let browser = std::thread::spawn(move || {
            let get = |target: &str| {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
                let mut answer = String::new();
                stream.read_to_string(&mut answer).unwrap();
                answer
            };
            // An icon request and a wrong state are not the answer.
            assert!(get("/favicon.ico").starts_with("HTTP/1.1 404"));
            assert!(get("/?code=bad&state=other").starts_with("HTTP/1.1 404"));
            get("/?code=good&state=s1")
        });
        assert_eq!(wait_for_code(&listener, "s1", Duration::from_secs(10)).unwrap(), "good");
        assert!(browser.join().unwrap().contains("Signed in to Box"));

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stream, "GET /?error=access_denied&state=s2 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let _ = stream.read_to_string(&mut String::new());
        });
        let error = wait_for_code(&listener, "s2", Duration::from_secs(10)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        let quiet = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        assert_eq!(
            wait_for_code(&quiet, "s3", Duration::from_millis(200)).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn reads_items_and_errors() {
        let page: Items = serde_json::from_str(
            r#"{"total_count":2,"entries":[
                {"type":"folder","id":"11","name":"Docs","size":500,"modified_at":"2026-11-04T02:20:30-08:00"},
                {"type":"file","id":"12","name":"a.txt","size":42,"content_modified_at":"2026-11-04T10:20:30Z","modified_at":"2026-11-05T10:20:30Z"}
            ],"next_marker":""}"#,
        )
        .unwrap();
        let folder = page.entries[0].entry(PathBuf::from("/Docs"));
        assert_eq!((folder.kind, folder.size), (EntryKind::Folder, 0));
        let file = page.entries[1].entry(PathBuf::from("/a.txt"));
        assert_eq!((file.kind, file.size), (EntryKind::File, 42));
        // The file's own date, not when it was changed in Box.
        assert_eq!(file.modified, http::parse_iso("2026-11-04T10:20:30Z"));
        assert_eq!(folder.modified, file.modified);
        let conflict = box_error(
            409,
            r#"{"type":"error","status":409,"code":"item_name_in_use","message":"Item with the same name already exists"}"#,
        );
        assert_eq!(conflict.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(box_error(404, r#"{"code":"not_found","message":"Not Found"}"#).kind(), io::ErrorKind::NotFound);
        assert_eq!(parent_and_name(Path::new("/Docs/a.txt")).unwrap(), (PathBuf::from("/Docs"), "a.txt".into()));
        assert!(parent_and_name(Path::new("/")).is_err());
    }
}
