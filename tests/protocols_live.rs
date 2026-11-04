//! Talks to real FTP/FTPS, WebDAV, and S3 servers. Ignored unless run on
//! purpose; `scripts/test_protocols.sh` starts throwaway local servers
//! (pyftpdlib, WsgiDAV, and moto) and runs these:
//!
//!   FORKZ_TEST_FTP=127.0.0.1:2121 FORKZ_TEST_DAV=127.0.0.1:8080 \
//!   FORKZ_TEST_S3=127.0.0.1:9555 \
//!     cargo test --test protocols_live -- --ignored --test-threads=1
//!
//! Every server has the user "forkz" with the password "secret" (any keys
//! work with moto). The WebDAV share is at /dav.

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use forkz::fs::ops::{self, Answer, CancelToken, Conflict, ErrorAnswer, Job, JobKind, Observer, Progress, Resolution};
use forkz::vfs::known_hosts::Status;
use forkz::vfs::sftp::Prompter;
use forkz::vfs::{self, FsRef, Vfs, ftp, s3, webdav};

/// Trusts certificates and gives the password when asked.
struct Answers {
    password: &'static str,
    asked_certificate: Mutex<Vec<Status>>,
    asked_secret: Mutex<usize>,
}

impl Prompter for Answers {
    fn host_key(&self, _host: &str, _port: u16, key_type: &str, fingerprint: &str, status: Status) -> bool {
        assert_eq!(key_type, "TLS certificate");
        assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
        self.asked_certificate.lock().unwrap().push(status);
        true
    }
    fn secret(&self, _message: &str, _informative: &str) -> Option<String> {
        *self.asked_secret.lock().unwrap() += 1;
        Some(self.password.to_string())
    }
}

fn answers() -> Answers {
    Answers { password: "secret", asked_certificate: Mutex::new(Vec::new()), asked_secret: Mutex::new(0) }
}

struct Quiet;
impl Observer for Quiet {
    fn progress(&mut self, _progress: &Progress) {}
    fn conflict(&mut self, _conflict: &Conflict) -> Answer {
        Answer { resolution: Resolution::Replace, apply_to_all: true }
    }
    fn error(&mut self, path: &Path, error: &io::Error) -> ErrorAnswer {
        panic!("{}: {error}", path.display())
    }
}

fn address(variable: &str) -> Option<(String, u16)> {
    let address = std::env::var(variable).ok()?;
    let (host, port) = address.rsplit_once(':').expect("host:port");
    Some((host.to_string(), port.parse().unwrap()))
}

/// Keeps trusted certificates away from the real ones.
fn isolated_trust() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: tests run with --test-threads=1.
    unsafe { std::env::set_var("FORKZ_KNOWN_HOSTS", dir.path().join("known_hosts")) };
    dir
}

fn read_all(fs: &FsRef, path: &Path) -> Vec<u8> {
    let mut data = Vec::new();
    fs.open_read(path).unwrap().read_to_end(&mut data).unwrap();
    data
}

fn names(fs: &FsRef, dir: &Path) -> Vec<(String, bool)> {
    let mut names: Vec<_> = fs.list(dir).unwrap().iter().map(|e| (e.name.clone(), e.is_folder_like())).collect();
    names.sort();
    names
}

fn run(job: &Job) {
    let outcome = ops::run(job, &mut Quiet, CancelToken::default());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
}

