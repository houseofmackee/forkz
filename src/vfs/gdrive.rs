//! Google Drive (My Drive), through its API. Signing in happens in the
//! browser (OAuth with PKCE); forkz keeps the sign-in (a refresh token) in
//! the Keychain for saved sites. Drive finds items by ID, and a folder may
//! hold several items of one name: forkz remembers the IDs of the paths it
//! has listed, and shows a second item of a name as "name (2)". Google
//! Docs, Sheets, and Slides show as Word, Excel, and PowerPoint files and
//! are exported when copied or opened. Deleted items go to Drive's trash.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Value, json};

use super::http::{self, Agent};
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

const FOLDER: &str = "application/vnd.google-apps.folder";
const FIELDS: &str = "id,name,mimeType,size,modifiedTime";

/// Uploads go in pieces of this size (a multiple of 256 KB, as Drive asks).
const CHUNK: usize = 8 * 1024 * 1024;

/// Google's own documents, and what they are exported as: (Drive type,
/// export type, extension shown).
const EXPORTS: [(&str, &str, &str); 4] = [
    (
        "application/vnd.google-apps.document",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docx",
    ),
    (
        "application/vnd.google-apps.spreadsheet",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsx",
    ),
    (
        "application/vnd.google-apps.presentation",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pptx",
    ),
    ("application/vnd.google-apps.drawing", "application/pdf", "pdf"),
];

/// Saves a new sign-in.
pub type SaveToken = Arc<dyn Fn(&str) + Send + Sync>;

/// forkz's app at Google (client ID and secret; for apps on a computer
/// the secret is not secret, Google says, but it is kept out of the code).
fn client() -> io::Result<(String, String)> {
    let id = super::oauth::app_setting("FORKZ_GOOGLE_CLIENT_ID", option_env!("FORKZ_GOOGLE_CLIENT_ID"));
    let secret = super::oauth::app_setting("FORKZ_GOOGLE_CLIENT_SECRET", option_env!("FORKZ_GOOGLE_CLIENT_SECRET"));
    match (id, secret) {
        (Some(id), Some(secret)) => Ok((id, secret)),
        _ => Err(io::Error::other(
            "This copy of forkz cannot sign in to Google Drive: it was built without a Google app (FORKZ_GOOGLE_CLIENT_ID and FORKZ_GOOGLE_CLIENT_SECRET).",
        )),
    }
}

/// Where Google is (a stand-in server for tests).
#[derive(Clone)]
pub struct Service {
    pub api: String,
    pub upload: String,
    pub token: String,
    pub authorize: String,
    pub client: Option<(String, String)>,
    /// The size of upload pieces.
    pub chunk: usize,
}

impl Default for Service {
    fn default() -> Self {
        Service {
            api: "https://www.googleapis.com/drive/v3".into(),
            upload: "https://www.googleapis.com/upload/drive/v3".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            client: None,
            chunk: CHUNK,
        }
    }
}

