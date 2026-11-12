//! Talks to real FTP/FTPS, WebDAV, and S3 servers. Ignored unless run on
//! purpose; `scripts/test_protocols.sh` starts throwaway local servers
//! (pyftpdlib, WsgiDAV, and moto) and runs these:
//!
//!   FORKZ_TEST_FTP=127.0.0.1:2121 FORKZ_TEST_FTP2=127.0.0.1:2122 \
//!   FORKZ_TEST_FTP_NO_FXP=127.0.0.1:2123 \
//!   FORKZ_TEST_DAV=127.0.0.1:8080 FORKZ_TEST_S3=127.0.0.1:9555 \
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
use forkz::vfs::{self, FsRef, Vfs, azure, boxcom, ftp, s3, webdav};

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

/// Notes whether any file went straight between servers (FXP).
#[derive(Default)]
struct WatchFxp {
    direct: bool,
}

impl Observer for WatchFxp {
    fn progress(&mut self, progress: &Progress) {
        self.direct |= progress.current.ends_with("(server to server)");
    }
    fn conflict(&mut self, _conflict: &Conflict) -> Answer {
        Answer { resolution: Resolution::Replace, apply_to_all: true }
    }
    fn error(&mut self, path: &Path, error: &io::Error) -> ErrorAnswer {
        panic!("{}: {error}", path.display())
    }
}

/// Copies `folder` from one server to another; returns whether FXP did it.
fn server_to_server(kind: JobKind, from: &FsRef, folder: &Path, to: &FsRef, into: &Path) -> bool {
    let mut watch = WatchFxp::default();
    let job = Job::between(kind, from.clone(), vec![folder.to_path_buf()], to.clone(), into.to_path_buf());
    let outcome = ops::run(&job, &mut watch, CancelToken::default());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    watch.direct
}

