//! OneDrive (personal, and work or school accounts), through Microsoft
//! Graph. Signing in happens in the browser (OAuth with PKCE, so forkz's app
//! needs no secret); forkz keeps the sign-in (a refresh token, replaced each
//! time it is renewed) in the Keychain for saved sites. Graph finds items
//! by path, as forkz does (case-insensitively).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use serde_json::{Value, json};

use super::http::{self, Agent};
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

/// What forkz asks for: the user's files (and those shared with them),
/// staying signed in, and their name and email.
const SCOPES: &str = "https://graph.microsoft.com/Files.ReadWrite.All offline_access User.Read";

/// Files up to this size are uploaded in one request (Graph allows 4 MB);
/// larger ones in an upload session.
const SIMPLE_UPLOAD: u64 = 4 * 1024 * 1024;

/// An upload session's parts: 10 MiB (Graph wants multiples of 320 KiB).
const PART: usize = 32 * 320 * 1024;

/// What forkz reads of each item.
const SELECT: &str = "id,name,size,folder,file,package,lastModifiedDateTime,fileSystemInfo";

/// Saves a new sign-in.
pub type SaveToken = Arc<dyn Fn(&str) + Send + Sync>;

/// Makes a request, given the access token (None for pre-signed URLs).
type BuildRequest<'a> = dyn Fn(Option<&str>) -> io::Result<ureq::http::Request<Vec<u8>>> + 'a;

/// forkz's app at Microsoft (its client ID).
fn client_id() -> io::Result<String> {
    super::oauth::app_setting("FORKZ_ONEDRIVE_CLIENT_ID", option_env!("FORKZ_ONEDRIVE_CLIENT_ID")).ok_or_else(|| {
        io::Error::other(
            "This copy of forkz cannot sign in to OneDrive: it was built without a Microsoft app (FORKZ_ONEDRIVE_CLIENT_ID).",
        )
    })
}

/// Where Microsoft is (a stand-in server for tests).
#[derive(Clone)]
pub struct Service {
    pub graph: String,
    pub token: String,
    pub authorize: String,
    /// Larger files are uploaded in parts of `part` bytes.
    pub simple_upload: u64,
    pub part: usize,
    /// The client ID (none: forkz's own).
    pub client_id: Option<String>,
}

impl Default for Service {
    fn default() -> Self {
        Service {
            graph: "https://graph.microsoft.com/v1.0".into(),
            token: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
            authorize: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
            simple_upload: SIMPLE_UPLOAD,
            part: PART,
            client_id: None,
        }
    }
}

#[derive(Clone)]
pub struct OneDriveParams {
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
    client_id: String,
    tokens: Mutex<Tokens>,
    save_token: Option<SaveToken>,
}

#[derive(Clone)]
pub struct OneDriveFs {
    shared: Arc<Shared>,
    agent: Agent,
    email: String,
    drive: String,
}

fn tokens_from(reply: &Value, refresh: Option<&str>) -> io::Result<Tokens> {
    let access =
        reply["access_token"].as_str().ok_or_else(|| io::Error::other("Microsoft answered without a token."))?;
    let refresh = reply["refresh_token"].as_str().or(refresh).unwrap_or_default();
    let expires = reply["expires_in"].as_u64().unwrap_or(3_600);
    Ok(Tokens {
        access: access.to_string(),
        refresh: refresh.to_string(),
        renew_at: Instant::now() + Duration::from_secs(expires.saturating_sub(60)),
    })
}

