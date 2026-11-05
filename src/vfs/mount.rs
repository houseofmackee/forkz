//! Network shares (SMB, AFP, NFS), mounted the way Finder mounts them:
//! with macOS's NetFS (its sign-in and share-picker dialogs included) at a
//! folder in /Volumes. forkz then works in that folder, through
//! [`MountFs`], which is the Mac's file system confined to the share.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSMutableDictionary, NSString, NSURL};

use super::{FsRef, Vfs};
use crate::fs::Entry;

#[link(name = "NetFS", kind = "framework")]
unsafe extern "C" {
    fn NetFSMountURLSync(
        url: *const c_void,
        mountpath: *const c_void,
        user: *const c_void,
        passwd: *const c_void,
        open_options: *const c_void,
        mount_options: *const c_void,
        mountpoints: *mut *const c_void,
    ) -> c_int;
}

/// What to mount.
#[derive(Debug, Clone)]
pub struct ShareParams {
    /// "smb", "afp", or "nfs".
    pub scheme: &'static str,
    pub host: String,
    /// None for the protocol's usual port.
    pub port: Option<u16>,
    pub user: String,
    pub password: Option<String>,
    /// "Share" or "Share/folder" (NFS: the export path); empty to choose
    /// the share in macOS's dialog (SMB and AFP).
    pub share: String,
}

impl ShareParams {
    /// "smb://user@host:port/Share/folder" (user and port only if set).
    pub fn url(&self) -> String {
        let user =
            if self.user.is_empty() { String::new() } else { format!("{}@", super::http::encode_value(&self.user)) };
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let share = self.share.trim_matches('/');
        let path = if share.is_empty() { String::new() } else { super::http::encode_path(&format!("/{share}")) };
        format!("{}://{user}{}{port}{path}", self.scheme, self.host)
    }
}

/// Mounts the share (asking for what is missing in macOS's dialogs) and
/// returns where it is. A share mounted already is found and used.
pub fn mount(params: &ShareParams) -> io::Result<PathBuf> {
    let url = NSURL::URLWithString(&NSString::from_str(&params.url()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "That server address is not valid."))?;
    let user = (!params.user.is_empty()).then(|| NSString::from_str(&params.user));
    let password = params.password.as_deref().map(NSString::from_str);
    // macOS's sign-in and share dialogs may show.
    let open_options: Retained<NSMutableDictionary<NSString, NSString>> = NSMutableDictionary::new();
    open_options.insert(&*NSString::from_str("UIOption"), &*NSString::from_str("AllowUI"));
    let ptr = |o: Option<&Retained<NSString>>| o.map_or(std::ptr::null(), |s| Retained::as_ptr(s).cast::<c_void>());
    let mut mountpoints: *const c_void = std::ptr::null();
    // SAFETY: the Foundation objects are toll-free bridged to the CF types
    // NetFS takes, and live through the call.
    let status = unsafe {
        NetFSMountURLSync(
            Retained::as_ptr(&url).cast(),
            std::ptr::null(),
            ptr(user.as_ref()),
            ptr(password.as_ref()),
            Retained::as_ptr(&open_options).cast(),
            std::ptr::null(),
            &mut mountpoints,
        )
    };
    let first = if mountpoints.is_null() {
        None
    } else {
        // SAFETY: NetFS returns a retained CFArray of CFStrings.
        let array: Retained<NSArray<NSString>> =
            unsafe { Retained::from_raw(mountpoints as *mut NSArray<NSString>) }.expect("not null");
        array.iter().next().map(|s| PathBuf::from(s.to_string()))
    };
    match (status, first) {
        (0, Some(path)) => Ok(path),
        // Mounted already (by Finder, or before): find where.
        (17, _) | (0, None) => {
            find_mounted(params).ok_or_else(|| io::Error::other("The share is mounted, but forkz cannot find where."))
        }
        (-128, _) => Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled")),
        (status, _) => Err(error(status, params)),
    }
}

fn error(status: c_int, params: &ShareParams) -> io::Error {
    let (kind, text) = match status {
        80 | -6600 if params.password.is_some() => {
            (io::ErrorKind::PermissionDenied, "The user name or password was not accepted.".to_string())
        }
        80 => (io::ErrorKind::PermissionDenied, "The server refused the login.".to_string()),
        2 => (io::ErrorKind::NotFound, "There is no such share on the server.".to_string()),
        13 => (io::ErrorKind::PermissionDenied, "Access to the share was refused.".to_string()),
        60 | 64 | 65 => (io::ErrorKind::TimedOut, "The server cannot be reached.".to_string()),
        61 => (io::ErrorKind::ConnectionRefused, "The server refused the connection.".to_string()),
        -5998 | -6003 => (io::ErrorKind::NotFound, "The server has no shares for this account.".to_string()),
        -6602 => (io::ErrorKind::Other, "The share could not be mounted.".to_string()),
        s if s > 0 => (io::ErrorKind::Other, io::Error::from_raw_os_error(s).to_string()),
        s => (io::ErrorKind::Other, format!("The share could not be mounted (error {s}).")),
    };
    io::Error::new(kind, text)
}

