//! Dropbox, through its API. Signing in happens in the browser (OAuth
//! with PKCE, so forkz's app needs no secret); forkz keeps the sign-in (a
//! refresh token) in the Keychain for saved sites. Dropbox finds items by
//! path, as forkz does (case-insensitively).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::http::{self, Agent};
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

/// Where Dropbox sends the browser back after signing in (registered with
/// forkz's Dropbox app as http://127.0.0.1:53792).
pub const REDIRECT_PORT: u16 = 53_792;

/// Files up to this size are uploaded in one request (Dropbox allows 150
/// MB); larger ones in parts of this size.
const SIMPLE_UPLOAD: u64 = 128 * 1024 * 1024;

/// Saves a new sign-in.
pub type SaveToken = Arc<dyn Fn(&str) + Send + Sync>;

/// forkz's app at Dropbox (its app key).
fn app_key() -> io::Result<String> {
    super::oauth::app_setting("FORKZ_DROPBOX_APP_KEY", option_env!("FORKZ_DROPBOX_APP_KEY")).ok_or_else(|| {
        io::Error::other(
            "This copy of forkz cannot sign in to Dropbox: it was built without a Dropbox app (FORKZ_DROPBOX_APP_KEY).",
        )
    })
}

/// Where Dropbox is (a stand-in server for tests).
#[derive(Clone)]
pub struct Service {
    pub api: String,
    pub content: String,
    pub token: String,
    pub authorize: String,
    /// Larger files are uploaded in parts.
    pub simple_upload: u64,
    /// The app key (none: forkz's own).
    pub app_key: Option<String>,
}

impl Default for Service {
    fn default() -> Self {
        Service {
            api: "https://api.dropboxapi.com/2".into(),
            content: "https://content.dropboxapi.com/2".into(),
            token: "https://api.dropboxapi.com/oauth2/token".into(),
            authorize: "https://www.dropbox.com/oauth2/authorize".into(),
            simple_upload: SIMPLE_UPLOAD,
            app_key: None,
        }
    }
}

#[derive(Clone)]
pub struct DropboxParams {
    /// The last sign-in (none: sign in in the browser).
    pub refresh_token: Option<String>,
    pub save_token: Option<SaveToken>,
    pub timeout: Duration,
    pub service: Service,
    /// Sign in in the browser when there is no sign-in (off in tests).
    pub browser: bool,
}

struct Tokens {
    access: String,
    refresh: String,
    renew_at: Instant,
}

struct Shared {
    service: Service,
    app_key: String,
    tokens: Mutex<Tokens>,
}

#[derive(Clone)]
pub struct DropboxFs {
    shared: Arc<Shared>,
    agent: Agent,
    email: String,
    account: String,
}

fn tokens_from(reply: &Value, refresh: Option<&str>) -> io::Result<Tokens> {
    let access = reply["access_token"].as_str().ok_or_else(|| io::Error::other("Dropbox answered without a token."))?;
    let refresh = reply["refresh_token"].as_str().or(refresh).unwrap_or_default();
    let expires = reply["expires_in"].as_u64().unwrap_or(14_400);
    Ok(Tokens {
        access: access.to_string(),
        refresh: refresh.to_string(),
        renew_at: Instant::now() + Duration::from_secs(expires.saturating_sub(60)),
    })
}

