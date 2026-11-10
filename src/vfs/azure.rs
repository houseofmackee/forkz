//! Azure Blob Storage, with Shared Key authorization (the storage
//! account's name and one of its access keys). The top level lists the
//! containers; inside a container, "/" in blob names makes folders, as in
//! the Azure portal. Works with Azurite (Microsoft's local emulator) too:
//! for an IP address or localhost the account goes in the path.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use roxmltree::Document;
use sha2::Sha256;

use super::http::{self, Agent};
use super::sftp::Prompter;
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

/// The REST API version used.
const VERSION: &str = "2021-08-06";

/// Larger files would need block lists (not done yet).
const MAX_SINGLE_UPLOAD: u64 = 5000 * 1024 * 1024;

#[derive(Clone)]
pub struct AzureParams {
    /// "account.blob.core.windows.net", or Azurite's "127.0.0.1".
    pub host: String,
    pub port: u16,
    pub secure: bool,
    pub account: String,
    /// The account key (base64), asked for if missing.
    pub key: Option<String>,
    pub timeout: Duration,
}

pub struct AzureFs {
    params: AzureParams,
    agent: Agent,
    key: Option<String>,
}

/// The usual host of an account ("myaccount.blob.core.windows.net").
pub fn account_host(account: &str) -> String {
    format!("{account}.blob.core.windows.net")
}

/// Whether the account goes in the path (Azurite, IP addresses), not the
/// host name.
fn path_style(host: &str) -> bool {
    host == "localhost" || host.parse::<std::net::IpAddr>().is_ok()
}

/// The string signed for Shared Key authorization.
#[allow(clippy::too_many_arguments)]
pub fn string_to_sign(
    method: &str,
    account: &str,
    url_path: &str,
    query: &[(String, String)],
    headers: &[(String, String)],
    content_length: u64,
    range: &str,
) -> String {
    let header =
        |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).unwrap_or("");
    let length = if content_length == 0 { String::new() } else { content_length.to_string() };
    let mut ms: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| k.to_lowercase().starts_with("x-ms-"))
        .map(|(k, v)| (k.to_lowercase(), v.trim().to_string()))
        .collect();
    ms.sort();
    let canonical_headers: String = ms.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let mut params: Vec<(String, String)> = query.iter().map(|(k, v)| (k.to_lowercase(), v.clone())).collect();
    params.sort();
    let mut resource = format!("/{account}{url_path}");
    for (name, value) in params {
        resource.push_str(&format!("\n{name}:{value}"));
    }
    format!(
        "{method}\n{}\n{}\n{length}\n{}\n{}\n\n{}\n{}\n{}\n{}\n{range}\n{canonical_headers}{resource}",
        header("content-encoding"),
        header("content-language"),
        header("content-md5"),
        header("content-type"),
        header("if-modified-since"),
        header("if-match"),
        header("if-none-match"),
        header("if-unmodified-since"),
    )
}

/// The Authorization header for a string to sign.
pub fn signature(account: &str, key_base64: &str, to_sign: &str) -> io::Result<String> {
    let key = STANDARD
        .decode(key_base64.trim())
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "The account key is not valid (it is base64)."))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("any key length");
    mac.update(to_sign.as_bytes());
    Ok(format!("SharedKey {account}:{}", STANDARD.encode(mac.finalize().into_bytes())))
}

/// Connects: lists the containers (which checks the key), asking for the
/// key if it is missing or refused.
pub fn connect(params: AzureParams, prompter: &dyn Prompter) -> io::Result<Arc<AzureFs>> {
    let mut fs = AzureFs { agent: http::agent(params.timeout), key: params.key.clone(), params };
    for attempt in 0..4 {
        if fs.key.is_some() {
            match fs.list(Path::new("/")) {
                Ok(_) => return Ok(Arc::new(fs)),
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempt < 3 => {}
                Err(e) => return Err(e),
            }
        }
        let message = format!("Enter an access key of the storage account \u{201c}{}\u{201d}", fs.params.account);
        let informative = if fs.key.is_some() {
            "The key was not accepted."
        } else {
            "In the Azure portal: the storage account, then Security + networking > Access keys."
        };
        match prompter.secret(&message, informative) {
            Some(key) => fs.key = Some(key),
            None => return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled")),
        }
    }
    Err(io::Error::new(io::ErrorKind::PermissionDenied, "The key was not accepted."))
}