/// Everything forkz does with a server, through its file system alone.
/// `base` is a new folder for the run; `folders_rename` is false for S3.
fn exercise(remote: FsRef, base: &Path, folders_rename: bool) {
    let local = tempfile::tempdir().unwrap();
    let src = local.path().join("project");
    std::fs::create_dir_all(src.join("assets/img")).unwrap();
    std::fs::create_dir_all(src.join("my docs")).unwrap();
    std::fs::write(src.join("index.html"), "<h1>hello</h1>").unwrap();
    std::fs::write(src.join("my docs/\u{e4} b+c.txt"), "unicode and spaces").unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.join("assets/img/big.bin"), &big).unwrap();

    remote.create_dir(base).unwrap();
    run(&Job::between(JobKind::Copy, vfs::local(), vec![src.clone()], remote.clone(), base.to_path_buf()));
    let project = base.join("project");
    assert_eq!(
        names(&remote, &project),
        [("assets".to_string(), true), ("index.html".to_string(), false), ("my docs".to_string(), true)]
    );
    assert_eq!(names(&remote, &project.join("my docs")), [("\u{e4} b+c.txt".to_string(), false)]);
    let stat = remote.stat(&project.join("assets/img/big.bin")).unwrap();
    assert_eq!((stat.size, stat.is_folder_like()), (3_000_000, false));
    assert!(remote.stat(&project.join("assets")).unwrap().is_folder_like());
    assert_eq!(remote.stat(&project.join("nothing")).unwrap_err().kind(), io::ErrorKind::NotFound);

    // Uploading again replaces (the observer answers Replace).
    std::fs::write(src.join("index.html"), "<h1>changed</h1>").unwrap();
    run(&Job::between(JobKind::Copy, vfs::local(), vec![src.join("index.html")], remote.clone(), project.clone()));
    assert_eq!(read_all(&remote, &project.join("index.html")), b"<h1>changed</h1>");

    let back = local.path().join("back");
    std::fs::create_dir(&back).unwrap();
    run(&Job::between(JobKind::Copy, remote.clone(), vec![project.clone()], vfs::local(), back.clone()));
    assert_eq!(std::fs::read(back.join("project/assets/img/big.bin")).unwrap(), big);
    assert_eq!(std::fs::read_to_string(back.join("project/my docs/\u{e4} b+c.txt")).unwrap(), "unicode and spaces");

    // Resume. A download continues from its partial file here (ranged
    // reads); an upload continues where the server allows appending and
    // starts over where it does not. Either way the result is whole.
    let resume_src = local.path().join("resume.bin");
    let resume_data: Vec<u8> = (0..1_500_000u32).map(|i| (i % 241) as u8).collect();
    std::fs::write(&resume_src, &resume_data).unwrap();
    let old = SystemTime::now() - Duration::from_secs(600);
    std::fs::File::options().write(true).open(&resume_src).unwrap().set_modified(old).unwrap();
    let mut partial = remote.open_write_sized(&base.join(".resume.bin.forkz-partial"), Some(500_000)).unwrap();
    partial.write_all(&resume_data[..500_000]).unwrap();
    partial.flush().unwrap();
    drop(partial);
    run(&Job::between(JobKind::Copy, vfs::local(), vec![resume_src.clone()], remote.clone(), base.to_path_buf()));
    assert_eq!(read_all(&remote, &base.join("resume.bin")), resume_data);
    assert!(!names(&remote, base).iter().any(|(n, _)| n.contains("forkz-partial")));
    let down = local.path().join("down");
    std::fs::create_dir(&down).unwrap();
    std::fs::write(down.join(".resume.bin.forkz-partial"), &resume_data[..700_000]).unwrap();
    run(&Job::between(JobKind::Copy, remote.clone(), vec![base.join("resume.bin")], vfs::local(), down.clone()));
    assert_eq!(std::fs::read(down.join("resume.bin")).unwrap(), resume_data);
    let mut tail = Vec::new();
    remote.open_read_from(&base.join("resume.bin"), 1_499_990).unwrap().read_to_end(&mut tail).unwrap();
    assert_eq!(tail, resume_data[1_499_990..]);

    // A copy within the server (reads and writes at once).
    let copies = base.join("copies");
    remote.create_dir(&copies).unwrap();
    run(&Job::between(JobKind::Copy, remote.clone(), vec![project.join("index.html")], remote.clone(), copies.clone()));
    assert_eq!(read_all(&remote, &copies.join("index.html")), b"<h1>changed</h1>");

    // Renames and moves.
    let renamed = ops::rename(&remote, &copies.join("index.html"), "home.html").unwrap();
    assert_eq!(read_all(&remote, &renamed), b"<h1>changed</h1>");
    assert_eq!(names(&remote, &copies), [("home.html".to_string(), false)]);
    run(&Job::between(JobKind::Move, remote.clone(), vec![renamed.clone()], remote.clone(), base.to_path_buf()));
    assert!(names(&remote, base).contains(&("home.html".to_string(), false)));
    assert!(names(&remote, &copies).is_empty());
    if folders_rename {
        let site = ops::rename(&remote, &project, "site").unwrap();
        assert!(remote.stat(&site).unwrap().is_folder_like());
        assert_eq!(read_all(&remote, &site.join("assets/img/big.bin")).len(), 3_000_000);
    }

    // Deleting removes folders with what is in them.
    run(&Job::between(JobKind::Delete, remote.clone(), vec![base.to_path_buf()], remote.clone(), base.to_path_buf()));
    let parent = base.parent().unwrap();
    let gone = base.file_name().unwrap().to_string_lossy().into_owned();
    assert!(!names(&remote, parent).iter().any(|(n, _)| *n == gone), "{base:?} is still there");
}

