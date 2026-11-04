//! WebDAV over HTTP or HTTPS (macOS's TLS and trust settings), with Basic
//! authentication: PROPFIND to list, GET (with ranges to resume), streamed
//! PUT, MKCOL, MOVE, and DELETE.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use roxmltree::Document;

use super::http::{self, Agent};
use super::sftp::Prompter;
use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

#[derive(Clone)]
pub struct DavParams {
    pub host: String,
    pub port: u16,
    pub secure: bool,
    pub user: String,
    pub password: Option<String>,
    /// The server's WebDAV root ("/" or for example "/remote.php/dav/files/me").
    pub root: String,
    pub timeout: Duration,
}

pub struct DavFs {
    params: DavParams,
    agent: Agent,
    secret: Option<String>,
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

/// Connects (checks the root can be listed), asking for a password if
/// there is none or it is refused.
pub fn connect(params: DavParams, prompter: &dyn Prompter) -> io::Result<Arc<DavFs>> {
    let mut fs = DavFs { agent: http::agent(params.timeout), secret: params.password.clone(), params };
    for attempt in 0..4 {
        match fs.propfind(Path::new("/"), "0") {
            Ok(_) => return Ok(Arc::new(fs)),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && attempt < 3 => {
                let message = format!("Enter the password for {}@{}", fs.params.user, fs.params.host);
                let informative = if fs.secret.is_some() { "The password was not accepted." } else { "" };
                match prompter.secret(&message, informative) {
                    Some(password) => fs.secret = Some(password),
                    None => return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled")),
                }
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} refused the login.", fs.params.host)))
}

impl DavFs {
    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    fn origin(&self) -> String {
        let scheme = if self.params.secure { "https" } else { "http" };
        let default = if self.params.secure { 443 } else { 80 };
        let port = if self.params.port == default { String::new() } else { format!(":{}", self.params.port) };
        format!("{scheme}://{}{port}", self.params.host)
    }

    /// The server path of a path in forkz (under the WebDAV root).
    fn server_path(&self, path: &Path) -> String {
        let root = self.params.root.trim_end_matches('/');
        format!("{root}{}", path.display())
    }

    fn url(&self, path: &Path) -> String {
        format!("{}{}", self.origin(), http::encode_path(&self.server_path(path)))
    }

    fn request(&self, method: &str, path: &Path) -> ureq::http::request::Builder {
        let mut builder = ureq::http::Request::builder().method(method).uri(self.url(path));
        if !self.params.user.is_empty() {
            let credentials = format!("{}:{}", self.params.user, self.secret.as_deref().unwrap_or(""));
            builder = builder.header("Authorization", format!("Basic {}", STANDARD.encode(credentials)));
        }
        builder
    }

    /// Runs a request; an error unless the status is 2xx.
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
        Err(http::status_error(status, &body))
    }

    fn propfind(&self, path: &Path, depth: &str) -> io::Result<Vec<Entry>> {
        let request = self
            .request("PROPFIND", path)
            .header("Depth", depth)
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(PROPFIND_BODY)
            .map_err(io::Error::other)?;
        let text = self.send(request)?.into_body().read_to_string().map_err(http::to_io)?;
        parse_multistatus(&text, &self.params.root)
    }
}

/// The entries of a PROPFIND answer, with paths relative to `root`.
pub fn parse_multistatus(text: &str, root: &str) -> io::Result<Vec<Entry>> {
    let doc = Document::parse(text).map_err(|e| io::Error::other(format!("unreadable WebDAV answer: {e}")))?;
    let root = root.trim_end_matches('/');
    let mut entries = Vec::new();
    for response in doc.descendants().filter(|n| n.tag_name().name() == "response") {
        let Some(href) = response.descendants().find(|n| n.tag_name().name() == "href").and_then(|n| n.text()) else {
            continue;
        };
        // An absolute URL or a path; decoded, without the root.
        let href = match href.find("://") {
            Some(scheme) => href[scheme + 3..].find('/').map_or("/", |slash| &href[scheme + 3 + slash..]),
            None => href,
        };
        let decoded = http::decode(href);
        let relative = decoded.strip_prefix(root).unwrap_or(&decoded);
        let relative = relative.trim_end_matches('/');
        let path = PathBuf::from(if relative.is_empty() { "/" } else { relative });
        let prop = |name: &str| response.descendants().find(|n| n.tag_name().name() == name);
        let folder = prop("resourcetype").is_some_and(|r| r.children().any(|c| c.tag_name().name() == "collection"));
        let size = prop("getcontentlength").and_then(|n| n.text()).and_then(|t| t.trim().parse().ok()).unwrap_or(0);
        let modified =
            prop("getlastmodified").and_then(|n| n.text()).and_then(|t| httpdate::parse_http_date(t.trim()).ok());
        let name = path.file_name().map_or_else(|| "/".to_string(), |n| n.to_string_lossy().into_owned());
        entries.push(Entry {
            hidden: name.starts_with('.'),
            name,
            path,
            kind: if folder { EntryKind::Folder } else { EntryKind::File },
            size: if folder { 0 } else { size },
            modified,
            package: false,
            permissions: None,
            owner: None,
            group: None,
        });
    }
    Ok(entries)
}

impl Vfs for DavFs {
    fn key(&self) -> String {
        format!("{}/{}", self.origin().replace("://", "://dav:"), self.params.user)
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        if self.params.user.is_empty() {
            self.params.host.clone()
        } else {
            format!("{}@{}", self.params.user, self.params.host)
        }
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let entries = self.propfind(dir, "1")?;
        // The folder itself is in the answer too.
        Ok(entries.into_iter().filter(|e| e.path != dir).collect())
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        self.propfind(path, "0")?
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such file or folder."))
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let request = self.request("MKCOL", path).body(()).map_err(io::Error::other)?;
        self.send(request).map(|_| ())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let request = self
            .request("MOVE", from)
            .header("Destination", self.url(to))
            .header("Overwrite", "T")
            .body(())
            .map_err(io::Error::other)?;
        self.send(request).map(|_| ())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        let request = self.request("DELETE", path).body(()).map_err(io::Error::other)?;
        self.send(request).map(|_| ())
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.remove_file(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let request = self.request("GET", path).body(()).map_err(io::Error::other)?;
        Ok(Box::new(self.send(request)?.into_body().into_reader()))
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        let request =
            self.request("GET", path).header("Range", format!("bytes={offset}-")).body(()).map_err(io::Error::other)?;
        let response = self.send(request)?;
        let partial = response.status().as_u16() == 206;
        let mut reader = response.into_body().into_reader();
        if !partial {
            // The server sent all of it: skip what is already here.
            io::copy(&mut (&mut reader).take(offset), &mut io::sink())?;
        }
        Ok(Box::new(reader))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.open_write_sized(path, None)
    }

    fn open_write_sized(&self, path: &Path, size: Option<u64>) -> io::Result<Box<dyn Write + Send>> {
        let mut builder = self.request("PUT", path).header("Content-Type", "application/octet-stream");
        if let Some(size) = size {
            builder = builder.header("Content-Length", size.to_string());
        }
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
        // WebDAV servers do not let clients set dates or permissions.
        Ok(())
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(DavFs {
            params: self.params.clone(),
            agent: http::agent(self.params.timeout),
            secret: self.secret.clone(),
        }))
    }

    fn display_path(&self, path: &Path) -> String {
        format!("{}{}", self.origin(), http::encode_path(&self.server_path(path)))
    }

    fn supports_permissions(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multistatus() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response><d:href>/dav/files/</d:href>
    <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
  <d:response><d:href>https://cloud.example/dav/files/My%20Docs/</d:href>
    <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype>
      <d:getlastmodified>Tue, 03 Nov 2026 10:00:00 GMT</d:getlastmodified></d:prop></d:propstat></d:response>
  <d:response><d:href>/dav/files/report%2B1.pdf</d:href>
    <d:propstat><d:prop><d:resourcetype/><d:getcontentlength>1234</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        let entries = parse_multistatus(xml, "/dav").unwrap();
        let summary: Vec<_> =
            entries.iter().map(|e| (e.path.display().to_string(), e.is_folder_like(), e.size)).collect();
        assert_eq!(
            summary,
            [
                ("/files".into(), true, 0),
                ("/files/My Docs".into(), true, 0),
                ("/files/report+1.pdf".into(), false, 1234)
            ]
        );
        assert!(entries[1].modified.is_some());
    }
}