/// Connects: renews the saved sign-in, or signs in in the browser.
pub fn connect(params: OneDriveParams) -> io::Result<Arc<OneDriveFs>> {
    let id = match params.service.client_id.clone() {
        Some(id) => id,
        None => client_id()?,
    };
    let agent = http::agent(params.timeout);
    let service = params.service.clone();
    let renew = |refresh: &str| {
        let reply = super::oauth::token_request(
            &agent,
            "OneDrive",
            &service.token,
            &[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", &id), ("scope", SCOPES)],
        )?;
        tokens_from(&reply, Some(refresh))
    };
    let sign_in = || -> io::Result<Tokens> {
        // Any port: Microsoft accepts http://127.0.0.1 on any port for apps
        // on a computer.
        let code = super::oauth::sign_in("OneDrive", 0, |redirect, state, challenge| {
            format!(
                "{}?client_id={}&response_type=code&response_mode=query&prompt=select_account&scope={}&code_challenge={challenge}&code_challenge_method=S256&redirect_uri={}&state={state}",
                service.authorize,
                http::encode_value(&id),
                http::encode_value(SCOPES),
                http::encode_value(redirect)
            )
        })?;
        let reply = super::oauth::token_request(
            &agent,
            "OneDrive",
            &service.token,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code.code),
                ("client_id", &id),
                ("redirect_uri", &code.redirect),
                ("code_verifier", &code.verifier),
                ("scope", SCOPES),
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
        None => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Not signed in to OneDrive.")),
    };
    if let Some(save) = &params.save_token {
        save(&tokens.refresh);
    }
    let shared = Arc::new(Shared {
        service: params.service,
        client_id: id,
        tokens: Mutex::new(tokens),
        save_token: params.save_token,
    });
    let mut fs = OneDriveFs { shared, agent, email: String::new(), drive: String::new() };
    let me = fs.call("GET", &format!("{}/me?$select=mail,userPrincipalName", fs.graph()), None)?;
    fs.email = me["mail"].as_str().or(me["userPrincipalName"].as_str()).unwrap_or_default().to_string();
    let drive = fs.call("GET", &format!("{}/me/drive?$select=id", fs.graph()), None)?;
    fs.drive = drive["id"].as_str().unwrap_or_default().to_string();
    Ok(Arc::new(fs))
}

/// A path as Graph addresses it under the drive's root: "" for the top,
/// else "/a/b" percent-encoded.
fn graph_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() { String::new() } else { http::encode_path(trimmed) }
}

/// An error from Graph ({"error": {"code": "itemNotFound", "message": ..}}).
fn graph_error(status: u16, body: &str) -> io::Error {
    let reply: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = reply["error"]["code"].as_str().unwrap_or_default();
    let message = reply["error"]["message"].as_str().unwrap_or_default();
    match code {
        "itemNotFound" => return io::Error::new(io::ErrorKind::NotFound, "No such file or folder."),
        "nameAlreadyExists" => {
            return io::Error::new(io::ErrorKind::AlreadyExists, "An item with that name already exists there.");
        }
        "quotaLimitReached" | "insufficientStorage" => {
            return io::Error::new(io::ErrorKind::StorageFull, "The OneDrive is full.");
        }
        "accessDenied" => return io::Error::new(io::ErrorKind::PermissionDenied, "Access denied."),
        _ => {}
    }
    if status == 409 {
        return io::Error::new(io::ErrorKind::AlreadyExists, "An item with that name already exists there.");
    }
    let detail = if message.is_empty() { body.trim() } else { message };
    http::status_error(status, detail)
}

fn entry(path: PathBuf, item: &Value) -> Entry {
    // OneNote notebooks are packages of folders.
    let folder = item["folder"].is_object() || item["package"].is_object();
    let name = item["name"].as_str().map(str::to_string).unwrap_or_else(|| crate::pane::title_of(&path));
    let modified = item["fileSystemInfo"]["lastModifiedDateTime"]
        .as_str()
        .or(item["lastModifiedDateTime"].as_str())
        .and_then(http::parse_iso);
    Entry {
        hidden: name.starts_with('.'),
        name,
        path,
        kind: if folder { EntryKind::Folder } else { EntryKind::File },
        size: if folder { 0 } else { item["size"].as_u64().unwrap_or(0) },
        modified,
        package: false,
        permissions: None,
        owner: None,
        group: None,
    }
}

impl OneDriveFs {
    fn graph(&self) -> &str {
        &self.shared.service.graph
    }

