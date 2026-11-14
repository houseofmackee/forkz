//! Amazon S3 and S3-compatible storage (Backblaze B2, Cloudflare R2, MinIO,
//! Wasabi, and others), with AWS Signature Version 4. The top level lists
//! the buckets; inside a bucket, "/" in keys makes folders. Path-style
//! requests (https://endpoint/bucket/key) work with every provider.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use hmac::{Hmac, KeyInit, Mac};
use roxmltree::Document;
use sha2::{Digest, Sha256};

use super::http::{self, Agent};
use super::sftp::Prompter;
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

#[derive(Clone)]
pub struct S3Params {
    /// The endpoint host ("s3.eu-central-1.amazonaws.com", "localhost").
    pub host: String,
    pub port: u16,
    pub secure: bool,
    pub access_key: String,
    pub secret_key: Option<String>,
    pub region: String,
    pub timeout: Duration,
    /// The bucket to check the keys with (keys for one bucket may not list
    /// all of them); None: the list of buckets.
    pub check: Option<String>,
}

pub struct S3Fs {
    params: S3Params,
    agent: Agent,
    secret: Option<String>,
}

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// S3 copies up to this much in one request; larger files are copied in
/// parts (still on the server).
const SINGLE_COPY: u64 = 5 * 1024 * 1024 * 1024;

/// The size of each part of a copy in parts (S3 allows 10,000 parts).
const COPY_PART: u64 = 512 * 1024 * 1024;