#[derive(Clone)]
pub struct DriveParams {
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

/// An item seen: its ID, whether it is a folder, and for Google's own
/// documents what they are exported as.
#[derive(Clone)]
struct Known {
    id: String,
    folder: bool,
    export: Option<&'static str>,
}

struct Shared {
    service: Service,
    client: (String, String),
    tokens: Mutex<Tokens>,
    ids: Mutex<HashMap<PathBuf, Known>>,
}

#[derive(Clone)]
pub struct DriveFs {
    shared: Arc<Shared>,
    agent: Agent,
    /// For upload pieces (Drive answers 308 for "go on", not a redirect).
    direct: Agent,
    email: String,
    user: String,
}

fn tokens_from(reply: &Value, refresh: Option<&str>) -> io::Result<Tokens> {
    let access = reply["access_token"].as_str().ok_or_else(|| io::Error::other("Google answered without a token."))?;
    let refresh = reply["refresh_token"].as_str().or(refresh).unwrap_or_default();
    let expires = reply["expires_in"].as_u64().unwrap_or(3600);
    Ok(Tokens {
        access: access.to_string(),
        refresh: refresh.to_string(),
        renew_at: Instant::now() + Duration::from_secs(expires.saturating_sub(60)),
    })
}

/// Connects: renews the saved sign-in, or signs in in the browser.
pub fn connect(params: DriveParams) -> io::Result<Arc<DriveFs>> {
    let client = match params.service.client.clone() {
        Some(client) => client,
        None => client()?,
    };
    let agent = http::agent(params.timeout);
    let service = params.service.clone();
    let renew = |refresh: &str| {
        let reply = super::oauth::token_request(
            &agent,
            "Google",
            &service.token,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", &client.0),
                ("client_secret", &client.1),
            ],
        )?;
        tokens_from(&reply, Some(refresh))
    };
    let sign_in = || -> io::Result<Tokens> {
        // Google allows any port on 127.0.0.1 for apps on a computer.
        let code = super::oauth::sign_in("Google Drive", 0, |redirect, state, challenge| {
            format!(
                "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&code_challenge={challenge}&code_challenge_method=S256&state={state}",
                service.authorize,
                http::encode_value(&client.0),
                http::encode_value(redirect),
                http::encode_value("https://www.googleapis.com/auth/drive"),
            )
        })?;
        let reply = super::oauth::token_request(
            &agent,
            "Google",
            &service.token,
            &[
                ("grant_type", "authorization_code"),
                ("code", &code.code),
                ("client_id", &client.0),
                ("client_secret", &client.1),
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
        None => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Not signed in to Google Drive.")),
    };
    if let Some(save) = &params.save_token {
        save(&tokens.refresh);
    }
    let shared = Arc::new(Shared {
        service: params.service,
        client,
        tokens: Mutex::new(tokens),
        ids: Mutex::new(HashMap::new()),
    });
    let mut fs = DriveFs {
        shared,
        agent,
        direct: http::agent_without_redirects(params.timeout),
        email: String::new(),
        user: String::new(),
    };
    let about = fs.json("GET", &format!("{}/about?fields=user(emailAddress,permissionId)", fs.api()), None)?;
    fs.email = about["user"]["emailAddress"].as_str().unwrap_or_default().to_string();
    fs.user = about["user"]["permissionId"].as_str().unwrap_or_default().to_string();
    Ok(Arc::new(fs))
}

/// An error from Google ({"error": {"code", "message", "errors": [{"reason"}]}}).
fn drive_error(status: u16, body: &str) -> io::Error {
    let reply: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let reason = reply["error"]["errors"][0]["reason"].as_str().unwrap_or_default();
    let message = reply["error"]["message"].as_str().unwrap_or(body.trim());
    match (status, reason) {
        (404, _) => io::Error::new(io::ErrorKind::NotFound, "No such file or folder."),
        (_, "storageQuotaExceeded") => io::Error::new(io::ErrorKind::StorageFull, "The Google Drive is full."),
        (403, "insufficientFilePermissions") => {
            io::Error::new(io::ErrorKind::PermissionDenied, "You may not change this item in Google Drive.")
        }
        _ => http::status_error(status, message),
    }
}

/// The name an item shows: Google's documents with the extension they are
/// exported with.
fn shown_name(name: &str, mime: &str) -> (String, Option<&'static str>) {
    match EXPORTS.iter().find(|(kind, _, _)| *kind == mime) {
        Some((_, export, extension)) => (format!("{name}.{extension}"), Some(*export)),
        None => (name.to_string(), None),
    }
}

/// "report (2).pdf" for the second item of a name in a folder.
fn numbered(name: &str, n: usize) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

fn parent_and_name(path: &Path) -> io::Result<(PathBuf, String)> {
    let parent =
        path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "The top cannot be changed."))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Ok((parent.to_path_buf(), name))
}

impl DriveFs {
    fn api(&self) -> &str {
        &self.shared.service.api
    }

