//! Talks to a real SSH server. Ignored unless run on purpose:
//!
//!   FORKZ_TEST_SFTP=127.0.0.1:2222 FORKZ_TEST_KEY=/path/to/key \
//!   FORKZ_TEST_KEY_PASS=/path/to/encrypted_key FORKZ_TEST_REMOTE=/tmp/dir \
//!     cargo test --test sftp_live -- --ignored --test-threads=1
//!
//! FORKZ_TEST_REMOTE must be a folder on the server that is also readable
//! here (the test server runs on this Mac), to check results directly.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use forkz::fs::ops::{self, Answer, CancelToken, Conflict, ErrorAnswer, Job, JobKind, Observer, Progress, Resolution};
use forkz::vfs::known_hosts::Status;
use forkz::vfs::sftp::{self, AuthMethod, ConnectParams, Prompter};
use forkz::vfs::{self, FsRef};

struct Answers {
    passphrase: Option<String>,
    trust: bool,
    asked_host: Mutex<Vec<Status>>,
    asked_secret: Mutex<usize>,
}

impl Prompter for Answers {
    fn host_key(&self, _host: &str, _port: u16, key_type: &str, fingerprint: &str, status: Status) -> bool {
        assert_eq!(key_type, "ssh-ed25519");
        assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
        self.asked_host.lock().unwrap().push(status);
        self.trust
    }
    fn secret(&self, _message: &str, _informative: &str) -> Option<String> {
        *self.asked_secret.lock().unwrap() += 1;
        self.passphrase.clone()
    }
}