    /// The URL of an item by path, with `rest` after it ("/children",
    /// "/content", or "").
    fn item_url(&self, path: &Path, rest: &str) -> String {
        match graph_path(path).as_str() {
            "" => format!("{}/me/drive/root{rest}", self.graph()),
            path if rest.is_empty() => format!("{}/me/drive/root:{path}", self.graph()),
            path => format!("{}/me/drive/root:{path}:{rest}", self.graph()),
        }
    }

    fn access_token(&self) -> io::Result<String> {
        let mut tokens = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if Instant::now() >= tokens.renew_at {
            self.renew_locked(&mut tokens)?;
        }
        Ok(tokens.access.clone())
    }

    /// Microsoft replaces the sign-in each time it is renewed: a saved
    /// site keeps the latest one in the Keychain.
    fn renew_locked(&self, tokens: &mut Tokens) -> io::Result<()> {
        let reply = super::oauth::token_request(
            &self.agent,
            "OneDrive",
            &self.shared.service.token,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &tokens.refresh),
                ("client_id", &self.shared.client_id),
                ("scope", SCOPES),
            ],
        )?;
        let renewed = tokens_from(&reply, Some(&tokens.refresh))?;
        let changed = renewed.refresh != tokens.refresh;
        *tokens = renewed;
        if changed && let Some(save) = &self.shared.save_token {
            save(&tokens.refresh);
        }
        Ok(())
    }

    /// A request with the access token (none for Graph's pre-signed upload
    /// and download URLs), renewed and tried again once if Graph refuses
    /// it, and tried again when Graph asks to slow down.
    fn send(&self, signed: bool, build: &BuildRequest<'_>) -> io::Result<ureq::http::Response<ureq::Body>> {
        let mut renewed = false;
        let mut waits = 0;
        loop {
            let token = if signed { Some(self.access_token()?) } else { None };
            let response = self.agent.run(build(token.as_deref())?).map_err(http::to_io)?;
            let status = response.status().as_u16();
            if status == 401 && signed && !renewed {
                renewed = true;
                let mut tokens = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner());
                if Some(&tokens.access) == token.as_ref() {
                    self.renew_locked(&mut tokens)?;
                }
                continue;
            }
            if matches!(status, 429 | 503 | 509) && waits < 5 {
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
            if (200..300).contains(&status) {
                return Ok(response);
            }
            let text = response.into_body().read_to_string().unwrap_or_default();
            return Err(graph_error(status, &text));
        }
    }

    /// A Graph call with JSON in (if any) and out.
    fn call(&self, method: &str, url: &str, body: Option<&Value>) -> io::Result<Value> {
        let bytes = match body {
            Some(body) => serde_json::to_vec(body).map_err(io::Error::other)?,
            None => Vec::new(),
        };
        let response = self.send(true, &|token| {
            let mut request = ureq::http::Request::builder().method(method).uri(url);
            if let Some(token) = token {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            if body.is_some() {
                request = request.header("Content-Type", "application/json");
            }
            request.body(bytes.clone()).map_err(io::Error::other)
        })?;
        let text = response.into_body().read_to_string().map_err(http::to_io)?;
        if text.trim().is_empty() { Ok(Value::Null) } else { serde_json::from_str(&text).map_err(io::Error::other) }
    }

    /// An item's details by path.
    fn item(&self, path: &Path, select: &str) -> io::Result<Value> {
        self.call("GET", &format!("{}?$select={select}", self.item_url(path, "")), None)
    }

    /// Uploads a small file in one request.
    fn upload_whole(&self, path: &Path, data: Vec<u8>) -> io::Result<()> {
        let url = format!("{}?@microsoft.graph.conflictBehavior=replace", self.item_url(path, "/content"));
        self.send(true, &|token| {
            ureq::http::Request::builder()
                .method("PUT")
                .uri(&url)
                .header("Authorization", format!("Bearer {}", token.unwrap_or_default()))
                .header("Content-Type", "application/octet-stream")
                .body(data.clone())
                .map_err(io::Error::other)
        })?;
        Ok(())
    }

    /// Starts an upload session for a larger file: the URL its parts go to.
    fn start_session(&self, path: &Path) -> io::Result<String> {
        let body = json!({ "item": { "@microsoft.graph.conflictBehavior": "replace" } });
        let reply = self.call("POST", &self.item_url(path, "/createUploadSession"), Some(&body))?;
        reply["uploadUrl"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| io::Error::other("OneDrive did not start the upload."))
    }

    /// Has OneDrive copy an item, and waits until it is done.
    fn copy_and_wait(&self, from: &Path, to: &Path) -> io::Result<()> {
        if self.stat(to).is_ok() {
            self.remove_file(to)?;
        }
        let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let body = json!({ "parentReference": { "driveId": self.drive, "id": self.parent_id(to)? }, "name": name });
        let url = self.item_url(from, "/copy");
        let bytes = serde_json::to_vec(&body).map_err(io::Error::other)?;
        let response = self.send(true, &|token| {
            ureq::http::Request::builder()
                .method("POST")
                .uri(&url)
                .header("Authorization", format!("Bearer {}", token.unwrap_or_default()))
                .header("Content-Type", "application/json")
                .body(bytes.clone())
                .map_err(io::Error::other)
        })?;
        let Some(monitor) = response.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string) else {
            return Ok(());
        };
        let until = Instant::now() + Duration::from_secs(30 * 60);
        loop {
            let reply = self.send(false, &|_| {
                ureq::http::Request::builder().method("GET").uri(&monitor).body(Vec::new()).map_err(io::Error::other)
            })?;
            let text = reply.into_body().read_to_string().unwrap_or_default();
            let status: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            match status["status"].as_str() {
                Some("completed") => return Ok(()),
                // The finished copy itself (some answers end with it).
                None if status["id"].is_string() => return Ok(()),
                Some("failed") => {
                    let detail = status["error"]["message"].as_str().unwrap_or("OneDrive could not copy it.");
                    return Err(io::Error::other(detail.to_string()));
                }
                _ if Instant::now() > until => return Err(io::Error::other("OneDrive took too long to copy it.")),
                _ => std::thread::sleep(Duration::from_millis(500)),
            }
        }
    }

    /// Gives a copy (and everything in it) the original's dates. A date
    /// that cannot be set is left as OneDrive made it.
    fn restore_dates(&self, from: &Path, to: &Path) {
        let Ok(original) = self.stat(from) else { return };
        if original.kind == EntryKind::Folder {
            for item in self.list(from).unwrap_or_default() {
                self.restore_dates(&item.path, &to.join(&item.name));
            }
        }
        if let Err(e) = self.set_attributes(to, original.modified, None) {
            tracing::info!("could not keep the date of {}: {e}", to.display());
        }
    }

    /// The parent folder's ID, for moves and copies.
    fn parent_id(&self, path: &Path) -> io::Result<String> {
        let parent = path.parent().unwrap_or(Path::new("/"));
        let item = self.item(parent, "id")?;
        item["id"].as_str().map(str::to_string).ok_or_else(|| io::Error::other("OneDrive did not say where that is."))
    }
}