    fn access_token(&self) -> io::Result<String> {
        let mut tokens = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner());
        if Instant::now() >= tokens.renew_at {
            self.renew_locked(&mut tokens)?;
        }
        Ok(tokens.access.clone())
    }

    fn renew_locked(&self, tokens: &mut Tokens) -> io::Result<()> {
        let (id, secret) = &self.shared.client;
        let reply = super::oauth::token_request(
            &self.agent,
            "Google",
            &self.shared.service.token,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &tokens.refresh),
                ("client_id", id),
                ("client_secret", secret),
            ],
        )?;
        *tokens = tokens_from(&reply, Some(&tokens.refresh))?;
        Ok(())
    }

    /// A request with the access token: renewed and tried again once if
    /// refused, and tried again when Google asks to slow down.
    fn send(
        &self,
        agent: &Agent,
        build: &dyn Fn(&str) -> io::Result<ureq::http::Request<Vec<u8>>>,
    ) -> io::Result<ureq::http::Response<ureq::Body>> {
        let mut renewed = false;
        let mut waits = 0;
        loop {
            let token = self.access_token()?;
            let response = agent.run(build(&token)?).map_err(http::to_io)?;
            let status = response.status().as_u16();
            if status == 401 && !renewed {
                renewed = true;
                let mut tokens = self.shared.tokens.lock().unwrap_or_else(|e| e.into_inner());
                if tokens.access == token {
                    self.renew_locked(&mut tokens)?;
                }
                continue;
            }
            if matches!(status, 429 | 500 | 502 | 503) && waits < 5 {
                waits += 1;
                std::thread::sleep(Duration::from_millis(500 << waits));
                continue;
            }
            if status == 403 && waits < 5 {
                // Rate limits come as 403 too: read the reason.
                let text = response.into_body().read_to_string().unwrap_or_default();
                if text.contains("ateLimitExceeded") {
                    waits += 1;
                    std::thread::sleep(Duration::from_millis(500 << waits));
                    continue;
                }
                return Err(drive_error(status, &text));
            }
            if (200..400).contains(&status) {
                return Ok(response);
            }
            let text = response.into_body().read_to_string().unwrap_or_default();
            return Err(drive_error(status, &text));
        }
    }

    fn json(&self, method: &str, url: &str, body: Option<&Value>) -> io::Result<Value> {
        let bytes = match body {
            Some(body) => serde_json::to_vec(body).map_err(io::Error::other)?,
            None => Vec::new(),
        };
        let response = self.send(&self.agent, &|token| {
            let mut request = ureq::http::Request::builder()
                .method(method)
                .uri(url)
                .header("Authorization", format!("Bearer {token}"));
            if body.is_some() {
                request = request.header("Content-Type", "application/json");
            }
            request.body(bytes.clone()).map_err(io::Error::other)
        })?;
        let text = response.into_body().read_to_string().map_err(http::to_io)?;
        if text.trim().is_empty() { Ok(Value::Null) } else { serde_json::from_str(&text).map_err(io::Error::other) }
    }

    fn known(&self, path: &Path) -> Option<Known> {
        self.shared.ids.lock().unwrap_or_else(|e| e.into_inner()).get(path).cloned()
    }

    fn remember(&self, path: &Path, known: Known) {
        self.shared.ids.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_path_buf(), known);
    }

    fn forget(&self, path: &Path) {
        self.shared.ids.lock().unwrap_or_else(|e| e.into_inner()).retain(|p, _| !p.starts_with(path));
    }

    /// The item at a path (listing the folders above it if needed).
    fn id_of(&self, path: &Path) -> io::Result<Known> {
        if path.parent().is_none() {
            return Ok(Known { id: "root".into(), folder: true, export: None });
        }
        if let Some(known) = self.known(path) {
            return Ok(known);
        }
        let (parent, _) = parent_and_name(path)?;
        let above = self.id_of(&parent)?;
        if !above.folder {
            return Err(io::Error::new(io::ErrorKind::NotFound, "No such file or folder."));
        }
        self.items(&parent, &above.id)?;
        self.known(path).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such file or folder."))
    }

    /// Lists a folder (by ID), remembering what is in it.
    fn items(&self, dir: &Path, id: &str) -> io::Result<Vec<Entry>> {
        let mut found = Vec::new();
        let mut page_token: Option<String> = None;
        let query = http::encode_value(&format!("'{id}' in parents and trashed = false"));
        loop {
            let mut url = format!(
                "{}/files?q={query}&fields=nextPageToken,files({FIELDS})&pageSize=1000&orderBy=name",
                self.api()
            );
            if let Some(token) = &page_token {
                url.push_str(&format!("&pageToken={}", http::encode_value(token)));
            }
            let page = self.json("GET", &url, None)?;
            found.extend(page["files"].as_array().cloned().unwrap_or_default());
            page_token = page["nextPageToken"].as_str().map(str::to_string).filter(|t| !t.is_empty());
            if page_token.is_none() {
                break;
            }
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut entries = Vec::new();
        let mut ids = self.shared.ids.lock().unwrap_or_else(|e| e.into_inner());
        ids.retain(|p, _| p.parent() != Some(dir));
        for item in found {
            let mime = item["mimeType"].as_str().unwrap_or_default();
            let folder = mime == FOLDER;
            // Other kinds of Google's own (forms, sites, shortcuts) cannot
            // be copied as files.
            let (name, export) = shown_name(item["name"].as_str().unwrap_or_default(), mime);
            if mime.starts_with("application/vnd.google-apps.") && !folder && export.is_none() {
                continue;
            }
            let count = seen.entry(name.to_lowercase()).or_insert(0);
            *count += 1;
            let name = if *count > 1 { numbered(&name, *count) } else { name };
            let path = dir.join(&name);
            let id = item["id"].as_str().unwrap_or_default().to_string();
            ids.insert(path.clone(), Known { id, folder, export });
            entries.push(Entry {
                hidden: name.starts_with('.'),
                name,
                path,
                kind: if folder { EntryKind::Folder } else { EntryKind::File },
                size: item["size"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
                modified: item["modifiedTime"].as_str().and_then(http::parse_iso),
                package: false,
                permissions: None,
                owner: None,
                group: None,
            });
        }
        Ok(entries)
    }

    /// Moves an item to Drive's trash.
    fn trash(&self, path: &Path) -> io::Result<()> {
        let known = self.id_of(path)?;
        self.json("PATCH", &format!("{}/files/{}", self.api(), known.id), Some(&json!({ "trashed": true })))?;
        self.forget(path);
        Ok(())
    }

    /// Starts a resumable upload (a new file, or a new version of one);
    /// returns where to send the content.
    fn start_upload(&self, path: &Path, size: u64) -> io::Result<String> {
        let (parent, name) = parent_and_name(path)?;
        let existing = match self.id_of(path) {
            Ok(known) if known.folder => {
                return Err(io::Error::new(io::ErrorKind::IsADirectory, "A folder has that name."));
            }
            // A Google document: replaced by a plain file of that name.
            Ok(known) if known.export.is_some() => {
                self.trash(path)?;
                None
            }
            Ok(known) => Some(known.id),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        let upload = &self.shared.service.upload;
        let (method, url, metadata) = match &existing {
            Some(id) => ("PATCH", format!("{upload}/files/{id}?uploadType=resumable&fields={FIELDS}"), json!({})),
            None => {
                let parent_id = self.id_of(&parent)?.id;
                (
                    "POST",
                    format!("{upload}/files?uploadType=resumable&fields={FIELDS}"),
                    json!({ "name": name, "parents": [parent_id] }),
                )
            }
        };
        let bytes = serde_json::to_vec(&metadata).map_err(io::Error::other)?;
        let response = self.send(&self.agent, &|token| {
            ureq::http::Request::builder()
                .method(method)
                .uri(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json; charset=UTF-8")
                .header("X-Upload-Content-Length", size.to_string())
                .body(bytes.clone())
                .map_err(io::Error::other)
        })?;
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| io::Error::other("Google Drive did not start the upload."))?;
        Ok(location.to_string())
    }

    /// Remembers an uploaded file (from the answer to its last piece).
    fn remember_uploaded(&self, path: &Path, text: &str) {
        if let Ok(item) = serde_json::from_str::<Value>(text)
            && let Some(id) = item["id"].as_str()
        {
            self.remember(path, Known { id: id.to_string(), folder: false, export: None });
        }
    }
}

/// Sends a file in pieces as it is written (a resumable upload).
struct Upload {
    fs: DriveFs,
    path: PathBuf,
    session: String,
    total: u64,
    chunk: usize,
    buffer: Vec<u8>,
    offset: u64,
    done: bool,
}

impl Upload {
    fn send_piece(&mut self, data: Vec<u8>) -> io::Result<()> {
        let length = data.len() as u64;
        let range = if length == 0 {
            format!("bytes */{}", self.total)
        } else {
            format!("bytes {}-{}/{}", self.offset, self.offset + length - 1, self.total)
        };
        // The session's address carries the permission: no token needed,
        // but it does no harm.
        let request = ureq::http::Request::builder()
            .method("PUT")
            .uri(&self.session)
            .header("Content-Range", range)
            .header("Content-Length", length.to_string())
            .body(data)
            .map_err(io::Error::other)?;
        let response = self.fs.direct.run(request).map_err(http::to_io)?;
        let status = response.status().as_u16();
        let text = response.into_body().read_to_string().unwrap_or_default();
        match status {
            308 => {}
            200 | 201 => self.fs.remember_uploaded(&self.path, &text),
            _ => return Err(drive_error(status, &text)),
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
        // The last piece goes on flush (it may be smaller).
        while self.buffer.len() > self.chunk {
            let piece: Vec<u8> = self.buffer.drain(..self.chunk).collect();
            self.send_piece(piece)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        let rest = std::mem::take(&mut self.buffer);
        self.send_piece(rest)
    }
}

/// For writes of an unknown size: kept in memory, uploaded on flush.
struct BufferedUpload {
    fs: DriveFs,
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

impl Vfs for DriveFs {
    fn key(&self) -> String {
        format!("gdrive://{}", self.user)
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        if self.email.is_empty() { "Google Drive".into() } else { format!("Google Drive: {}", self.email) }
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let known = self.id_of(dir)?;
        if !known.folder {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "Not a folder."));
        }
        self.items(dir, &known.id)
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        let name = path.file_name().map_or_else(|| "/".to_string(), |n| n.to_string_lossy().into_owned());
        let known = self.id_of(path)?;
        let item = self.json("GET", &format!("{}/files/{}?fields={FIELDS}", self.api(), known.id), None)?;
        Ok(Entry {
            hidden: name.starts_with('.'),
            name,
            path: path.to_path_buf(),
            kind: if known.folder { EntryKind::Folder } else { EntryKind::File },
            size: item["size"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
            modified: item["modifiedTime"].as_str().and_then(http::parse_iso),
            package: false,
            permissions: None,
            owner: None,
            group: None,
        })
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let (parent, name) = parent_and_name(path)?;
        if self.id_of(path).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "An item with that name already exists there."));
        }
        let parent_id = self.id_of(&parent)?.id;
        let body = json!({ "name": name, "mimeType": FOLDER, "parents": [parent_id] });
        let made = self.json("POST", &format!("{}/files?fields=id", self.api()), Some(&body))?;
        let id = made["id"].as_str().unwrap_or_default().to_string();
        self.remember(path, Known { id, folder: true, export: None });
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let known = self.id_of(from)?;
        let (old_parent, _) = parent_and_name(from)?;
        let (new_parent, mut name) = parent_and_name(to)?;
        // A file of that name is replaced (to Drive's trash).
        match self.id_of(to) {
            Ok(there) if there.id != known.id => self.trash(to)?,
            _ => {}
        }
        // Google documents keep their own name (without the extension).
        if let Some(export) = known.export
            && let Some((_, _, extension)) = EXPORTS.iter().find(|(_, e, _)| *e == export)
        {
            name = name.strip_suffix(&format!(".{extension}")).unwrap_or(&name).to_string();
        }
        let mut url = format!("{}/files/{}?fields=id", self.api(), known.id);
        if old_parent != new_parent {
            let (old_id, new_id) = (self.id_of(&old_parent)?.id, self.id_of(&new_parent)?.id);
            url.push_str(&format!("&addParents={new_id}&removeParents={old_id}"));
        }
        self.json("PATCH", &url, Some(&json!({ "name": name })))?;
        self.forget(from);
        self.remember(to, known);
        Ok(())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.trash(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.trash(path)
    }

    fn removes_folders_whole(&self) -> bool {
        true
    }

    fn deleted_items_go(&self) -> Option<&'static str> {
        Some("Google Drive\u{2019}s trash")
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.open_read_from(path, 0)
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let known = self.id_of(path)?;
        if known.folder {
            return Err(io::Error::new(io::ErrorKind::IsADirectory, "A folder."));
        }
        let url = match known.export {
            Some(export) => format!("{}/files/{}/export?mimeType={}", self.api(), known.id, http::encode_value(export)),
            None => format!("{}/files/{}?alt=media", self.api(), known.id),
        };
        // Exports cannot start partway: their first bytes are skipped.
        let ranged = known.export.is_none() && offset > 0;
        let response = self.send(&self.agent, &|token| {
            let mut request = ureq::http::Request::builder()
                .method("GET")
                .uri(&url)
                .header("Authorization", format!("Bearer {token}"));
            if ranged {
                request = request.header("Range", format!("bytes={offset}-"));
            }
            request.body(Vec::new()).map_err(io::Error::other)
        })?;
        let mut reader: Box<dyn Read + Send> = Box::new(response.into_body().into_reader());
        if known.export.is_some() && offset > 0 {
            io::copy(&mut reader.by_ref().take(offset), &mut io::sink())?;
        }
        Ok(reader)
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.open_write_sized(path, None)
    }

    fn open_write_sized(&self, path: &Path, size: Option<u64>) -> io::Result<Box<dyn Write + Send>> {
        let Some(size) = size else {
            return Ok(Box::new(BufferedUpload {
                fs: self.clone(),
                path: path.to_path_buf(),
                data: Vec::new(),
                done: false,
            }));
        };
        let session = self.start_upload(path, size)?;
        Ok(Box::new(Upload {
            fs: self.clone(),
            path: path.to_path_buf(),
            session,
            total: size,
            chunk: self.shared.service.chunk,
            buffer: Vec::new(),
            offset: 0,
            done: false,
        }))
    }

    fn checksum_kind(&self) -> Option<crate::fs::checksum::Kind> {
        Some(crate::fs::checksum::Kind::Md5)
    }

    /// Drive keeps an MD5 of files with contents (not Google's own
    /// documents).
    fn server_checksum(&self, path: &Path) -> io::Result<Option<String>> {
        let known = self.id_of(path)?;
        if known.folder || known.export.is_some() {
            return Ok(None);
        }
        let item = self.json("GET", &format!("{}/files/{}?fields=md5Checksum", self.api(), known.id), None)?;
        Ok(item["md5Checksum"].as_str().map(str::to_ascii_lowercase))
    }

    fn writes_whole(&self) -> bool {
        true
    }

    fn copy_on_server(&self, from: &Path, to: &Path) -> Option<io::Result<()>> {
        let copy = || -> io::Result<()> {
            let known = self.id_of(from)?;
            if known.folder {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "A folder."));
            }
            if let Ok(there) = self.id_of(to)
                && there.id != known.id
            {
                self.trash(to)?;
            }
            let (parent, mut name) = parent_and_name(to)?;
            if let Some(export) = known.export
                && let Some((_, _, extension)) = EXPORTS.iter().find(|(_, e, _)| *e == export)
            {
                name = name.strip_suffix(&format!(".{extension}")).unwrap_or(&name).to_string();
            }
            let parent_id = self.id_of(&parent)?.id;
            let body = json!({ "name": name, "parents": [parent_id] });
            let made = self.json("POST", &format!("{}/files/{}/copy?fields=id", self.api(), known.id), Some(&body))?;
            let id = made["id"].as_str().unwrap_or_default().to_string();
            self.remember(to, Known { id, folder: false, export: known.export });
            Ok(())
        };
        Some(copy())
    }

    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, _permissions: Option<u32>) -> io::Result<()> {
        let Some(modified) = modified else { return Ok(()) };
        let known = self.id_of(path)?;
        let body = json!({ "modifiedTime": http::iso_utc(modified) });
        self.json("PATCH", &format!("{}/files/{}?fields=id", self.api(), known.id), Some(&body))?;
        Ok(())
    }

    fn shares_links(&self) -> bool {
        true
    }

    /// Anyone with the link can view (Drive's links do not expire).
    fn share_link(&self, path: &Path, _valid_for: Duration) -> io::Result<(String, bool)> {
        let known = self.id_of(path)?;
        let body = json!({ "role": "reader", "type": "anyone" });
        self.json("POST", &format!("{}/files/{}/permissions?fields=id", self.api(), known.id), Some(&body))?;
        let item = self.json("GET", &format!("{}/files/{}?fields=webViewLink", self.api(), known.id), None)?;
        let link =
            item["webViewLink"].as_str().ok_or_else(|| io::Error::other("Google Drive did not answer with a link."))?;
        Ok((link.to_string(), false))
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(DriveFs {
            agent: http::agent(Duration::from_secs(30)),
            direct: http::agent_without_redirects(Duration::from_secs(30)),
            ..self.clone()
        }))
    }

    fn display_path(&self, path: &Path) -> String {
        format!("gdrive://{}", http::encode_path(&path.to_string_lossy()).trim_start_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_items() {
        assert_eq!(numbered("report.pdf", 2), "report (2).pdf");
        assert_eq!(numbered("Photos", 3), "Photos (3)");
        assert_eq!(shown_name("Plan", "application/vnd.google-apps.spreadsheet").0, "Plan.xlsx");
        assert_eq!(shown_name("a.txt", "text/plain"), ("a.txt".to_string(), None));
        let error = drive_error(
            403,
            r#"{"error": {"code": 403, "message": "full", "errors": [{"reason": "storageQuotaExceeded"}]}}"#,
        );
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert_eq!(drive_error(404, "{}").kind(), io::ErrorKind::NotFound);
    }
}