/// The parts of a copy of `size` bytes: (first byte, last byte), each at
/// most `part` (more if 10,000 would not do).
pub fn copy_ranges(size: u64, part: u64) -> Vec<(u64, u64)> {
    let part = part.max(size.div_ceil(10_000)).max(1);
    (0..size.div_ceil(part)).map(|i| (i * part, ((i + 1) * part).min(size) - 1)).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// The Authorization header of a request (AWS Signature Version 4).
#[allow(clippy::too_many_arguments)]
pub fn authorization(
    access_key: &str,
    secret_key: &str,
    region: &str,
    method: &str,
    host: &str,
    path: &str,
    query: &[(String, String)],
    extra_headers: &[(String, String)],
    payload_hash: &str,
    now: SystemTime,
) -> (String, String) {
    let (date, stamp) = http::utc_stamp(now);
    let mut query: Vec<(String, String)> =
        query.iter().map(|(k, v)| (http::encode_value(k), http::encode_value(v))).collect();
    query.sort();
    let canonical_query = query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let mut headers: Vec<(String, String)> = vec![
        ("host".into(), host.to_string()),
        ("x-amz-content-sha256".into(), payload_hash.to_string()),
        ("x-amz-date".into(), stamp.clone()),
    ];
    headers.extend(extra_headers.iter().map(|(k, v)| (k.to_lowercase(), v.trim().to_string())));
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical_request =
        format!("{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}", hex(&Sha256::digest(canonical_request.as_bytes())));
    let key = hmac(&hmac(&hmac(&hmac(format!("AWS4{secret_key}").as_bytes(), &date), region), "s3"), "aws4_request");
    let signature = hex(&hmac(&key, &string_to_sign));
    (
        format!(
            "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        ),
        stamp,
    )
}

/// Connects: lists the buckets (which checks the keys), asking for the
/// secret key if it is missing or refused.
pub fn connect(params: S3Params, prompter: &dyn Prompter) -> io::Result<Arc<S3Fs>> {
    let mut fs = S3Fs { agent: http::agent(params.timeout), secret: params.secret_key.clone(), params };
    for attempt in 0..4 {
        if fs.secret.is_some() {
            let check = fs.params.check.as_ref().map_or_else(|| PathBuf::from("/"), |b| PathBuf::from(format!("/{b}")));
            match fs.list(&check) {
                Ok(_) => return Ok(Arc::new(fs)),
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempt < 3 => {}
                Err(e) => return Err(e),
            }
        }
        let message = format!("Enter the secret key for {}", fs.params.access_key);
        let informative = if fs.secret.is_some() { "The keys were not accepted." } else { "" };
        match prompter.secret(&message, informative) {
            Some(secret) => fs.secret = Some(secret),
            None => return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled")),
        }
    }
    Err(io::Error::new(io::ErrorKind::PermissionDenied, "The keys were not accepted."))
}

/// The region an AWS bucket is in: AWS tells anyone (no keys needed), in
/// a header of any answer about the bucket.
pub fn bucket_region(bucket: &str, timeout: Duration) -> Option<String> {
    let agent = http::agent_without_redirects(timeout);
    let url = format!("https://s3.amazonaws.com/{}", http::encode_value(bucket));
    let response = agent.head(&url).call().ok()?;
    let region = response.headers().get("x-amz-bucket-region")?.to_str().ok()?.trim().to_string();
    // Letters, digits, and dashes ("eu-west-2"): it goes into a host name.
    (!region.is_empty() && region.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).then_some(region)
}

/// A path in forkz as bucket and key ("/photos/2026/a.jpg" is bucket
/// "photos", key "2026/a.jpg"). The root has neither.
fn split(path: &Path) -> (Option<String>, String) {
    let text = path.to_string_lossy();
    let trimmed = text.trim_start_matches('/');
    match trimmed.split_once('/') {
        Some((bucket, key)) => (Some(bucket.to_string()), key.trim_end_matches('/').to_string()),
        None if trimmed.is_empty() => (None, String::new()),
        None => (Some(trimmed.to_string()), String::new()),
    }
}

impl S3Fs {
    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    fn host_header(&self) -> String {
        let default = if self.params.secure { 443 } else { 80 };
        if self.params.port == default {
            self.params.host.clone()
        } else {
            format!("{}:{}", self.params.host, self.params.port)
        }
    }

    /// A signed request. `path` is "/bucket/key" (URL-encoded here).
    fn request(
        &self,
        method: &str,
        bucket_and_key: &str,
        query: &[(&str, &str)],
        headers: &[(&str, String)],
        payload_hash: &str,
    ) -> ureq::http::request::Builder {
        let path = http::encode_path(bucket_and_key);
        let query: Vec<(String, String)> = query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let headers: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let (authorization, stamp) = authorization(
            &self.params.access_key,
            self.secret.as_deref().unwrap_or(""),
            &self.params.region,
            method,
            &self.host_header(),
            &path,
            &query,
            &headers,
            payload_hash,
            SystemTime::now(),
        );
        let scheme = if self.params.secure { "https" } else { "http" };
        let query_text = query
            .iter()
            .map(|(k, v)| format!("{}={}", http::encode_value(k), http::encode_value(v)))
            .collect::<Vec<_>>()
            .join("&");
        let uri = if query_text.is_empty() {
            format!("{scheme}://{}{path}", self.host_header())
        } else {
            format!("{scheme}://{}{path}?{query_text}", self.host_header())
        };
        let mut builder = ureq::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", authorization)
            .header("x-amz-date", stamp)
            .header("x-amz-content-sha256", payload_hash);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        builder
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
        // S3 explains in XML: <Error><Code>..</Code><Message>..</Message>.
        let message = Document::parse(&body)
            .ok()
            .and_then(|d| {
                d.descendants().find(|n| n.has_tag_name("Message")).and_then(|n| n.text()).map(str::to_string)
            })
            .unwrap_or(body);
        Err(http::status_error(status, &message))
    }

    fn get_xml(&self, bucket_and_key: &str, query: &[(&str, &str)]) -> io::Result<String> {
        let request =
            self.request("GET", bucket_and_key, query, &[], EMPTY_SHA256).body(()).map_err(io::Error::other)?;
        self.send(request)?.into_body().read_to_string().map_err(http::to_io)
    }

    fn buckets(&self) -> io::Result<Vec<Entry>> {
        let text = self.get_xml("/", &[])?;
        let doc = Document::parse(&text).map_err(io::Error::other)?;
        Ok(doc
            .descendants()
            .filter(|n| n.has_tag_name("Bucket"))
            .filter_map(|bucket| {
                let name = bucket.children().find(|c| c.has_tag_name("Name"))?.text()?.to_string();
                let created = bucket
                    .children()
                    .find(|c| c.has_tag_name("CreationDate"))
                    .and_then(|c| c.text())
                    .and_then(parse_iso);
                Some(folder_entry(PathBuf::from(format!("/{name}")), created))
            })
            .collect())
    }

    /// One level of a bucket: objects and "folders" (common prefixes).
    fn objects(&self, bucket: &str, prefix: &str, limit: Option<&str>) -> io::Result<Vec<Entry>> {
        self.list_objects(bucket, prefix, limit, false)
    }

    /// Objects under a prefix: one level (with folders), or (`flat`) all
    /// of them at any depth, folders' markers included, in far fewer
    /// requests.
    fn list_objects(&self, bucket: &str, prefix: &str, limit: Option<&str>, flat: bool) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query: Vec<(&str, &str)> = vec![("list-type", "2"), ("prefix", prefix)];
            if !flat {
                query.push(("delimiter", "/"));
            }
            if let Some(token) = &token {
                query.push(("continuation-token", token));
            }
            if let Some(limit) = limit {
                query.push(("max-keys", limit));
            }
            let text = self.get_xml(&format!("/{bucket}"), &query)?;
            let doc = Document::parse(&text).map_err(io::Error::other)?;
            let child_text = |node: roxmltree::Node, name: &str| -> Option<String> {
                node.children().find(|c| c.has_tag_name(name)).and_then(|c| c.text()).map(str::to_string)
            };
            for prefix_node in doc.descendants().filter(|n| n.has_tag_name("CommonPrefixes")) {
                if let Some(folder) = child_text(prefix_node, "Prefix") {
                    entries
                        .push(folder_entry(PathBuf::from(format!("/{bucket}/{}", folder.trim_end_matches('/'))), None));
                }
            }
            for object in doc.descendants().filter(|n| n.has_tag_name("Contents")) {
                let Some(key) = child_text(object, "Key") else { continue };
                // The marker of the folder being listed.
                if key == prefix {
                    continue;
                }
                if key.ends_with('/') {
                    // Deeper folders' markers count when listing it all.
                    if flat {
                        entries.push(folder_entry(
                            PathBuf::from(format!("/{bucket}/{}", key.trim_end_matches('/'))),
                            None,
                        ));
                    }
                    continue;
                }
                let size = child_text(object, "Size").and_then(|s| s.parse().ok()).unwrap_or(0);
                let modified = child_text(object, "LastModified").as_deref().and_then(parse_iso);
                let path = PathBuf::from(format!("/{bucket}/{key}"));
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                entries.push(Entry {
                    hidden: name.starts_with('.'),
                    name,
                    path,
                    kind: EntryKind::File,
                    size,
                    modified,
                    package: false,
                    permissions: None,
                    owner: None,
                    group: None,
                });
            }
            let truncated =
                doc.descendants().find(|n| n.has_tag_name("IsTruncated")).and_then(|n| n.text()) == Some("true");
            token = doc
                .descendants()
                .find(|n| n.has_tag_name("NextContinuationToken"))
                .and_then(|n| n.text())
                .map(str::to_string);
            if !truncated || token.is_none() || limit.is_some() {
                break;
            }
        }
        Ok(entries)
    }
}