/// Uploads as it is written: a file up to the simple limit in one request,
/// larger ones in an upload session (parts sent as they fill).
struct Upload {
    fs: OneDriveFs,
    path: PathBuf,
    buffer: Vec<u8>,
    /// The total size, for each part's Content-Range (none: known at the
    /// end, so it is held until then).
    size: Option<u64>,
    session: Option<String>,
    offset: u64,
    done: bool,
}

impl Upload {
    fn in_parts(&self) -> bool {
        self.size.is_some_and(|s| s > self.fs.shared.service.simple_upload)
    }

    fn send_part(&mut self, data: Vec<u8>, total: u64) -> io::Result<()> {
        let url = match &self.session {
            Some(url) => url.clone(),
            None => {
                let url = self.fs.start_session(&self.path)?;
                self.session = Some(url.clone());
                url
            }
        };
        let length = data.len() as u64;
        let range = format!("bytes {}-{}/{total}", self.offset, self.offset + length.max(1) - 1);
        // The session's URL is signed: no token (Graph refuses one there).
        self.fs.send(false, &|_| {
            ureq::http::Request::builder()
                .method("PUT")
                .uri(&url)
                .header("Content-Range", range.as_str())
                .body(data.clone())
                .map_err(io::Error::other)
        })?;
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
        let part = self.fs.shared.service.part;
        if let Some(total) = self.size.filter(|_| self.in_parts()) {
            // Keep the last part for flush (it closes the session).
            while self.buffer.len() > part {
                let data: Vec<u8> = self.buffer.drain(..part).collect();
                self.send_part(data, total)?;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        let rest = std::mem::take(&mut self.buffer);
        let total = self.offset + rest.len() as u64;
        if self.session.is_none() && total <= self.fs.shared.service.simple_upload {
            return self.fs.upload_whole(&self.path, rest);
        }
        let part = self.fs.shared.service.part;
        let mut rest = rest.as_slice();
        while !rest.is_empty() {
            let take = rest.len().min(part);
            self.send_part(rest[..take].to_vec(), total)?;
            rest = &rest[take..];
        }
        Ok(())
    }
}

impl Vfs for OneDriveFs {
    fn key(&self) -> String {
        format!("onedrive://{}", self.drive)
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        if self.email.is_empty() { "OneDrive".into() } else { format!("OneDrive: {}", self.email) }
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut found = Vec::new();
        let mut url = Some(format!("{}?$top=1000&$select={SELECT}", self.item_url(dir, "/children")));
        while let Some(next) = url {
            let page = self.call("GET", &next, None)?;
            for item in page["value"].as_array().into_iter().flatten() {
                let name = item["name"].as_str().unwrap_or_default();
                found.push(entry(dir.join(name), item));
            }
            url = page["@odata.nextLink"].as_str().map(str::to_string);
        }
        Ok(found)
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        let item = self.item(path, SELECT)?;
        let mut found = entry(path.to_path_buf(), &item);
        if graph_path(path).is_empty() {
            found.name = "/".into();
            found.kind = EntryKind::Folder;
        }
        Ok(found)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let parent = path.parent().unwrap_or(Path::new("/"));
        let body = json!({ "name": name, "folder": {}, "@microsoft.graph.conflictBehavior": "fail" });
        self.call("POST", &self.item_url(parent, "/children"), Some(&body))?;
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut body = json!({ "name": name });
        if from.parent() != to.parent() {
            body["parentReference"] = json!({ "id": self.parent_id(to)? });
        }
        match self.call("PATCH", &self.item_url(from, ""), Some(&body)) {
            // An item is there: replaced (as renames do), unless it is the
            // same item in another case.
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && !graph_path(from).eq_ignore_ascii_case(&graph_path(to)) =>
            {
                self.remove_file(to)?;
                self.call("PATCH", &self.item_url(from, ""), Some(&body)).map(|_| ())
            }
            other => other.map(|_| ()),
        }
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.call("DELETE", &self.item_url(path, ""), None)?;
        Ok(())
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.remove_file(path)
    }

    fn removes_folders_whole(&self) -> bool {
        true
    }

    fn deleted_items_go(&self) -> Option<&'static str> {
        Some("the OneDrive recycle bin")
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.open_read_from(path, 0)
    }

    /// From the file's pre-signed download URL (no token there; it takes
    /// a range, to resume). Graph only includes the URL with the whole
    /// item, not when fields are picked with $select.
    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let item = self.call("GET", &self.item_url(path, ""), None)?;
        if item["folder"].is_object() {
            return Err(io::Error::new(io::ErrorKind::IsADirectory, "That is a folder."));
        }
        let url = item["@microsoft.graph.downloadUrl"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| io::Error::other("OneDrive does not offer that item for download."))?;
        let response = self.send(false, &|_| {
            let mut request = ureq::http::Request::builder().method("GET").uri(&url);
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
        Ok(Box::new(Upload {
            fs: self.clone(),
            path: path.to_path_buf(),
            buffer: Vec::new(),
            size,
            session: None,
            offset: 0,
            done: false,
        }))
    }

    fn checksum_kind(&self) -> Option<crate::fs::checksum::Kind> {
        Some(crate::fs::checksum::Kind::QuickXor)
    }

    /// OneDrive's QuickXorHash (base64), as hex.
    fn server_checksum(&self, path: &Path) -> io::Result<Option<String>> {
        let item = self.item(path, "id,file")?;
        let hash = item["file"]["hashes"]["quickXorHash"].as_str();
        let bytes = hash.and_then(|h| base64::engine::general_purpose::STANDARD.decode(h).ok());
        Ok(bytes.filter(|b| b.len() == 20).map(|b| b.iter().map(|x| format!("{x:02x}")).collect()))
    }

    fn writes_whole(&self) -> bool {
        true
    }

    /// Copied by OneDrive itself: it answers at once with a page to watch
    /// until the copy is done. OneDrive gives copies the date they were
    /// made: the originals' dates are put back after, as Finder keeps them.
    fn copy_on_server(&self, from: &Path, to: &Path) -> Option<io::Result<()>> {
        Some(self.copy_and_wait(from, to).map(|()| self.restore_dates(from, to)))
    }

    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, _permissions: Option<u32>) -> io::Result<()> {
        let Some(modified) = modified else { return Ok(()) };
        let body = json!({ "fileSystemInfo": { "lastModifiedDateTime": http::iso_utc(modified) } });
        self.call("PATCH", &self.item_url(path, ""), Some(&body)).map(|_| ())
    }

    fn shares_links(&self) -> bool {
        true
    }

    /// A view-only link anyone can open. Accounts that cannot set when it
    /// expires (personal ones without Microsoft 365) get one that stays
    /// until removed in OneDrive.
    fn share_link(&self, path: &Path, valid_for: Duration) -> io::Result<(String, bool)> {
        let url = self.item_url(path, "/createLink");
        let until = http::iso_utc(SystemTime::now() + valid_for);
        let with_expiry = json!({ "type": "view", "scope": "anonymous", "expirationDateTime": until });
        match self.call("POST", &url, Some(&with_expiry)) {
            Ok(reply) => return link_of(&reply).map(|l| (l, true)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(e),
            Err(_) => {}
        }
        let reply = self.call("POST", &url, Some(&json!({ "type": "view", "scope": "anonymous" })))?;
        link_of(&reply).map(|l| (l, false))
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(OneDriveFs { agent: http::agent(Duration::from_secs(30)), ..self.clone() }))
    }

    fn display_path(&self, path: &Path) -> String {
        format!("onedrive://{}", http::encode_path(&path.to_string_lossy()).trim_start_matches('/'))
    }
}

fn link_of(reply: &Value) -> io::Result<String> {
    reply["link"]["webUrl"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| io::Error::other("OneDrive did not answer with a link."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_errors_and_items() {
        assert_eq!(graph_path(Path::new("/")), "");
        assert_eq!(graph_path(Path::new("/My Files/a#1.txt")), "/My%20Files/a%231.txt");
        let error = graph_error(404, r#"{"error": {"code": "itemNotFound", "message": "Item not found"}}"#);
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let error = graph_error(409, r#"{"error": {"code": "nameAlreadyExists", "message": "..."}}"#);
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        let error = graph_error(400, r#"{"error": {"code": "invalidRequest", "message": "Bad name"}}"#);
        assert!(error.to_string().contains("Bad name"), "{error}");
        let item = json!({"name": "a.txt", "size": 5, "file": {},
            "fileSystemInfo": {"lastModifiedDateTime": "2026-11-04T10:20:30Z"},
            "lastModifiedDateTime": "2026-11-28T10:00:00Z"});
        let e = entry(PathBuf::from("/a.txt"), &item);
        assert_eq!((e.size, e.kind == EntryKind::File), (5, true));
        assert_eq!(e.modified, http::parse_iso("2026-11-04T10:20:30Z"), "the file's own date, not the upload's");
        let folder =
            entry(PathBuf::from("/Notes"), &json!({"name": "Notes", "size": 900, "package": {"type": "oneNote"}}));
        assert_eq!((folder.kind, folder.size), (EntryKind::Folder, 0));
    }
}