/// Connects: renews the saved sign-in, or signs in in the browser.
pub fn connect(params: DropboxParams) -> io::Result<Arc<DropboxFs>> {
    let key = match params.service.app_key.clone() {
        Some(key) => key,
        None => app_key()?,
    };
    let agent = http::agent(params.timeout);
    let service = params.service.clone();
    let renew = |refresh: &str| {
        let reply = super::oauth::token_request(
            &agent,
            "Dropbox",
            &service.token,
            &[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", &key)],
        )?;
        tokens_from(&reply, Some(refresh))
    };
    let sign_in = || -> io::Result<Tokens> {
        let code = super::oauth::sign_in("Dropbox", REDIRECT_PORT, |redirect, state, challenge| {
            format!(
                "{}?client_id={}&response_type=code&token_access_type=offline&code_challenge={challenge}&code_challenge_method=S256&redirect_uri={}&state={state}",
                service.authorize,
                http::encode_value(&key),
                http::encode_value(redirect)
            )
        })?;
        let reply = super::oauth::token_request(
            &agent,
            "Dropbox",
            &service.token,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code.code),
                ("client_id", &key),
                ("redirect_uri", &code.redirect),
                ("code_verifier", &code.verifier),
            ],
        )?;
        tokens_from(&reply, None)
    };
    let saved = params.refresh_token.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let tokens = match saved {
        Some(refresh) => match renew(refresh) {
            Ok(tokens) => tokens,
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && params.browser => sign_in()?,
            Err(e) => return Err(e),
        },
        None if params.browser => sign_in()?,
        None => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Not signed in to Dropbox.")),
    };
    if let Some(save) = &params.save_token {
        save(&tokens.refresh);
    }
    let shared = Arc::new(Shared { service: params.service, app_key: key, tokens: Mutex::new(tokens) });
    let mut fs = DropboxFs { shared, agent, email: String::new(), account: String::new() };
    let me = fs.api("users/get_current_account", &Value::Null)?;
    fs.email = me["email"].as_str().unwrap_or_default().to_string();
    fs.account = me["account_id"].as_str().unwrap_or_default().to_string();
    Ok(Arc::new(fs))
}

/// A path as Dropbox writes it: "" for the top, else "/a/b".
fn dropbox_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() { String::new() } else { trimmed.to_string() }
}

/// An error from Dropbox ({"error_summary": "path/not_found/..", ..}).
fn dropbox_error(status: u16, body: &str) -> io::Error {
    let reply: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let summary = reply["error_summary"].as_str().unwrap_or_default();
    if summary.contains("not_found") {
        return io::Error::new(io::ErrorKind::NotFound, "No such file or folder.");
    }
    if summary.contains("conflict") {
        return io::Error::new(io::ErrorKind::AlreadyExists, "An item with that name already exists there.");
    }
    if summary.contains("insufficient_space") {
        return io::Error::new(io::ErrorKind::StorageFull, "The Dropbox account is full.");
    }
    if summary.contains("disallowed_name") || summary.contains("malformed_path") {
        return io::Error::new(io::ErrorKind::InvalidInput, "Dropbox does not allow that name.");
    }
    let detail = if summary.is_empty() { body.trim() } else { summary.trim_end_matches('/') };
    http::status_error(status, detail)
}

fn entry(path: PathBuf, item: &Value) -> Entry {
    let folder = item[".tag"] == "folder";
    let name = item["name"].as_str().map(str::to_string).unwrap_or_else(|| crate::pane::title_of(&path));
    let modified = item["client_modified"].as_str().or(item["server_modified"].as_str()).and_then(http::parse_iso);
    Entry {
        hidden: name.starts_with('.'),
        name,
        path,
        kind: if folder { EntryKind::Folder } else { EntryKind::File },
        size: item["size"].as_u64().unwrap_or(0),
        modified,
        package: false,
        permissions: None,
        owner: None,
        group: None,
    }
}

impl DropboxFs {
    fn access_token(&self) -> io::Result<String> {
        let mut tokens = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if Instant::now() >= tokens.renew_at {
            self.renew_locked(&mut tokens)?;
        }
        Ok(tokens.access.clone())
    }

    fn renew_locked(&self, tokens: &mut Tokens) -> io::Result<()> {
        let reply = super::oauth::token_request(
            &self.agent,
            "Dropbox",
            &self.shared.service.token,
            &[("grant_type", "refresh_token"), ("refresh_token", &tokens.refresh), ("client_id", &self.shared.app_key)],
        )?;
        *tokens = tokens_from(&reply, Some(&tokens.refresh))?;
        Ok(())
    }