fn folder_entry(path: PathBuf, modified: Option<SystemTime>) -> Entry {
    let name = path.file_name().map_or_else(|| "/".to_string(), |n| n.to_string_lossy().into_owned());
    Entry {
        hidden: name.starts_with('.'),
        name,
        path,
        kind: EntryKind::Folder,
        size: 0,
        modified,
        package: false,
        permissions: None,
        owner: None,
        group: None,
    }
}

use super::http::parse_iso;

/// Text for XML (an ETag has quotes).
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

impl S3Fs {
    /// A copy request's answer: S3 can answer 200 and report an error in
    /// the body (when the copy fails while it runs).
    fn copy_answer(&self, request: ureq::http::Request<()>) -> io::Result<String> {
        let text = self.send(request)?.into_body().read_to_string().map_err(http::to_io)?;
        if let Ok(doc) = Document::parse(&text)
            && doc.root_element().has_tag_name("Error")
        {
            let message = doc.descendants().find(|n| n.has_tag_name("Message")).and_then(|n| n.text()).unwrap_or("");
            return Err(io::Error::other(format!("The copy on the server failed: {message}")));
        }
        Ok(text)
    }

    /// Copies an object on the server: in one request, or in parts if it
    /// is larger than S3 allows for one (Google copies any size at once).
    fn copy_object(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (Some(from_bucket), from_key) = split(from) else { return Err(io::Error::other("Not a file.")) };
        let (Some(to_bucket), to_key) = split(to) else { return Err(io::Error::other("Choose a bucket first.")) };
        if from_key.is_empty() || to_key.is_empty() {
            return Err(io::Error::other("Not a file."));
        }
        let source = http::encode_path(&format!("/{from_bucket}/{from_key}"));
        let target = format!("/{to_bucket}/{to_key}");
        let size = self.stat(from)?.size;
        if size <= SINGLE_COPY || self.params.host.ends_with("googleapis.com") {
            let request = self
                .request("PUT", &target, &[], &[("x-amz-copy-source", source)], EMPTY_SHA256)
                .body(())
                .map_err(io::Error::other)?;
            return self.copy_answer(request).map(|_| ());
        }
        self.copy_in_parts(&source, &target, size, COPY_PART)
    }