fn ftp_params(host: &str, port: u16, security: ftp::Security, password: Option<&str>) -> ftp::FtpParams {
    ftp::FtpParams {
        host: host.to_string(),
        port,
        user: "forkz".into(),
        password: password.map(str::to_string),
        security,
        timeout: Duration::from_secs(10),
    }
}

#[test]
#[ignore = "needs an FTP server (scripts/test_protocols.sh)"]
fn ftp_and_ftps() {
    let Some((host, port)) = address("FORKZ_TEST_FTP") else { return };
    let _trust = isolated_trust();

    // TLS when offered: the self-signed certificate is asked about once,
    // then remembered. No password yet: asked for.
    let prompter = answers();
    let fs = ftp::connect(ftp_params(&host, port, ftp::Security::TlsIfAvailable, None), &prompter).unwrap();
    assert_eq!(*prompter.asked_certificate.lock().unwrap(), [Status::Unknown]);
    assert_eq!(*prompter.asked_secret.lock().unwrap(), 1);
    assert!(fs.display_path(Path::new("/a")).starts_with("ftps://"));
    let prompter = answers();
    let fs = ftp::connect(ftp_params(&host, port, ftp::Security::Tls, Some("secret")), &prompter).unwrap();
    assert!(prompter.asked_certificate.lock().unwrap().is_empty(), "remembered");
    assert_eq!(*prompter.asked_secret.lock().unwrap(), 0);
    let fs: FsRef = fs;
    exercise(fs.clone(), Path::new("/ftps-run"), true);

    // Permissions where the server takes SITE CHMOD.
    assert!(fs.supports_permissions());

    // Plain FTP too.
    let plain: FsRef = ftp::connect(ftp_params(&host, port, ftp::Security::Plain, Some("secret")), &answers()).unwrap();
    assert!(plain.display_path(Path::new("/a")).starts_with("ftp://"));
    exercise(plain, Path::new("/ftp-run"), true);

    // A wrong password, then cancelling, is refused.
    struct Wrong;
    impl Prompter for Wrong {
        fn host_key(&self, _: &str, _: u16, _: &str, _: &str, _: Status) -> bool {
            true
        }
        fn secret(&self, _: &str, _: &str) -> Option<String> {
            None
        }
    }
    let error = ftp::connect(ftp_params(&host, port, ftp::Security::Plain, Some("wrong")), &Wrong).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
}

#[test]
#[ignore = "needs a WebDAV server (scripts/test_protocols.sh)"]
fn webdav() {
    let Some((host, port)) = address("FORKZ_TEST_DAV") else { return };
    let params = webdav::DavParams {
        host,
        port,
        secure: false,
        user: "forkz".into(),
        password: None,
        root: "/dav".into(),
        timeout: Duration::from_secs(10),
    };
    let prompter = answers();
    let fs = webdav::connect(params, &prompter).unwrap();
    assert_eq!(*prompter.asked_secret.lock().unwrap(), 1);
    let fs: FsRef = fs;
    assert!(!fs.supports_permissions());
    exercise(fs, Path::new("/dav-run"), true);
}

#[test]
#[ignore = "needs an S3 server (scripts/test_protocols.sh)"]
fn s3() {
    let Some((host, port)) = address("FORKZ_TEST_S3") else { return };
    let params = s3::S3Params {
        host,
        port,
        secure: false,
        access_key: "forkz".into(),
        secret_key: Some("secret".into()),
        region: "us-east-1".into(),
        timeout: Duration::from_secs(10),
    };
    let fs: FsRef = s3::connect(params, &answers()).unwrap();
    let bucket = Path::new("/forkz-test");
    fs.create_dir(bucket).unwrap();
    assert!(names(&fs, Path::new("/")).contains(&("forkz-test".to_string(), true)));
    assert!(fs.stat(bucket).unwrap().is_folder_like());
    exercise(fs.clone(), &bucket.join("run"), false);
    // Folders cannot be renamed (S3 has no folders, only names).
    fs.create_dir(&bucket.join("f")).unwrap();
    assert!(ops::rename(&fs, &bucket.join("f"), "g").is_err());
    fs.remove_dir(&bucket.join("f")).unwrap();
    fs.remove_dir(bucket).unwrap();
}