/// A path in forkz as container and blob name ("/photos/2026/a.jpg" is
/// container "photos", blob "2026/a.jpg"). The root has neither.
fn split(path: &Path) -> (Option<String>, String) {
    let text = path.to_string_lossy();
    let trimmed = text.trim_start_matches('/');
    match trimmed.split_once('/') {
        Some((container, blob)) => (Some(container.to_string()), blob.trim_end_matches('/').to_string()),
        None if trimmed.is_empty() => (None, String::new()),
        None => (Some(trimmed.to_string()), String::new()),
    }
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

impl AzureFs {
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    fn host_header(&self) -> String {
        let default = if self.params.secure { 443 } else { 80 };
        if self.params.port == default {
            self.params.host.clone()
        } else {
            format!("{}:{}", self.params.host, self.params.port)
        }
    }

    /// A signed request for "/container/blob" (not yet encoded).
    fn request(
        &self,
        method: &str,
        target: &str,
        query: &[(&str, &str)],
        headers: &[(&str, String)],
        content_length: u64,
    ) -> io::Result<ureq::http::request::Builder> {
        let encoded = http::encode_path(target);
        let url_path =
            if path_style(&self.params.host) { format!("/{}{encoded}", self.params.account) } else { encoded };
        let date = httpdate::fmt_http_date(SystemTime::now());
        let mut all: Vec<(String, String)> = vec![("x-ms-date".into(), date), ("x-ms-version".into(), VERSION.into())];
        all.extend(headers.iter().map(|(k, v)| (k.to_string(), v.clone())));
        let range = all.iter().find(|(k, _)| k == "range").map(|(_, v)| v.clone()).unwrap_or_default();
        let query: Vec<(String, String)> = query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let to_sign = string_to_sign(method, &self.params.account, &url_path, &query, &all, content_length, &range);
        let authorization = signature(&self.params.account, self.key.as_deref().unwrap_or(""), &to_sign)?;
        let scheme = if self.params.secure { "https" } else { "http" };
        let query_text = query
            .iter()
            .map(|(k, v)| format!("{}={}", http::encode_value(k), http::encode_value(v)))
            .collect::<Vec<_>>()
            .join("&");
        let uri = if query_text.is_empty() {
            format!("{scheme}://{}{url_path}", self.host_header())
        } else {
            format!("{scheme}://{}{url_path}?{query_text}", self.host_header())
        };
        let mut builder = ureq::http::Request::builder().method(method).uri(uri).header("Authorization", authorization);
        for (name, value) in all {
            builder = builder.header(name, value);
        }
        if content_length > 0 || matches!(method, "PUT" | "POST") {
            builder = builder.header("Content-Length", content_length.to_string());
        }
        Ok(builder)
    }

    fn send(
        &self,
        request: ureq::http::Request<impl ureq::AsSendBody>,
    ) -> io::Result<ureq::http::Response<ureq::Body>> {
        let response = self.agent.run(request).map_err(http::to_io)?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(response);
        }
        let body = response.into_body().read_to_string().unwrap_or_default();
        // Azure explains in XML: <Error><Code>..</Code><Message>..</Message>.
        let message = Document::parse(body.trim_start_matches('\u{feff}'))
            .ok()
            .and_then(|d| {
                d.descendants().find(|n| n.has_tag_name("Message")).and_then(|n| n.text()).map(str::to_string)
            })
            .map(|m| m.lines().next().unwrap_or_default().to_string())
            .unwrap_or(body);
        Err(http::status_error(status, &message))
    }

    fn get_xml(&self, target: &str, query: &[(&str, &str)]) -> io::Result<String> {
        let request = self.request("GET", target, query, &[], 0)?.body(()).map_err(io::Error::other)?;
        let text = self.send(request)?.into_body().read_to_string().map_err(http::to_io)?;
        Ok(text.trim_start_matches('\u{feff}').to_string())
    }

    fn containers(&self) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut marker = String::new();
        loop {
            let mut query = vec![("comp", "list")];
            if !marker.is_empty() {
                query.push(("marker", &marker));
            }
            let text = self.get_xml("/", &query)?;
            let doc = Document::parse(&text).map_err(io::Error::other)?;
            for container in doc.descendants().filter(|n| n.has_tag_name("Container")) {
                let Some(name) = container.children().find(|c| c.has_tag_name("Name")).and_then(|c| c.text()) else {
                    continue;
                };
                let modified = container
                    .descendants()
                    .find(|c| c.has_tag_name("Last-Modified"))
                    .and_then(|c| c.text())
                    .and_then(|d| httpdate::parse_http_date(d).ok());
                entries.push(entry(PathBuf::from(format!("/{name}")), EntryKind::Folder, 0, modified));
            }
            marker = next_marker(&doc);
            if marker.is_empty() {
                return Ok(entries);
            }
        }
    }

    /// One level of a container: blobs and "folders" (prefixes).
    fn blobs(&self, container: &str, prefix: &str, limit: Option<&str>) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut marker = String::new();
        loop {
            let mut query = vec![("restype", "container"), ("comp", "list"), ("delimiter", "/"), ("prefix", prefix)];
            if !marker.is_empty() {
                query.push(("marker", &marker));
            }
            if let Some(limit) = limit {
                query.push(("maxresults", limit));
            }
            let text = self.get_xml(&format!("/{container}"), &query)?;
            let doc = Document::parse(&text).map_err(io::Error::other)?;
            let child = |node: roxmltree::Node, name: &str| -> Option<String> {
                node.descendants().find(|c| c.has_tag_name(name)).and_then(|c| c.text()).map(str::to_string)
            };
            for folder in doc.descendants().filter(|n| n.has_tag_name("BlobPrefix")) {
                if let Some(name) = child(folder, "Name") {
                    let path = PathBuf::from(format!("/{container}/{}", name.trim_end_matches('/')));
                    entries.push(entry(path, EntryKind::Folder, 0, None));
                }
            }
            for blob in doc.descendants().filter(|n| n.has_tag_name("Blob")) {
                let Some(name) = child(blob, "Name") else { continue };
                // A folder's marker (an empty blob ending in "/").
                if name == prefix || name.ends_with('/') {
                    continue;
                }
                let size = child(blob, "Content-Length").and_then(|s| s.parse().ok()).unwrap_or(0);
                let modified = child(blob, "Last-Modified").and_then(|d| httpdate::parse_http_date(&d).ok());
                entries.push(entry(PathBuf::from(format!("/{container}/{name}")), EntryKind::File, size, modified));
            }
            marker = next_marker(&doc);
            if marker.is_empty() || limit.is_some() {
                return Ok(entries);
            }
        }
    }

    /// Copies a blob within the account (waiting for the copy to finish).
    fn copy_blob(&self, from: &str, to: &str) -> io::Result<()> {
        let source_path = if path_style(&self.params.host) {
            format!("/{}{}", self.params.account, http::encode_path(from))
        } else {
            http::encode_path(from)
        };
        let scheme = if self.params.secure { "https" } else { "http" };
        let source = format!("{scheme}://{}{source_path}", self.host_header());
        let request =
            self.request("PUT", to, &[], &[("x-ms-copy-source", source)], 0)?.body(()).map_err(io::Error::other)?;
        let response = self.send(request)?;
        let mut status =
            response.headers().get("x-ms-copy-status").and_then(|v| v.to_str().ok()).unwrap_or("success").to_string();
        for _ in 0..600 {
            if status != "pending" {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
            let head = self.request("HEAD", to, &[], &[], 0)?.body(()).map_err(io::Error::other)?;
            let response = self.send(head)?;
            status = response.headers().get("x-ms-copy-status").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        }
        if status == "success" || status.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(format!("The copy on the server did not finish ({status}).")))
        }
    }
}