    /// A copy in parts of this part size (for tests: real copies only use
    /// parts above 5 GB).
    #[doc(hidden)]
    pub fn copy_in_parts_of(&self, from: &Path, to: &Path, part: u64) -> io::Result<()> {
        let (Some(from_bucket), from_key) = split(from) else { return Err(io::Error::other("Not a file.")) };
        let source = http::encode_path(&format!("/{from_bucket}/{from_key}"));
        let size = self.stat(from)?.size;
        self.copy_in_parts(&source, &to.to_string_lossy(), size, part)
    }

    /// A multipart upload whose parts are copied from `source` on the
    /// server; abandoned (not left to be billed) if anything fails.
    fn copy_in_parts(&self, source: &str, target: &str, size: u64, part: u64) -> io::Result<()> {
        let start =
            self.request("POST", target, &[("uploads", "")], &[], EMPTY_SHA256).body(()).map_err(io::Error::other)?;
        let text = self.send(start)?.into_body().read_to_string().map_err(http::to_io)?;
        let doc = Document::parse(&text).map_err(io::Error::other)?;
        let upload = doc
            .descendants()
            .find(|n| n.has_tag_name("UploadId"))
            .and_then(|n| n.text())
            .ok_or_else(|| io::Error::other("S3 did not start the copy."))?
            .to_string();
        let result = (|| {
            let mut parts = String::new();
            for (index, (first, last)) in copy_ranges(size, part).into_iter().enumerate() {
                let number = (index + 1).to_string();
                let request = self
                    .request(
                        "PUT",
                        target,
                        &[("partNumber", &number), ("uploadId", &upload)],
                        &[
                            ("x-amz-copy-source", source.to_string()),
                            ("x-amz-copy-source-range", format!("bytes={first}-{last}")),
                        ],
                        EMPTY_SHA256,
                    )
                    .body(())
                    .map_err(io::Error::other)?;
                let answer = self.copy_answer(request)?;
                let doc = Document::parse(&answer).map_err(io::Error::other)?;
                let etag = doc
                    .descendants()
                    .find(|n| n.has_tag_name("ETag"))
                    .and_then(|n| n.text())
                    .ok_or_else(|| io::Error::other("S3 did not confirm a part of the copy."))?;
                parts.push_str(&format!(
                    "<Part><PartNumber>{number}</PartNumber><ETag>{}</ETag></Part>",
                    xml_escape(etag)
                ));
            }
            let body = format!("<CompleteMultipartUpload>{parts}</CompleteMultipartUpload>");
            let hash = hex(&Sha256::digest(body.as_bytes()));
            let request = self
                .request("POST", target, &[("uploadId", &upload)], &[], &hash)
                .body(body.into_bytes())
                .map_err(io::Error::other)?;
            let text = self.send(request)?.into_body().read_to_string().map_err(http::to_io)?;
            if text.contains("<Error>") {
                return Err(io::Error::other("S3 could not put the copied parts together."));
            }
            Ok(())
        })();
        if result.is_err() {
            let abort = self.request("DELETE", target, &[("uploadId", &upload)], &[], EMPTY_SHA256).body(());
            if let Ok(abort) = abort {
                let _ = self.send(abort);
            }
        }
        result
    }
}