    /// A request with the access token, renewed and tried again once if
    /// Dropbox refuses it, and tried again when Dropbox asks to slow down.
    fn send(
        &self,
        build: &dyn Fn(&str) -> io::Result<ureq::http::Request<Vec<u8>>>,
    ) -> io::Result<ureq::http::Response<ureq::Body>> {
        let mut renewed = false;
        let mut waits = 0;
        loop {
            let token = self.access_token()?;
            let response = self.agent.run(build(&token)?).map_err(http::to_io)?;
            let status = response.status().as_u16();
            if status == 401 && !renewed {
                renewed = true;
                let mut tokens = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner());
                if tokens.access == token {
                    self.renew_locked(&mut tokens)?;
                }
                continue;
            }
            if (status == 429 || status == 503) && waits < 5 {
                waits += 1;
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .unwrap_or(2);
                std::thread::sleep(Duration::from_secs(wait.clamp(1, 30)));
                continue;
            }
            if (200..300).contains(&status) || status == 206 {
                return Ok(response);
            }
            let text = response.into_body().read_to_string().unwrap_or_default();
            return Err(dropbox_error(status, &text));
        }
    }

    /// An RPC call (JSON in and out).
    fn api(&self, route: &str, body: &Value) -> io::Result<Value> {
        let url = format!("{}/{route}", self.shared.service.api);
        let bytes =
            if body.is_null() { b"null".to_vec() } else { serde_json::to_vec(body).map_err(io::Error::other)? };
        let response = self.send(&|token| {
            ureq::http::Request::builder()
                .method("POST")
                .uri(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .body(bytes.clone())
                .map_err(io::Error::other)
        })?;
        let text = response.into_body().read_to_string().map_err(http::to_io)?;
        if text.trim().is_empty() { Ok(Value::Null) } else { serde_json::from_str(&text).map_err(io::Error::other) }
    }

    fn upload_whole(&self, path: &Path, data: Vec<u8>) -> io::Result<()> {
        let arg = json!({ "path": dropbox_path(path), "mode": "overwrite", "mute": true }).to_string();
        let url = format!("{}/files/upload", self.shared.service.content);
        self.send(&|token| {
            ureq::http::Request::builder()
                .method("POST")
                .uri(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/octet-stream")
                .header("Dropbox-API-Arg", http_header_json(&arg))
                .body(data.clone())
                .map_err(io::Error::other)
        })?;
        Ok(())
    }

    /// A content call with a JSON argument in the header and bytes as the
    /// body (an upload session's parts).
    fn content(&self, route: &str, arg: &Value, data: Vec<u8>) -> io::Result<Value> {
        let url = format!("{}/{route}", self.shared.service.content);
        let arg = http_header_json(&arg.to_string());
        let response = self.send(&|token| {
            ureq::http::Request::builder()
                .method("POST")
                .uri(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/octet-stream")
                .header("Dropbox-API-Arg", arg.as_str())
                .body(data.clone())
                .map_err(io::Error::other)
        })?;
        let text = response.into_body().read_to_string().unwrap_or_default();
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

/// JSON for a header: characters outside ASCII escaped (as Dropbox asks).
fn http_header_json(json: &str) -> String {
    json.chars()
        .map(|c| {
            if c.is_ascii() {
                c.to_string()
            } else {
                c.encode_utf16(&mut [0; 2]).iter().map(|u| format!("\\u{u:04x}")).collect()
            }
        })
        .collect()
}

/// Uploads as it is written: a file of a known size up to the simple
/// limit in one request, larger ones in parts (an upload session).
struct Upload {
    fs: DropboxFs,
    path: PathBuf,
    buffer: Vec<u8>,
    part: usize,
    session: Option<String>,
    offset: u64,
    /// Parts are used (a large or unknown size).
    in_parts: bool,
    done: bool,
}

impl Upload {
    fn send_part(&mut self, data: Vec<u8>) -> io::Result<()> {
        let length = data.len() as u64;
        match &self.session {
            None => {
                let reply = self.fs.content("files/upload_session/start", &json!({ "close": false }), data)?;
                let id = reply["session_id"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("Dropbox did not start the upload."))?;
                self.session = Some(id.to_string());
            }
            Some(id) => {
                let arg = json!({ "cursor": { "session_id": id, "offset": self.offset }, "close": false });
                self.fs.content("files/upload_session/append_v2", &arg, data)?;
            }
        }
        self.offset += length;
        Ok(())
    }
}

impl Write for Upload {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.done {
            return Err(io::Error::other("the upload is finished"));
        }
        self.buffer.extend_from_slice(buf);
        while self.in_parts && self.buffer.len() >= self.part {
            let part: Vec<u8> = self.buffer.drain(..self.part).collect();
            self.send_part(part)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        let rest = std::mem::take(&mut self.buffer);
        if self.session.is_none() {
            // All of it fits in one request.
            if rest.len() as u64 <= self.fs.shared.service.simple_upload {
                return self.fs.upload_whole(&self.path, rest);
            }
            self.send_part(rest)?;
            return self.finish(Vec::new());
        }
        self.finish(rest)
    }
}

impl Upload {
    fn finish(&mut self, last: Vec<u8>) -> io::Result<()> {
        let id = self.session.clone().unwrap_or_default();
        let arg = json!({
            "cursor": { "session_id": id, "offset": self.offset },
            "commit": { "path": dropbox_path(&self.path), "mode": "overwrite", "mute": true },
        });
        self.fs.content("files/upload_session/finish", &arg, last)?;
        Ok(())
    }
}

impl Vfs for DropboxFs {
    fn key(&self) -> String {
        format!("dropbox://{}", self.account)
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        if self.email.is_empty() { "Dropbox".into() } else { format!("Dropbox: {}", self.email) }
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut found = Vec::new();
        let mut page = self.api("files/list_folder", &json!({ "path": dropbox_path(dir), "limit": 2000 }))?;
        loop {
            for item in page["entries"].as_array().into_iter().flatten() {
                // Deleted items are not listed; files and folders are.
                if item[".tag"] == "file" || item[".tag"] == "folder" {
                    let name = item["name"].as_str().unwrap_or_default();
                    found.push(entry(dir.join(name), item));
                }
            }
            if page["has_more"] != true {
                break;
            }
            let cursor = page["cursor"].as_str().unwrap_or_default().to_string();
            page = self.api("files/list_folder/continue", &json!({ "cursor": cursor }))?;
        }
        Ok(found)
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        if dropbox_path(path).is_empty() {
            let mut root = entry(PathBuf::from("/"), &json!({ ".tag": "folder", "name": "/" }));
            root.name = "/".into();
            return Ok(root);
        }
        let item = self.api("files/get_metadata", &json!({ "path": dropbox_path(path) }))?;
        Ok(entry(path.to_path_buf(), &item))
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.api("files/create_folder_v2", &json!({ "path": dropbox_path(path), "autorename": false }))?;
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let body = json!({ "from_path": dropbox_path(from), "to_path": dropbox_path(to), "autorename": false });
        match self.api("files/move_v2", &body) {
            // A file is there: replaced (as renames do), unless it is the
            // same item in another case.
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && !dropbox_path(from).eq_ignore_ascii_case(&dropbox_path(to)) =>
            {
                self.remove_file(to)?;
                self.api("files/move_v2", &body).map(|_| ())
            }
            other => other.map(|_| ()),
        }
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.api("files/delete_v2", &json!({ "path": dropbox_path(path) }))?;
        Ok(())
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.remove_file(path)
    }

    fn removes_folders_whole(&self) -> bool {
        true
    }

    fn deleted_items_go(&self) -> Option<&'static str> {
        Some("Dropbox\u{2019}s deleted files")
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.open_read_from(path, 0)
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let url = format!("{}/files/download", self.shared.service.content);
        let arg = http_header_json(&json!({ "path": dropbox_path(path) }).to_string());
        let response = self.send(&|token| {
            let mut request = ureq::http::Request::builder()
                .method("POST")
                .uri(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Dropbox-API-Arg", arg.as_str());
            if offset > 0 {
                request = request.header("Range", format!("bytes={offset}-"));
            }
            request.body(Vec::new()).map_err(io::Error::other)
        })?;
        Ok(Box::new(response.into_body().into_reader()))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.open_write_sized(path, None)
    }

    fn open_write_sized(&self, path: &Path, size: Option<u64>) -> io::Result<Box<dyn Write + Send>> {
        let simple = self.shared.service.simple_upload;
        Ok(Box::new(Upload {
            fs: self.clone(),
            path: path.to_path_buf(),
            buffer: Vec::new(),
            // Parts of 32 MB (Dropbox wants multiples of 4 MB).
            part: simple.min(32 * 1024 * 1024) as usize,
            session: None,
            offset: 0,
            // A size over the limit (or none) goes in parts as it comes.
            in_parts: size.is_none_or(|s| s > simple),
            done: false,
        }))
    }

    fn writes_whole(&self) -> bool {
        true
    }

    fn copy_on_server(&self, from: &Path, to: &Path) -> Option<io::Result<()>> {
        let body = json!({ "from_path": dropbox_path(from), "to_path": dropbox_path(to), "autorename": false });
        Some(match self.api("files/copy_v2", &body) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                self.remove_file(to).and_then(|()| self.api("files/copy_v2", &body).map(|_| ()))
            }
            other => other.map(|_| ()),
        })
    }

    fn set_attributes(&self, _path: &Path, _modified: Option<SystemTime>, _permissions: Option<u32>) -> io::Result<()> {
        // Dropbox keeps the date a file is uploaded with, and none can be
        // set after.
        Ok(())
    }

    fn shares_links(&self) -> bool {
        true
    }

    /// A shared link (paid accounts can set when it expires; on others it
    /// stays until removed in Dropbox).
    fn share_link(&self, path: &Path, valid_for: Duration) -> io::Result<(String, bool)> {
        let until = http::iso_utc(SystemTime::now() + valid_for);
        let target = dropbox_path(path);
        let with_expiry = json!({ "path": target, "settings": { "expires": until, "audience": "public" } });
        match self.api("sharing/create_shared_link_with_settings", &with_expiry) {
            Ok(reply) => return link_of(&reply).map(|l| (l, true)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(e),
            Err(_) => {}
        }
        // Not allowed (a basic account), or there is a link already.
        match self.api("sharing/create_shared_link_with_settings", &json!({ "path": target })) {
            Ok(reply) => link_of(&reply).map(|l| (l, false)),
            Err(_) => {
                let existing =
                    self.api("sharing/list_shared_links", &json!({ "path": target, "direct_only": true }))?;
                let link = existing["links"].get(0).ok_or_else(|| io::Error::other("Dropbox did not make a link."))?;
                link_of(link).map(|l| (l, link["expires"].is_string()))
            }
        }
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(DropboxFs { agent: http::agent(Duration::from_secs(30)), ..self.clone() }))
    }

    fn display_path(&self, path: &Path) -> String {
        format!("dropbox://{}", http::encode_path(&path.to_string_lossy()).trim_start_matches('/'))
    }
}

fn link_of(reply: &Value) -> io::Result<String> {
    reply["url"].as_str().map(str::to_string).ok_or_else(|| io::Error::other("Dropbox did not answer with a link."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_errors() {
        assert_eq!(dropbox_path(Path::new("/")), "");
        assert_eq!(dropbox_path(Path::new("/Photos/a.jpg")), "/Photos/a.jpg");
        assert_eq!(http_header_json("{\"path\":\"/caf\u{e9}\"}"), "{\"path\":\"/caf\\u00e9\"}");
        let error = dropbox_error(409, r#"{"error_summary": "path/not_found/..", "error": {}}"#);
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let error = dropbox_error(409, r#"{"error_summary": "path/conflict/folder/..."}"#);
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        let item = json!({".tag": "file", "name": "a.txt", "size": 5, "client_modified": "2026-11-04T10:20:30Z"});
        let e = entry(PathBuf::from("/a.txt"), &item);
        assert_eq!((e.size, e.kind == EntryKind::File, e.modified.is_some()), (5, true, true));
    }
}