fn next_marker(doc: &Document) -> String {
    doc.descendants().find(|n| n.has_tag_name("NextMarker")).and_then(|n| n.text()).unwrap_or_default().to_string()
}

impl Vfs for AzureFs {
    fn key(&self) -> String {
        format!("azure://{}@{}", self.params.account, self.host_header())
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        self.params.account.clone()
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        match split(dir) {
            (None, _) => self.containers(),
            (Some(container), blob) => {
                let prefix = if blob.is_empty() { String::new() } else { format!("{blob}/") };
                self.blobs(&container, &prefix, None)
            }
        }
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        let (container, blob) = split(path);
        let Some(container) = container else { return Ok(entry(PathBuf::from("/"), EntryKind::Folder, 0, None)) };
        if blob.is_empty() {
            return self
                .containers()?
                .into_iter()
                .find(|c| c.path == path)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such container."));
        }
        let request =
            self.request("HEAD", &format!("/{container}/{blob}"), &[], &[], 0)?.body(()).map_err(io::Error::other)?;
        let response = self.agent.run(request).map_err(http::to_io)?;
        if response.status().is_success() {
            let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
            let size = header("content-length").and_then(|s| s.parse().ok()).unwrap_or(0);
            let modified = header("last-modified").and_then(|d| httpdate::parse_http_date(&d).ok());
            return Ok(entry(path.to_path_buf(), EntryKind::File, size, modified));
        }
        // Not a blob: a folder if anything is under it.
        if !self.blobs(&container, &format!("{blob}/"), Some("1"))?.is_empty() {
            return Ok(entry(path.to_path_buf(), EntryKind::Folder, 0, None));
        }
        let marker =
            self.request("HEAD", &format!("/{container}/{blob}/"), &[], &[], 0)?.body(()).map_err(io::Error::other)?;
        if self.agent.run(marker).map_err(http::to_io)?.status().is_success() {
            Ok(entry(path.to_path_buf(), EntryKind::Folder, 0, None))
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "No such file or folder."))
        }
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let (container, blob) = split(path);
        let Some(container) = container else { return Err(io::Error::other("Choose a container first.")) };
        let request = if blob.is_empty() {
            self.request("PUT", &format!("/{container}"), &[("restype", "container")], &[], 0)?
        } else {
            // A folder is an empty marker blob ending in "/".
            self.request("PUT", &format!("/{container}/{blob}/"), &[], &[("x-ms-blob-type", "BlockBlob".into())], 0)?
        };
        self.send(request.body(()).map_err(io::Error::other)?).map(|_| ())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (Some(from_container), from_blob) = split(from) else {
            return Err(io::Error::other("Containers cannot be renamed."));
        };
        let (Some(to_container), to_blob) = split(to) else {
            return Err(io::Error::other("Containers cannot be renamed."));
        };
        if from_blob.is_empty() || to_blob.is_empty() {
            return Err(io::Error::other("Containers cannot be renamed."));
        }
        if self.stat(from)?.kind == EntryKind::Folder {
            return Err(io::Error::other("Folders cannot be renamed in Azure Blob Storage (only files)."));
        }
        // No rename: a copy on the server, then the original goes.
        self.copy_blob(&format!("/{from_container}/{from_blob}"), &format!("/{to_container}/{to_blob}"))?;
        self.remove_file(from)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let (Some(container), blob) = split(path) else { return Err(io::Error::other("Nothing to delete.")) };
        let request =
            self.request("DELETE", &format!("/{container}/{blob}"), &[], &[], 0)?.body(()).map_err(io::Error::other)?;
        self.send(request).map(|_| ())
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        let (Some(container), blob) = split(path) else { return Err(io::Error::other("Nothing to delete.")) };
        if blob.is_empty() {
            let request = self
                .request("DELETE", &format!("/{container}"), &[("restype", "container")], &[], 0)?
                .body(())
                .map_err(io::Error::other)?;
            return self.send(request).map(|_| ());
        }
        // Its marker, if it has one (a folder with nothing left is gone).
        let request = self
            .request("DELETE", &format!("/{container}/{blob}/"), &[], &[], 0)?
            .body(())
            .map_err(io::Error::other)?;
        match self.send(request) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.open_read_from(path, 0)
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let (Some(container), blob) = split(path) else { return Err(io::Error::other("Not a file.")) };
        let headers: Vec<(&str, String)> =
            if offset > 0 { vec![("range", format!("bytes={offset}-"))] } else { Vec::new() };
        let request = self
            .request("GET", &format!("/{container}/{blob}"), &[], &headers, 0)?
            .body(())
            .map_err(io::Error::other)?;
        Ok(Box::new(self.send(request)?.into_body().into_reader()))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.open_write_sized(path, None)
    }

    fn open_write_sized(&self, path: &Path, size: Option<u64>) -> io::Result<Box<dyn Write + Send>> {
        let (Some(container), blob) = split(path) else { return Err(io::Error::other("Choose a container first.")) };
        if blob.is_empty() {
            return Err(io::Error::other("Files go inside a container."));
        }
        let Some(size) = size else {
            return Err(io::Error::other("Azure Blob Storage needs the size of a file before uploading it."));
        };
        if size > MAX_SINGLE_UPLOAD {
            return Err(io::Error::other("Files over 5,000 MB cannot be uploaded to Azure Blob Storage yet."));
        }
        let builder =
            self.request("PUT", &format!("/{container}/{blob}"), &[], &[("x-ms-blob-type", "BlockBlob".into())], size)?;
        let agent = self.agent.clone();
        Ok(Box::new(http::upload_pipe(move |reader| {
            let request = builder.body(ureq::SendBody::from_owned_reader(reader)).map_err(io::Error::other)?;
            let response = agent.run(request).map_err(http::to_io)?;
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                Ok(())
            } else {
                Err(http::status_error(status, &response.into_body().read_to_string().unwrap_or_default()))
            }
        })))
    }

    fn set_attributes(&self, _path: &Path, _modified: Option<SystemTime>, _permissions: Option<u32>) -> io::Result<()> {
        // Blobs keep the date they were uploaded.
        Ok(())
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(AzureFs {
            params: self.params.clone(),
            agent: http::agent(self.params.timeout),
            key: self.key.clone(),
        }))
    }

    fn display_path(&self, path: &Path) -> String {
        format!("azure://{}{}", self.params.account, path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_like_azure() {
        // The shape Azure documents for a container listing.
        let headers = vec![
            ("x-ms-date".to_string(), "Fri, 26 Jun 2015 23:39:12 GMT".to_string()),
            ("x-ms-version".to_string(), "2015-02-21".to_string()),
        ];
        let query =
            vec![("restype".to_string(), "container".to_string()), ("comp".to_string(), "metadata".to_string())];
        let text = string_to_sign("GET", "myaccount", "/mycontainer", &query, &headers, 0, "");
        assert_eq!(
            text,
            "GET\n\n\n\n\n\n\n\n\n\n\n\nx-ms-date:Fri, 26 Jun 2015 23:39:12 GMT\nx-ms-version:2015-02-21\n/myaccount/mycontainer\ncomp:metadata\nrestype:container"
        );
        // An upload signs its length and type.
        let put = vec![("x-ms-blob-type".to_string(), "BlockBlob".to_string())];
        let text = string_to_sign("PUT", "acct", "/c/a%20b.txt", &[], &put, 12, "");
        assert!(text.starts_with("PUT\n\n\n12\n"));
        assert!(text.ends_with("x-ms-blob-type:BlockBlob\n/acct/c/a%20b.txt"));
        // Azurite's well-known key signs; a key that is not base64 does not.
        let key = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFr/KBHBeksoGMGw==";
        assert!(signature("devstoreaccount1", key, "x").unwrap().starts_with("SharedKey devstoreaccount1:"));
        assert!(signature("a", "not base64!", "x").is_err());
        assert!(path_style("127.0.0.1") && path_style("localhost") && !path_style("acct.blob.core.windows.net"));
        assert_eq!(account_host("acct"), "acct.blob.core.windows.net");
    }
}