impl Vfs for S3Fs {
    fn key(&self) -> String {
        format!("s3://{}@{}", self.params.access_key, self.host_header())
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        self.params.host.clone()
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        match split(dir) {
            (None, _) => self.buckets(),
            (Some(bucket), key) => {
                let prefix = if key.is_empty() { String::new() } else { format!("{key}/") };
                self.objects(&bucket, &prefix, None)
            }
        }
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        let (bucket, key) = split(path);
        let Some(bucket) = bucket else { return Ok(folder_entry(PathBuf::from("/"), None)) };
        if key.is_empty() {
            return self
                .buckets()?
                .into_iter()
                .find(|b| b.path == path)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such bucket."));
        }
        let request = self
            .request("HEAD", &format!("/{bucket}/{key}"), &[], &[], EMPTY_SHA256)
            .body(())
            .map_err(io::Error::other)?;
        let response = self.agent.run(request).map_err(http::to_io)?;
        if response.status().is_success() {
            let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            return Ok(Entry {
                hidden: name.starts_with('.'),
                name,
                path: path.to_path_buf(),
                kind: EntryKind::File,
                size: header("content-length").and_then(|s| s.parse().ok()).unwrap_or(0),
                modified: header("last-modified").and_then(|d| httpdate::parse_http_date(&d).ok()),
                package: false,
                permissions: None,
                owner: None,
                group: None,
            });
        }
        // Not an object: a folder if anything is under it (or its marker).
        let under = self.objects(&bucket, &format!("{key}/"), Some("1"))?;
        let marker = self
            .request("HEAD", &format!("/{bucket}/{key}/"), &[], &[], EMPTY_SHA256)
            .body(())
            .ok()
            .and_then(|r| self.agent.run(r).ok())
            .is_some_and(|r| r.status().is_success());
        if !under.is_empty() || marker {
            Ok(folder_entry(path.to_path_buf(), None))
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "No such file or folder."))
        }
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let (bucket, key) = split(path);
        let Some(bucket) = bucket else { return Err(io::Error::other("Choose a bucket first.")) };
        let target = if key.is_empty() { format!("/{bucket}") } else { format!("/{bucket}/{key}/") };
        // A new bucket outside us-east-1 says where it lives.
        let body = if key.is_empty() && self.params.region != "us-east-1" {
            format!(
                "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
                self.params.region
            )
        } else {
            String::new()
        };
        let hash = hex(&Sha256::digest(body.as_bytes()));
        let request = self.request("PUT", &target, &[], &[], &hash).body(body).map_err(io::Error::other)?;
        self.send(request).map(|_| ())
    }

    fn copy_on_server(&self, from: &Path, to: &Path) -> Option<io::Result<()>> {
        Some(self.copy_object(from, to))
    }

    fn list_tree(&self, folder: &Path) -> Option<io::Result<Vec<Entry>>> {
        let (Some(bucket), key) = split(folder) else { return None };
        let prefix = if key.is_empty() { String::new() } else { format!("{key}/") };
        Some(self.list_objects(&bucket, &prefix, None, true))
    }

    fn renames_folders(&self) -> bool {
        false
    }

    fn writes_whole(&self) -> bool {
        true
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (Some(from_bucket), from_key) = split(from) else {
            return Err(io::Error::other("Buckets cannot be renamed."));
        };
        let (Some(to_bucket), to_key) = split(to) else { return Err(io::Error::other("Buckets cannot be renamed.")) };
        if from_key.is_empty() || to_key.is_empty() {
            return Err(io::Error::other("Buckets cannot be renamed."));
        }
        if self.stat(from)?.kind == EntryKind::Folder {
            return Err(io::Error::other("Folders cannot be renamed on S3 (only files)."));
        }
        // S3 has no rename: a copy on the server, then the original goes.
        let source = http::encode_path(&format!("/{from_bucket}/{from_key}"));
        let request = self
            .request("PUT", &format!("/{to_bucket}/{to_key}"), &[], &[("x-amz-copy-source", source)], EMPTY_SHA256)
            .body(())
            .map_err(io::Error::other)?;
        self.send(request)?;
        self.remove_file(from)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let (Some(bucket), key) = split(path) else { return Err(io::Error::other("Nothing to delete.")) };
        let request = self
            .request("DELETE", &format!("/{bucket}/{key}"), &[], &[], EMPTY_SHA256)
            .body(())
            .map_err(io::Error::other)?;
        self.send(request).map(|_| ())
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        let (Some(bucket), key) = split(path) else { return Err(io::Error::other("Nothing to delete.")) };
        let target = if key.is_empty() { format!("/{bucket}") } else { format!("/{bucket}/{key}/") };
        let request = self.request("DELETE", &target, &[], &[], EMPTY_SHA256).body(()).map_err(io::Error::other)?;
        self.send(request).map(|_| ())
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.open_read_from(path, 0)
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let (Some(bucket), key) = split(path) else { return Err(io::Error::other("Not a file.")) };
        let headers: Vec<(&str, String)> =
            if offset > 0 { vec![("range", format!("bytes={offset}-"))] } else { Vec::new() };
        let request = self
            .request("GET", &format!("/{bucket}/{key}"), &[], &headers, EMPTY_SHA256)
            .body(())
            .map_err(io::Error::other)?;
        Ok(Box::new(self.send(request)?.into_body().into_reader()))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.open_write_sized(path, None)
    }

    fn open_write_sized(&self, path: &Path, size: Option<u64>) -> io::Result<Box<dyn Write + Send>> {
        let (Some(bucket), key) = split(path) else { return Err(io::Error::other("Choose a bucket first.")) };
        if key.is_empty() {
            return Err(io::Error::other("Files go inside a bucket."));
        }
        let Some(size) = size else {
            return Err(io::Error::other("S3 needs the size of a file before uploading it."));
        };
        if size > 5_000_000_000 {
            return Err(io::Error::other("Files over 5 GB cannot be uploaded to S3 yet."));
        }
        // The body is streamed, so it is not part of the signature.
        let builder = self.request(
            "PUT",
            &format!("/{bucket}/{key}"),
            &[],
            &[("content-length", size.to_string())],
            "UNSIGNED-PAYLOAD",
        );
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
        // Objects keep the date they were uploaded.
        Ok(())
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(S3Fs {
            params: self.params.clone(),
            agent: http::agent(self.params.timeout),
            secret: self.secret.clone(),
        }))
    }

    fn display_path(&self, path: &Path) -> String {
        let port = if self.params.port == if self.params.secure { 443 } else { 80 } {
            String::new()
        } else {
            format!(":{}", self.params.port)
        };
        format!(
            "s3://{}@{}{port}{}",
            self.params.access_key,
            self.params.host,
            http::encode_path(&path.to_string_lossy())
        )
    }

    fn supports_permissions(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS tells a bucket's region (needs the network: run with --ignored).
    #[test]
    #[ignore = "needs the network"]
    fn finds_a_buckets_region() {
        assert_eq!(bucket_region("commoncrawl", Duration::from_secs(10)).as_deref(), Some("us-east-1"));
    }

    #[test]
    fn copies_in_parts() {
        assert_eq!(copy_ranges(10, 4), [(0, 3), (4, 7), (8, 9)]);
        assert_eq!(copy_ranges(8, 4), [(0, 3), (4, 7)]);
        assert_eq!(copy_ranges(1, 4), [(0, 0)]);
        // Never more than 10,000 parts: they grow instead.
        let big = 6 * 1024 * 1024 * 1024 * 1024u64;
        let parts = copy_ranges(big, COPY_PART);
        assert!(parts.len() <= 10_000);
        assert_eq!(parts.last().unwrap().1, big - 1);
        assert!(parts.windows(2).all(|w| w[0].1 + 1 == w[1].0));
        assert_eq!(xml_escape("\"abc\""), "&quot;abc&quot;");
    }

    #[test]
    fn splits_paths() {
        assert_eq!(split(Path::new("/")), (None, String::new()));
        assert_eq!(split(Path::new("/photos")), (Some("photos".into()), String::new()));
        assert_eq!(split(Path::new("/photos/2026/a.jpg")), (Some("photos".into()), "2026/a.jpg".into()));
        assert_eq!(split(Path::new("/photos/2026/")), (Some("photos".into()), "2026".into()));
    }

    #[test]
    fn dates() {
        let time = parse_iso("2026-11-04T10:20:30.000Z").unwrap();
        assert_eq!(http::utc_stamp(time).1, "20261104T102030Z");
        assert_eq!(parse_iso("garbage"), None);
    }

    /// AWS's own example (S3 docs, "Signature Calculations for the
    /// Authorization Header": GET Object with a range).
    #[test]
    fn signature_matches_aws_example() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_369_353_600); // 2013-05-24T00:00:00Z
        let (header, stamp) = authorization(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "GET",
            "examplebucket.s3.amazonaws.com",
            "/test.txt",
            &[],
            &[("range".into(), "bytes=0-9".into())],
            EMPTY_SHA256,
            now,
        );
        assert_eq!(stamp, "20130524T000000Z");
        assert_eq!(
            header,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }
}