fn answers(passphrase: Option<&str>, trust: bool) -> Answers {
    Answers {
        passphrase: passphrase.map(str::to_string),
        trust,
        asked_host: Mutex::new(Vec::new()),
        asked_secret: Mutex::new(0),
    }
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

struct Setup {
    host: String,
    port: u16,
    key: PathBuf,
    key_pass: PathBuf,
    remote: PathBuf,
    _known_hosts: tempfile::TempDir,
}

fn setup() -> Option<Setup> {
    let address = std::env::var("FORKZ_TEST_SFTP").ok()?;
    let (host, port) = address.rsplit_once(':').expect("host:port");
    let known_hosts = tempfile::tempdir().unwrap();
    // SAFETY: tests run with --test-threads=1.
    unsafe { std::env::set_var("FORKZ_KNOWN_HOSTS", known_hosts.path().join("known_hosts")) };
    Some(Setup {
        host: host.to_string(),
        port: port.parse().unwrap(),
        key: std::env::var("FORKZ_TEST_KEY").unwrap().into(),
        key_pass: std::env::var("FORKZ_TEST_KEY_PASS").unwrap().into(),
        remote: std::env::var("FORKZ_TEST_REMOTE").unwrap().into(),
        _known_hosts: known_hosts,
    })
}

fn params(setup: &Setup, key: &Path) -> ConnectParams {
    ConnectParams {
        host: setup.host.clone(),
        port: setup.port,
        user: std::env::var("USER").unwrap(),
        auth: AuthMethod::Key,
        key_file: Some(key.to_path_buf()),
        password: None,
        timeout: Duration::from_secs(10),
        extra_keys: Vec::new(),
        identities_only: false,
        proxy: None,
    }
}

#[test]
#[ignore]
fn host_keys_passphrases_and_transfers() {
    let Some(setup) = setup() else { return };

    // Unknown host, refused: no connection, nothing trusted.
    let refuse = answers(None, false);
    let error = sftp::connect(params(&setup, &setup.key), &refuse).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(*refuse.asked_host.lock().unwrap(), [Status::Unknown]);

    // Trusted once, then not asked again.
    let trust = answers(None, true);
    sftp::connect(params(&setup, &setup.key), &trust).unwrap();
    let again = answers(None, false);
    let fs = sftp::connect(params(&setup, &setup.key), &again).unwrap();
    assert!(again.asked_host.lock().unwrap().is_empty());

    // An encrypted key: asked for the passphrase; a wrong one is asked again.
    let pass = answers(Some("hunter2"), false);
    let with_pass = sftp::connect(params(&setup, &setup.key_pass), &pass).unwrap();
    assert_eq!(*pass.asked_secret.lock().unwrap(), 1);
    assert_eq!(with_pass.secret(), Some("hunter2"));
    let wrong = answers(Some("nope"), false);
    assert!(sftp::connect(params(&setup, &setup.key_pass), &wrong).is_err());
    assert_eq!(*wrong.asked_secret.lock().unwrap(), 3);

    // Upload a tree, download it back, rename and delete on the server.
    let remote: FsRef = fs;
    let base = setup.remote.join(format!("run-{}", std::process::id()));
    remote.create_dir(&base).unwrap();
    let local = tempfile::tempdir().unwrap();
    let src = local.path().join("project");
    std::fs::create_dir_all(src.join("assets/img")).unwrap();
    std::fs::write(src.join("index.html"), "<h1>hi</h1>").unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(src.join("assets/img/big.bin"), &big).unwrap();

    let upload = Job::between(JobKind::Copy, vfs::local(), vec![src.clone()], remote.clone(), base.clone());
    let outcome = ops::run(&upload, &mut Quiet, CancelToken::default());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(std::fs::read(base.join("project/assets/img/big.bin")).unwrap(), big);

    let listing = remote.list(&base.join("project")).unwrap();
    let mut names: Vec<_> = listing.iter().map(|e| (e.name.clone(), e.is_folder_like())).collect();
    names.sort();
    assert_eq!(names, [("assets".to_string(), true), ("index.html".to_string(), false)]);

    // Uploading again replaces (the observer answers Replace).
    std::fs::write(src.join("index.html"), "<h1>changed</h1>").unwrap();
    let again =
        Job::between(JobKind::Copy, vfs::local(), vec![src.join("index.html")], remote.clone(), base.join("project"));
    ops::run(&again, &mut Quiet, CancelToken::default());
    assert_eq!(std::fs::read_to_string(base.join("project/index.html")).unwrap(), "<h1>changed</h1>");

    let back = local.path().join("back");
    std::fs::create_dir(&back).unwrap();
    let download = Job::between(JobKind::Copy, remote.clone(), vec![base.join("project")], vfs::local(), back.clone());
    let outcome = ops::run(&download, &mut Quiet, CancelToken::default());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(std::fs::read(back.join("project/assets/img/big.bin")).unwrap(), big);

    // Resume: an upload that stopped part way continues from its partial
    // file on the server, and a download from its partial file here.
    let resume_src = local.path().join("resume.bin");
    let resume_data: Vec<u8> = (0..1_500_000u32).map(|i| (i % 241) as u8).collect();
    std::fs::write(&resume_src, &resume_data).unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(600);
    std::fs::File::options().write(true).open(&resume_src).unwrap().set_modified(old).unwrap();
    std::fs::write(base.join(".resume.bin.forkz-partial"), &resume_data[..500_000]).unwrap();
    let up = Job::between(JobKind::Copy, vfs::local(), vec![resume_src.clone()], remote.clone(), base.clone());
    let outcome = ops::run(&up, &mut Quiet, CancelToken::default());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(std::fs::read(base.join("resume.bin")).unwrap(), resume_data);
    let down_folder = local.path().join("down");
    std::fs::create_dir(&down_folder).unwrap();
    std::fs::write(down_folder.join(".resume.bin.forkz-partial"), &resume_data[..700_000]).unwrap();
    let down =
        Job::between(JobKind::Copy, remote.clone(), vec![base.join("resume.bin")], vfs::local(), down_folder.clone());
    let outcome = ops::run(&down, &mut Quiet, CancelToken::default());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert_eq!(std::fs::read(down_folder.join("resume.bin")).unwrap(), resume_data);
    std::fs::remove_file(base.join("resume.bin")).unwrap();

    let renamed = ops::rename(&remote, &base.join("project"), "site").unwrap();
    assert!(remote.stat(&renamed).unwrap().is_folder_like());
    let folder = ops::new_folder(&remote, &base).unwrap();
    assert_eq!(folder, base.join("untitled folder"));

    // A second connection for jobs works with the remembered credentials.
    let other = remote.connect_another().unwrap();
    assert_eq!(other.list(&base).unwrap().len(), 2);

    let delete = Job::between(JobKind::Delete, remote.clone(), vec![base.clone()], remote.clone(), PathBuf::new());
    let outcome = ops::run(&delete, &mut Quiet, CancelToken::default());
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    assert!(!base.exists());
}

#[test]
#[ignore]
fn changed_host_key_is_reported() {
    let Some(setup) = setup() else { return };
    // Trust some other key for this host first.
    let mut fake = (11u32).to_be_bytes().to_vec();
    fake.extend_from_slice(b"ssh-ed25519");
    fake.extend_from_slice(&[7u8; 36]);
    vfs::known_hosts::trust(&setup.host, setup.port, &fake).unwrap();
    let refuse = answers(None, false);
    assert!(sftp::connect(params(&setup, &setup.key), &refuse).is_err());
    assert_eq!(*refuse.asked_host.lock().unwrap(), [Status::Changed]);
    // Trusting the new key replaces the old one.
    let trust = answers(None, true);
    sftp::connect(params(&setup, &setup.key), &trust).unwrap();
    let quiet = answers(None, false);
    sftp::connect(params(&setup, &setup.key), &quiet).unwrap();
    assert!(quiet.asked_host.lock().unwrap().is_empty());
}