#[test]
#[ignore = "needs three FTP servers (scripts/test_protocols.sh)"]
fn fxp_between_ftp_servers() {
    let (Some((host, port)), Some((_, port2)), Some((_, port3))) =
        (address("FORKZ_TEST_FTP"), address("FORKZ_TEST_FTP2"), address("FORKZ_TEST_FTP_NO_FXP"))
    else {
        return;
    };
    let _trust = isolated_trust();
    let plain = |port| -> FsRef {
        ftp::connect(ftp_params(&host, port, ftp::Security::Plain, Some("secret")), &answers()).unwrap()
    };
    let (a, b, c) = (plain(port), plain(port2), plain(port3));

    // Something to copy, on the first server.
    let local = tempfile::tempdir().unwrap();
    let src = local.path().join("fxp-set");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    let big: Vec<u8> = (0..4_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(src.join("big.bin"), &big).unwrap();
    std::fs::write(src.join("sub/\u{e4} note.txt"), "fxp").unwrap();
    let _ = a.create_dir(Path::new("/fxp"));
    run(&Job::between(JobKind::Copy, vfs::local(), vec![src.clone()], a.clone(), "/fxp".into()));
    let set = Path::new("/fxp/fxp-set");

    // Straight from server to server, keeping contents and dates.
    let _ = b.create_dir(Path::new("/in"));
    assert!(server_to_server(JobKind::Copy, &a, set, &b, Path::new("/in")), "FXP was not used");
    assert_eq!(read_all(&b, Path::new("/in/fxp-set/big.bin")), big);
    assert_eq!(read_all(&b, Path::new("/in/fxp-set/sub/\u{e4} note.txt")), b"fxp");
    let names_in = names(&b, Path::new("/in/fxp-set"));
    assert!(!names_in.iter().any(|(n, _)| n.contains("forkz-partial")), "{names_in:?}");
    let original = a.stat(&set.join("big.bin")).unwrap().modified.unwrap();
    let copied = b.stat(Path::new("/in/fxp-set/big.bin")).unwrap().modified.unwrap();
    let apart = original.duration_since(copied).unwrap_or_else(|e| e.duration());
    assert!(apart < Duration::from_secs(61), "dates {original:?} and {copied:?}");

    // Within one server (to itself), and moving (the source goes).
    let _ = a.create_dir(Path::new("/fxp-copy"));
    assert!(server_to_server(JobKind::Copy, &a, set, &a, Path::new("/fxp-copy")));
    assert_eq!(read_all(&a, Path::new("/fxp-copy/fxp-set/big.bin")), big);
    let _ = b.create_dir(Path::new("/moved"));
    assert!(server_to_server(JobKind::Move, &a, Path::new("/fxp-copy/fxp-set"), &b, Path::new("/moved")));
    assert_eq!(read_all(&b, Path::new("/moved/fxp-set/big.bin")), big);
    assert!(a.stat(Path::new("/fxp-copy/fxp-set")).is_err(), "moved away");

    // A server that refuses PORT can still receive (it only listens)...
    let _ = c.create_dir(Path::new("/in"));
    assert!(server_to_server(JobKind::Copy, &a, set, &c, Path::new("/in")), "FXP to it");
    // ...but not send: copied through the Mac instead.
    let _ = b.create_dir(Path::new("/back"));
    assert!(!server_to_server(JobKind::Copy, &c, Path::new("/in/fxp-set"), &b, Path::new("/back")), "refused");
    assert_eq!(read_all(&b, Path::new("/back/fxp-set/big.bin")), big);

    // Stop while the servers transfer: both are told to abort, the job
    // ends as stopped, nothing appears under the real name, and the
    // servers still work afterwards.
    struct StopWhenDirect(CancelToken);
    impl Observer for StopWhenDirect {
        fn progress(&mut self, progress: &Progress) {
            if progress.current.ends_with("(server to server)") {
                self.0.cancel();
            }
        }
        fn conflict(&mut self, _conflict: &Conflict) -> Answer {
            Answer { resolution: Resolution::Replace, apply_to_all: true }
        }
        fn error(&mut self, path: &Path, error: &io::Error) -> ErrorAnswer {
            panic!("{}: {error}", path.display())
        }
    }
    let cancel = CancelToken::default();
    let _ = b.create_dir(Path::new("/stopped"));
    let job = Job::between(JobKind::Copy, a.clone(), vec![set.join("big.bin")], b.clone(), "/stopped".into());
    let outcome = ops::run(&job, &mut StopWhenDirect(cancel.clone()), cancel);
    assert!(outcome.cancelled, "{outcome:?}");
    assert!(b.stat(Path::new("/stopped/big.bin")).is_err(), "not under its real name");
    assert!(server_to_server(JobKind::Copy, &a, &set.join("big.bin"), &b, Path::new("/stopped")));
    assert_eq!(read_all(&b, Path::new("/stopped/big.bin")), big);

    // Encrypted ends need SSCN, which pyftpdlib does not have: streamed.
    let tls = |port| -> FsRef {
        ftp::connect(ftp_params(&host, port, ftp::Security::Tls, Some("secret")), &answers()).unwrap()
    };
    let (ta, tb) = (tls(port), tls(port2));
    let _ = tb.create_dir(Path::new("/tls"));
    assert!(!server_to_server(JobKind::Copy, &ta, set, &tb, Path::new("/tls")), "no SSCN");
    assert_eq!(read_all(&tb, Path::new("/tls/fxp-set/big.bin")), big);
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
#[ignore = "needs Azurite (scripts/test_protocols.sh)"]
fn azure_blob() {
    let Some((host, port)) = address("FORKZ_TEST_AZURE") else { return };
    // Azurite's well-known development account.
    let params = azure::AzureParams {
        host,
        port,
        secure: false,
        account: "devstoreaccount1".into(),
        key: Some("Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==".into()),
        timeout: Duration::from_secs(10),
    };
    let fs: FsRef = azure::connect(params, &answers()).unwrap();
    let container = Path::new("/forkz-test");
    fs.create_dir(container).unwrap();
    assert!(names(&fs, Path::new("/")).contains(&("forkz-test".to_string(), true)));
    assert!(fs.stat(container).unwrap().is_folder_like());
    exercise(fs.clone(), &container.join("run"), false);
    // Folders cannot be renamed (blob storage has no folders, only names).
    fs.create_dir(&container.join("f")).unwrap();
    assert!(fs.stat(&container.join("f")).unwrap().is_folder_like());
    assert!(ops::rename(&fs, &container.join("f"), "g").is_err());
    fs.remove_dir(&container.join("f")).unwrap();
    // Moving a folder to another container: copied on the server (Copy
    // Blob), then deleted.
    let other = Path::new("/forkz-other");
    fs.create_dir(other).unwrap();
    let folder = container.join("moving");
    fs.create_dir(&folder).unwrap();
    let mut file = fs.open_write_sized(&folder.join("inside.txt"), Some(6)).unwrap();
    file.write_all(b"inside").unwrap();
    file.flush().unwrap();
    drop(file);
    run(&Job::between(JobKind::Move, fs.clone(), vec![folder.clone()], fs.clone(), other.to_path_buf()));
    assert_eq!(read_all(&fs, &other.join("moving/inside.txt")), b"inside");
    assert_eq!(fs.stat(&folder.join("inside.txt")).unwrap_err().kind(), io::ErrorKind::NotFound);
    run(&Job::between(JobKind::Delete, fs.clone(), vec![other.join("moving")], fs.clone(), other.to_path_buf()));
    fs.remove_dir(other).unwrap();
    fs.remove_dir(container).unwrap();
}

#[test]
#[ignore = "needs the Box stand-in (scripts/test_protocols.sh)"]
fn box_api() {
    let Some((host, port)) = address("FORKZ_TEST_BOX") else { return };
    let base = format!("http://{host}:{port}");
    let saved: std::sync::Arc<Mutex<Vec<String>>> = Default::default();
    let keep = saved.clone();
    let params = boxcom::BoxParams {
        refresh_token: Some("rt-0".into()),
        save_token: Some(std::sync::Arc::new(move |token: &str| keep.lock().unwrap().push(token.to_string()))),
        timeout: Duration::from_secs(10),
        service: boxcom::Service {
            api: format!("{base}/2.0"),
            upload: format!("{base}/api/2.0"),
            token: format!("{base}/oauth2/token"),
            // Files over 1 MB go in parts (the stand-in's parts are 1 MB).
            simple_upload: 1_000_000,
            client: Some(("forkz-test".into(), "test-secret".into())),
        },
        browser: false,
    };
    let fs: FsRef = boxcom::connect(params.clone()).unwrap();
    assert_eq!(fs.label(), "Box: tester@example.com");
    exercise(fs.clone(), Path::new("/box-run"), true);
    // Copies on Box's servers, also over an existing file.
    fs.create_dir(Path::new("/box-copy")).unwrap();
    let mut first = fs.open_write_sized(Path::new("/box-copy/a.txt"), Some(3)).unwrap();
    first.write_all(b"one").unwrap();
    first.flush().unwrap();
    drop(first);
    let mut second = fs.open_write_sized(Path::new("/box-copy/b.txt"), Some(3)).unwrap();
    second.write_all(b"two").unwrap();
    second.flush().unwrap();
    drop(second);
    fs.copy_on_server(Path::new("/box-copy/a.txt"), Path::new("/box-copy/c.txt")).unwrap().unwrap();
    fs.copy_on_server(Path::new("/box-copy/a.txt"), Path::new("/box-copy/b.txt")).unwrap().unwrap();
    assert_eq!(read_all(&fs, Path::new("/box-copy/c.txt")), b"one");
    assert_eq!(read_all(&fs, Path::new("/box-copy/b.txt")), b"one");
    assert_eq!(names(&fs, Path::new("/box-copy")).len(), 3, "no temporary copy left");
    fs.remove_dir(Path::new("/box-copy")).unwrap();
    // Box revoked access tokens along the way; each renewal was kept, and
    // only the last one still works.
    let tokens = saved.lock().unwrap().clone();
    assert!(tokens.len() > 2, "{tokens:?}");
    let stale = boxcom::BoxParams { refresh_token: Some(tokens[0].clone()), ..params.clone() };
    assert_eq!(boxcom::connect(stale).err().map(|e| e.kind()), Some(io::ErrorKind::PermissionDenied));
    let latest = boxcom::BoxParams { refresh_token: tokens.last().cloned(), ..params };
    let again: FsRef = boxcom::connect(latest).unwrap();
    assert!(again.list(Path::new("/")).unwrap().iter().all(|e| e.name != "box-run"));
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
    let s3fs = s3::connect(params, &answers()).unwrap();
    let fs: FsRef = s3fs.clone();
    let bucket = Path::new("/forkz-test");
    fs.create_dir(bucket).unwrap();
    assert!(names(&fs, Path::new("/")).contains(&("forkz-test".to_string(), true)));
    assert!(fs.stat(bucket).unwrap().is_folder_like());
    exercise(fs.clone(), &bucket.join("run"), false);
    // Folders cannot be renamed (S3 has no folders, only names).
    fs.create_dir(&bucket.join("f")).unwrap();
    assert!(ops::rename(&fs, &bucket.join("f"), "g").is_err());
    fs.remove_dir(&bucket.join("f")).unwrap();

    // Moving a folder to another bucket of the same account: copied on the
    // server, then deleted (it cannot be renamed).
    let other = Path::new("/forkz-other");
    fs.create_dir(other).unwrap();
    let folder = bucket.join("moving");
    fs.create_dir(&folder).unwrap();
    let mut file = fs.open_write_sized(&folder.join("inside.txt"), Some(6)).unwrap();
    file.write_all(b"inside").unwrap();
    file.flush().unwrap();
    drop(file);
    run(&Job::between(JobKind::Move, fs.clone(), vec![folder.clone()], fs.clone(), other.to_path_buf()));
    assert_eq!(read_all(&fs, &other.join("moving/inside.txt")), b"inside");
    assert_eq!(fs.stat(&folder.join("inside.txt")).unwrap_err().kind(), io::ErrorKind::NotFound);

    // A copy in parts on the server (as for files over 5 GB), here in 5 MB
    // parts (S3's smallest).
    let big: Vec<u8> = (0..11 * 1024 * 1024u32).map(|i| (i % 253) as u8).collect();
    let mut file = fs.open_write_sized(&bucket.join("big.bin"), Some(big.len() as u64)).unwrap();
    file.write_all(&big).unwrap();
    file.flush().unwrap();
    drop(file);
    s3fs.copy_in_parts_of(&bucket.join("big.bin"), &other.join("big copy.bin"), 5 * 1024 * 1024).unwrap();
    assert_eq!(read_all(&fs, &other.join("big copy.bin")), big);

    run(&Job::between(
        JobKind::Delete,
        fs.clone(),
        vec![other.join("moving"), other.join("big copy.bin")],
        fs.clone(),
        other.to_path_buf(),
    ));
    fs.remove_file(&bucket.join("big.bin")).unwrap();
    fs.remove_dir(other).unwrap();
    fs.remove_dir(bucket).unwrap();
}