/// Where a share of this server is mounted already, if it is.
fn find_mounted(params: &ShareParams) -> Option<PathBuf> {
    let share = params.share.trim_matches('/').split('/').next().unwrap_or("").to_lowercase();
    let host = params.host.to_lowercase();
    mounts().into_iter().find_map(|(from, on, kind)| {
        let from = from.to_lowercase();
        let kind_matches = match params.scheme {
            "smb" => kind == "smbfs",
            "afp" => kind == "afpfs",
            _ => kind == "nfs",
        };
        // "//user@host/share" (SMB), "afp_xxx@host/share", "host:/export" (NFS).
        let matches = from.contains(&host) && (share.is_empty() || from.trim_end_matches('/').ends_with(&share));
        (kind_matches && matches).then_some(on)
    })
}

/// (mounted from, mounted on, file system kind) for every mount.
fn mounts() -> Vec<(String, PathBuf, String)> {
    let mut list: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo points `list` at its own buffer of `count` items.
    let count = unsafe { libc::getmntinfo(&mut list, libc::MNT_NOWAIT) };
    if count <= 0 || list.is_null() {
        return Vec::new();
    }
    let items = unsafe { std::slice::from_raw_parts(list, count as usize) };
    let text = |chars: &[c_char]| unsafe { CStr::from_ptr(chars.as_ptr()) }.to_string_lossy().into_owned();
    items.iter().map(|m| (text(&m.f_mntfromname), PathBuf::from(text(&m.f_mntonname)), text(&m.f_fstypename))).collect()
}

/// Unmounts a share (as Finder's eject does).
pub fn unmount(mountpoint: &Path) -> io::Result<()> {
    let status = std::process::Command::new("/usr/sbin/diskutil").arg("unmount").arg(mountpoint).output()?;
    if status.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(String::from_utf8_lossy(&status.stderr).trim().to_string()))
    }
}

/// A mounted share: the Mac's file system within the share's folder.
pub struct MountFs {
    local: FsRef,
    root: PathBuf,
    label: String,
    url: String,
}

impl MountFs {
    pub fn new(root: PathBuf, params: &ShareParams) -> Arc<MountFs> {
        let share = params.share.trim_matches('/');
        let label = if share.is_empty() { params.host.clone() } else { format!("{share} on {}", params.host) };
        let url = ShareParams { password: None, ..params.clone() }.url();
        Arc::new(MountFs { local: super::local(), root, label, url })
    }

    pub fn mountpoint(&self) -> &Path {
        &self.root
    }
}

impl Vfs for MountFs {
    fn key(&self) -> String {
        format!("mount:{}", self.root.display())
    }

    fn is_local(&self) -> bool {
        // Real files on this Mac: Quick Look, the Trash, and Finder work.
        true
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn home(&self) -> PathBuf {
        self.root.clone()
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        self.local.list(dir)
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        self.local.stat(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.local.create_dir(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.local.rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.local.remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.local.remove_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.local.open_read(path)
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        self.local.open_read_from(path, offset)
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.local.open_write(path)
    }

    fn open_write_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Write + Send>> {
        self.local.open_write_from(path, offset)
    }

    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, permissions: Option<u32>) -> io::Result<()> {
        self.local.set_attributes(path, modified, permissions)
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(MountFs {
            local: self.local.clone(),
            root: self.root.clone(),
            label: self.label.clone(),
            url: self.url.clone(),
        }))
    }

    fn display_path(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(inside) if inside.as_os_str().is_empty() => self.url.clone(),
            Ok(inside) => {
                format!("{}/{}", self.url.trim_end_matches('/'), super::http::encode_path(&inside.to_string_lossy()))
            }
            Err(_) => path.display().to_string(),
        }
    }

    fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let mut params = ShareParams {
            scheme: "smb",
            host: "nas.local".into(),
            port: None,
            user: "me@example.com".into(),
            password: Some("secret".into()),
            share: "/Photos 2026/trips/".into(),
        };
        assert_eq!(params.url(), "smb://me%40example.com@nas.local/Photos%202026/trips");
        params.user.clear();
        params.share.clear();
        params.port = Some(4445);
        assert_eq!(params.url(), "smb://nas.local:4445");
        let fs = MountFs::new(PathBuf::from("/Volumes/Photos"), &ShareParams { share: "Photos".into(), ..params });
        assert_eq!(fs.display_path(Path::new("/Volumes/Photos/a b.jpg")), "smb://nas.local:4445/Photos/a%20b.jpg");
        assert!(fs.contains(Path::new("/Volumes/Photos/x")) && !fs.contains(Path::new("/Volumes")));
        assert_eq!(fs.label(), "Photos on nas.local");
    }

    #[test]
    fn lists_mounts() {
        // The startup disk is always mounted on "/".
        assert!(mounts().iter().any(|(_, on, _)| on == Path::new("/")));
    }
}
