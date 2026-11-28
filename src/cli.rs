//! forkz on the command line, with no window: `forkz copy`, `forkz sync`,
//! and `forkz synclet`. They use the saved sites, their passwords in the
//! Keychain, and the servers' keys forkz trusts, as the app does.
//!
//! A place is a path on the Mac, `SITE:PATH` for a saved site (by name),
//! or an address such as `sftp://user@host/path`.

use std::collections::HashMap;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::fs::ops::{self, Answer, CancelToken, Conflict, ErrorAnswer, Job, JobKind, Observer, Progress, Resolution};
use crate::fs::sync::{self, Action, Direction};
use crate::settings::{Settings, Synclet, WorkspaceTab};
use crate::sites::{Protocol, Site, SiteStore};
use crate::vfs::known_hosts::Status;
use crate::vfs::{self, FsRef};

pub const USAGE: &str = "\
Without a window:
  forkz copy SOURCE... DESTINATION     Copy files and folders into the
                                       DESTINATION folder (made if missing);
                                       files there are replaced, folders merged
  forkz sync SOURCE DESTINATION        Make DESTINATION like SOURCE: copy what
                                       is new or changed
      --delete                         also delete what only DESTINATION has
      --hidden                         include hidden items (.htaccess)
      --exclude 'PATTERNS'             names to leave out, separated by ;
                                       (the default is .DS_Store)
      --dry-run                        show what would change, change nothing
  forkz synclet NAME [--dry-run]       Run a saved synclet
      --watch                          keep running: sync again a few
                                       seconds after files change in its
                                       folder on the Mac

  A place is a path on the Mac, SITE:PATH for a saved site (by its name, as
  in mmg-staging.com:/var/www), or an address (sftp://user@host/path). A
  relative PATH starts in the site's remote folder. Passwords come from the
  Keychain; without one, forkz asks in the terminal, or reads it from
  standard input with --password-stdin.
";

/// The commands this module runs.
pub const COMMANDS: [&str; 3] = ["copy", "sync", "synclet"];

/// Runs a command (`args` starts with its name). Returns the exit code:
/// 0 when everything worked, 1 when something failed, 2 for a mistake in
/// the command.
pub fn run(args: &[String]) -> u8 {
    let (command, rest) = match args.split_first() {
        Some((command, rest)) => (command.as_str(), rest),
        None => return usage("no command"),
    };
    let options = match Options::parse(command, rest) {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let mut cli = match Cli::new(&options) {
        Ok(cli) => cli,
        Err(message) => return fail(&message),
    };
    let result = match command {
        "copy" => cli.copy(&options),
        "sync" => cli.sync(&options),
        _ => cli.synclet(&options),
    };
    match result {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(message) => fail(&message),
    }
}

fn usage(message: &str) -> u8 {
    eprintln!("forkz: {message}\n\n{USAGE}");
    2
}

fn fail(message: &str) -> u8 {
    eprintln!("forkz: {message}");
    1
}

/// What was typed after the command.
#[derive(Debug, Default, PartialEq)]
struct Options {
    places: Vec<String>,
    delete: bool,
    hidden: bool,
    exclude: Option<String>,
    dry_run: bool,
    password_stdin: bool,
    watch: bool,
}

impl Options {
    fn parse(command: &str, args: &[String]) -> Result<Options, String> {
        let mut options = Options::default();
        let mut args = args.iter();
        let mut only_places = false;
        while let Some(arg) = args.next() {
            if only_places || !arg.starts_with("--") {
                options.places.push(arg.clone());
                continue;
            }
            let sync_only = |name: &str| {
                if command == "sync" { Ok(()) } else { Err(format!("{name} is for forkz sync")) }
            };
            match arg.as_str() {
                "--" => only_places = true,
                "--dry-run" if command != "copy" => options.dry_run = true,
                "--password-stdin" => options.password_stdin = true,
                "--watch" if command == "synclet" => options.watch = true,
                "--delete" => {
                    sync_only(arg)?;
                    options.delete = true;
                }
                "--hidden" => {
                    sync_only(arg)?;
                    options.hidden = true;
                }
                "--exclude" => {
                    sync_only(arg)?;
                    options.exclude = Some(args.next().ok_or("--exclude needs patterns")?.clone());
                }
                other => return Err(format!("unknown option \"{other}\"")),
            }
        }
        let count = options.places.len();
        match command {
            "copy" if count < 2 => Err("copy needs a source and a destination".into()),
            "sync" if count != 2 => Err("sync needs a source and a destination".into()),
            "synclet" if count != 1 => Err("synclet needs the name of one synclet".into()),
            "synclet" if options.watch && options.dry_run => Err("--watch and --dry-run do not go together".into()),
            "copy" | "sync" | "synclet" => Ok(options),
            other => Err(format!("unknown command \"{other}\"")),
        }
    }
}

/// A place, as typed: on the Mac, or on a site.
#[derive(Debug, Clone, PartialEq)]
enum Place {
    Local(PathBuf),
    /// A site, and the path on it ("" for its remote folder).
    Remote(Box<Site>, String),
}

/// Reads a place: an address, `SITE:PATH` (a saved site's name, matched
/// without regard to case), or a path on the Mac.
fn parse_place(text: &str, sites: &SiteStore) -> Result<Place, String> {
    if let Some((scheme, rest)) = text.split_once("://")
        && let Some(protocol) = Protocol::from_scheme(scheme)
    {
        return address_place(protocol, rest, sites);
    }
    if let Some((name, path)) = text.split_once(':')
        && let Some(site) = sites.sites.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    {
        return Ok(Place::Remote(Box::new(site.clone()), path.to_string()));
    }
    Ok(Place::Local(std::path::absolute(text).map_err(|e| format!("{text}: {e}"))?))
}

/// `user@host:port/path` for a protocol: the saved site for it if there is
/// one, else a new one for the servers that need only a password.
fn address_place(protocol: Protocol, rest: &str, sites: &SiteStore) -> Result<Place, String> {
    let (authority, path) = match rest.find('/') {
        Some(slash) => (&rest[..slash], vfs::http::decode(&rest[slash..])),
        None => (rest, String::new()),
    };
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((user, host_port)) => (vfs::http::decode(user), host_port),
        None => (String::new(), authority),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => (host, port.parse().unwrap_or_default()),
        _ => (host_port, protocol.default_port()),
    };
    if host.is_empty() {
        return Err(format!("no server in \"{}://{rest}\"", protocol.scheme()));
    }
    let saved = sites.sites.iter().find(|s| {
        // As a site's address shows it: plain FTP as ftp://.
        s.protocol.scheme() == protocol.scheme()
            && s.host.eq_ignore_ascii_case(host)
            && s.port == port
            && (user.is_empty() || s.user == user)
    });
    if let Some(site) = saved {
        return Ok(Place::Remote(Box::new(site.clone()), path));
    }
    if !matches!(protocol, Protocol::Sftp | Protocol::Ftp | Protocol::Ftps | Protocol::Webdav | Protocol::Webdavs) {
        return Err(format!("save {host} as a site in forkz first, then use its name (SITE:PATH)"));
    }
    let mut site = Site::new(host, port, &user);
    site.protocol = protocol;
    Ok(Place::Remote(Box::new(site), path))
}

struct Cli {
    sites: SiteStore,
    settings: Settings,
    /// Open connections, by site id.
    connections: HashMap<String, FsRef>,
    /// The password from standard input, for the first site that needs one.
    password: Mutex<Option<String>>,
    /// What the last sync did, in a few words (for the synclet's record).
    summary: String,
}

impl Cli {
    fn new(options: &Options) -> Result<Cli, String> {
        let password = if options.password_stdin {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).map_err(|e| format!("could not read the password: {e}"))?;
            Some(text.trim_end_matches(['\r', '\n']).to_string())
        } else {
            None
        };
        let settings = Settings::load();
        let transfers = &settings.transfers;
        ops::set_speed_limits(transfers.upload_limit_kbps * 1024, transfers.download_limit_kbps * 1024);
        ops::set_reliability(transfers.verify_checksums, transfers.reconnect);
        Ok(Cli {
            sites: SiteStore::load(),
            settings,
            connections: HashMap::new(),
            password: Mutex::new(password),
            summary: String::new(),
        })
    }

    /// The file system and full path of a place, connecting if needed.
    fn open(&mut self, place: &Place) -> Result<(FsRef, PathBuf), String> {
        let (site, path) = match place {
            Place::Local(path) => return Ok((vfs::local(), path.clone())),
            Place::Remote(site, path) => (site, path),
        };
        let fs = match self.connections.get(&site.id) {
            Some(fs) => fs.clone(),
            None => {
                let fs = self.connect(site)?;
                self.connections.insert(site.id.clone(), fs.clone());
                fs
            }
        };
        // WebDAV's remote folder is the server's WebDAV root (see
        // sites::connect::open), so paths start at its top.
        let webdav = matches!(site.protocol, Protocol::Webdav | Protocol::Webdavs);
        let base = site.remote_dir.as_ref().filter(|_| !webdav).map(PathBuf::from).unwrap_or_else(|| fs.home());
        Ok((fs, if path.is_empty() { base } else { base.join(path) }))
    }

    fn connect(&self, site: &Site) -> Result<FsRef, String> {
        eprintln!("Connecting to {}\u{2026}", site.address());
        let saved = self.sites.get(&site.id).is_some();
        let password = saved.then(|| crate::sites::keychain::password(&site.id)).flatten();
        let password = password.or_else(|| self.password.lock().unwrap_or_else(|e| e.into_inner()).take());
        let timeout = Duration::from_secs(self.settings.sftp.timeout_seconds.max(5));
        let extra_keys = self.settings.sftp.key_files.clone();
        crate::sites::connect::open(site, password, timeout, extra_keys, &Terminal)
            .map(|(fs, _)| fs)
            .map_err(|e| format!("could not connect to {}: {e}", site.address()))
    }

    fn copy(&mut self, options: &Options) -> Result<bool, String> {
        let (destination, sources) = options.places.split_last().ok_or("nothing to copy")?;
        let mut from: Option<(FsRef, Vec<PathBuf>)> = None;
        for text in sources {
            let place = parse_place(text, &self.sites)?;
            let (fs, path) = self.open(&place)?;
            fs.stat(&path).map_err(|e| format!("{text}: {e}"))?;
            match &mut from {
                Some((first, paths)) if first.key() == fs.key() => paths.push(path),
                Some(_) => return Err("the sources must all be in one place (on the Mac, or on one site)".into()),
                None => from = Some((fs, vec![path])),
            }
        }
        let (source_fs, paths) = from.ok_or("nothing to copy")?;
        let (dest_fs, folder) = self.open(&parse_place(destination, &self.sites)?)?;
        make_folder(&dest_fs, &folder)?;
        let job = Job::between(JobKind::Copy, source_fs, paths, dest_fs, folder);
        eprintln!("{}", job.title());
        let outcome = ops::run(&job, &mut Report::new(), CancelToken::default());
        Ok(summarize(&[outcome]))
    }

    fn sync(&mut self, options: &Options) -> Result<bool, String> {
        let [source, destination] = options.places.as_slice() else { return Err("sync needs two places".into()) };
        let mut sync_options =
            sync::Options { hidden: options.hidden, delete_orphans: options.delete, ..Default::default() };
        if let Some(exclude) = &options.exclude {
            sync_options.exclude = exclude.clone();
        }
        let left = parse_place(source, &self.sites)?;
        let right = parse_place(destination, &self.sites)?;
        self.run_sync(&left, &right, &sync_options, options.dry_run)
    }

    fn synclet(&mut self, options: &Options) -> Result<bool, String> {
        let name = &options.places[0];
        let synclet = self
            .settings
            .synclets
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| format!("no synclet is called \u{201c}{name}\u{201d}"))?;
        let left = self.tab_place(&synclet.left)?;
        let right = self.tab_place(&synclet.right)?;
        if options.watch {
            return self.watch(&synclet, &left, &right);
        }
        Ok(self.run_synclet(&synclet, &left, &right, options.dry_run))
    }

    /// Runs a synclet once, and notes how it went (not for a dry run).
    fn run_synclet(&mut self, synclet: &Synclet, left: &Place, right: &Place, dry_run: bool) -> bool {
        let now = crate::vfs::http::iso_utc(std::time::SystemTime::now());
        eprintln!("Running \u{201c}{}\u{201d} ({now})", synclet.name);
        let (ok, summary) = match self.run_sync(left, right, &synclet.options, dry_run) {
            Ok(ok) => (ok, self.summary.clone()),
            Err(message) => {
                eprintln!("forkz: {message}");
                (false, message)
            }
        };
        if !dry_run {
            let previous = crate::schedule::runs().remove(&synclet.name).map(|run| run.ok);
            crate::schedule::record(&synclet.name, ok, &summary);
            if crate::schedule::is_scheduled_run() {
                let id = format!("synclet:{}", synclet.name);
                match crate::schedule::news(previous, ok) {
                    Some("failed") => {
                        crate::alerts::post(&id, &format!("\u{201c}{}\u{201d} did not sync", synclet.name), &summary)
                    }
                    Some(_) => {
                        crate::alerts::post(&id, &format!("\u{201c}{}\u{201d} syncs again", synclet.name), &summary)
                    }
                    None => {}
                }
            }
        }
        ok
    }

    /// Runs a synclet, then again a few seconds after files stop changing
    /// in its folders on the Mac, until stopped.
    fn watch(&mut self, synclet: &Synclet, left: &Place, right: &Place) -> Result<bool, String> {
        use notify::Watcher;
        // The side changes come from (a sync's own copies into its target
        // would only set off another run).
        let sources = match synclet.options.direction {
            Direction::LeftToRight => vec![left],
            Direction::RightToLeft => vec![right],
            Direction::Both => vec![left, right],
        };
        let folders: Vec<PathBuf> = sources
            .into_iter()
            .filter_map(|place| match place {
                Place::Local(path) => Some(path.clone()),
                Place::Remote(..) => None,
            })
            .collect();
        if folders.is_empty() {
            return Err("--watch needs a synclet that syncs from a folder on the Mac".into());
        }
        let (send, changes) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok_and(|e| !e.kind.is_access()) {
                let _ = send.send(());
            }
        })
        .map_err(|e| format!("could not watch for changes: {e}"))?;
        for folder in &folders {
            watcher
                .watch(folder, notify::RecursiveMode::Recursive)
                .map_err(|e| format!("could not watch {}: {e}", folder.display()))?;
        }
        self.run_synclet(synclet, left, right, false);
        eprintln!(
            "Watching for changes in {}",
            folders.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(" and ")
        );
        while changes.recv().is_ok() {
            // Until nothing has changed for a few seconds (a save, a build,
            // or a copy is many changes).
            loop {
                match changes.recv_timeout(SETTLE) {
                    Ok(()) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(false),
                }
            }
            self.run_synclet(synclet, left, right, false);
        }
        Ok(false)
    }

    /// A synclet's side as a place (its path is the full path).
    fn tab_place(&self, tab: &WorkspaceTab) -> Result<Place, String> {
        let Some(id) = &tab.site else { return Ok(Place::Local(tab.path.clone())) };
        let site = self.sites.get(id).ok_or("the synclet uses a site that was deleted")?;
        Ok(Place::Remote(Box::new(site.clone()), tab.path.to_string_lossy().into_owned()))
    }

    fn run_sync(
        &mut self,
        left: &Place,
        right: &Place,
        options: &sync::Options,
        dry_run: bool,
    ) -> Result<bool, String> {
        let (left_fs, left_root) = self.open(left)?;
        let (right_fs, right_root) = self.open(right)?;
        // A one-way sync can start an empty folder.
        match options.direction {
            Direction::LeftToRight if !dry_run => make_folder(&right_fs, &right_root)?,
            Direction::RightToLeft if !dry_run => make_folder(&left_fs, &left_root)?,
            _ => {}
        }
        eprintln!("Comparing {} with {}\u{2026}", left_fs.display_path(&left_root), right_fs.display_path(&right_root));
        let cancel = CancelToken::default();
        let items = sync::compare(&left_fs, &left_root, &right_fs, &right_root, options, &cancel)
            .map_err(|e| format!("could not compare the folders: {e}"))?;
        let chosen: Vec<&sync::Item> = items.iter().filter(|i| i.action.is_some()).collect();
        if chosen.is_empty() {
            eprintln!("Already in sync.");
            self.summary = "Already in sync".into();
            return Ok(true);
        }
        for item in &chosen {
            let (sign, side) = match item.action {
                Some(Action::CopyToRight) => ("+", "\u{2192}"),
                Some(Action::CopyToLeft) => ("+", "\u{2190}"),
                Some(Action::DeleteRight) => ("-", "\u{2192}"),
                _ => ("-", "\u{2190}"),
            };
            let slash = if item.folder { "/" } else { "" };
            println!("{sign} {side} {}{slash}  ({})", item.path.display(), item.reason.describe().to_lowercase());
        }
        if dry_run {
            eprintln!("{} to change; nothing was changed (--dry-run).", count(chosen.len(), "item"));
            return Ok(true);
        }
        let mut outcomes = Vec::new();
        for job in sync::jobs(&chosen, &left_fs, &left_root, &right_fs, &right_root, true) {
            outcomes.push(ops::run(&job, &mut Report::new(), CancelToken::default()));
        }
        let failed: usize = outcomes.iter().map(|o| o.failed.len()).sum();
        let changed = chosen.len().saturating_sub(failed);
        self.summary = match failed {
            0 => format!("{} changed", count(changed, "item")),
            _ => format!("{} changed, {failed} failed", count(changed, "item")),
        };
        Ok(summarize(&outcomes))
    }
}

/// How long files must stay unchanged before a watching synclet runs.
const SETTLE: Duration = Duration::from_secs(5);

/// Makes a folder and the folders it is in, as needed.
fn make_folder(fs: &FsRef, folder: &Path) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut at = Some(folder);
    while let Some(path) = at {
        match fs.stat(path) {
            Ok(entry) if entry.package || entry.is_folder_like() => break,
            Ok(_) => return Err(format!("{} is a file, not a folder", fs.display_path(path))),
            Err(_) => missing.push(path.to_path_buf()),
        }
        at = path.parent();
    }
    for path in missing.iter().rev() {
        fs.create_dir(path).map_err(|e| format!("could not make {}: {e}", fs.display_path(path)))?;
    }
    Ok(())
}

/// Prints what failed, and returns whether everything worked.
fn summarize(outcomes: &[ops::Outcome]) -> bool {
    let failed: Vec<&(PathBuf, String)> = outcomes.iter().flat_map(|o| &o.failed).collect();
    let skipped: usize = outcomes.iter().map(|o| o.skipped).sum();
    for (path, error) in &failed {
        eprintln!("Failed: {}: {error}", path.display());
    }
    let mut summary = String::from("Done");
    if !failed.is_empty() {
        summary = format!("Done, but {} failed", count(failed.len(), "item"));
    }
    if skipped > 0 {
        summary.push_str(&format!(" ({} skipped)", count(skipped, "item")));
    }
    eprintln!("{summary}.");
    failed.is_empty()
}

fn count(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

/// Answers a job's questions: files are replaced and folders merged; an
/// error is reported, and the job goes on. Shows progress in a terminal.
struct Report {
    terminal: bool,
    shown: Option<Instant>,
}

impl Report {
    fn new() -> Report {
        Report { terminal: std::io::stderr().is_terminal(), shown: None }
    }

    fn clear(&mut self) {
        if self.shown.take().is_some() {
            eprint!("\r\x1b[K");
        }
    }
}

impl Drop for Report {
    fn drop(&mut self) {
        self.clear();
    }
}

impl Observer for Report {
    fn progress(&mut self, progress: &Progress) {
        if !self.terminal || self.shown.is_some_and(|t| t.elapsed() < Duration::from_millis(100)) {
            return;
        }
        let percent = (progress.fraction() * 100.0).round();
        let name: String = progress.current.chars().take(60).collect();
        eprint!("\r\x1b[K{percent:>3}%  {} of {}  {name}", progress.done_items, progress.total_items);
        let _ = std::io::stderr().flush();
        self.shown = Some(Instant::now());
    }

    fn conflict(&mut self, conflict: &Conflict) -> Answer {
        let resolution = if conflict.can_merge() { Resolution::Merge } else { Resolution::Replace };
        Answer { resolution, apply_to_all: false }
    }

    fn error(&mut self, path: &Path, error: &std::io::Error) -> ErrorAnswer {
        self.clear();
        eprintln!("{}: {error}", path.display());
        ErrorAnswer::Skip
    }
}

/// Asks in the terminal: about a server's key forkz does not trust yet,
/// and for passwords. With no terminal, it answers no.
struct Terminal;

impl vfs::sftp::Prompter for Terminal {
    fn host_key(&self, host: &str, port: u16, key_type: &str, fingerprint: &str, status: Status) -> bool {
        let warning = match status {
            Status::Changed => format!(
                "WARNING: the key of {host}:{port} has changed since forkz last connected. Someone could be in the way."
            ),
            _ => format!("forkz has not connected to {host}:{port} before."),
        };
        eprintln!("{warning}\nIts {key_type} key's fingerprint is {fingerprint}.");
        match ask("Trust this key? (yes or no) ", false) {
            Some(answer) => matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
            None => {
                eprintln!("Not trusted (no terminal to ask in). Connect once in the forkz app to trust it.");
                false
            }
        }
    }

    fn secret(&self, message: &str, informative: &str) -> Option<String> {
        if !informative.is_empty() {
            eprintln!("{informative}");
        }
        let answer = ask(&format!("{message} "), true);
        if answer.is_none() {
            eprintln!("No password (no terminal to ask in). Save it in forkz, or use --password-stdin.");
        }
        answer
    }
}

impl vfs::aws::SignIn for Terminal {
    fn show_code(&self, code: &str, url: &str) {
        eprintln!("Sign in to AWS in your browser ({url}), and check that the page shows the code {code}.");
    }
}

impl crate::sites::connect::Ask for Terminal {
    fn for_renewal(&self) -> Box<dyn crate::sites::connect::Ask> {
        Box::new(Terminal)
    }
}

/// Asks a question in the terminal (`hidden`: without showing what is
/// typed). None if there is no terminal.
fn ask(question: &str, hidden: bool) -> Option<String> {
    use std::os::fd::AsRawFd;
    let mut tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    tty.write_all(question.as_bytes()).ok()?;
    let fd = tty.as_raw_fd();
    // SAFETY: termios is plain data; tcgetattr fills it for an open terminal.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    let echo_off = hidden && unsafe { libc::tcgetattr(fd, &mut saved) } == 0;
    if echo_off {
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        // SAFETY: the same open terminal, with settings it gave.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
    }
    let mut line = String::new();
    let read = std::io::BufRead::read_line(&mut std::io::BufReader::new(&tty), &mut line);
    if echo_off {
        // SAFETY: as above; puts the terminal back as it was.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
        let _ = tty.write_all(b"\n");
    }
    read.ok().filter(|n| *n > 0)?;
    Some(line.trim_end_matches(['\r', '\n']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    fn store() -> SiteStore {
        let mut web = Site::new("web.example.com", 22, "deploy");
        web.name = "Web Server".into();
        web.remote_dir = Some("/var/www".into());
        let mut ftp = Site::new("files.example.com", 21, "me");
        ftp.protocol = Protocol::Ftp;
        SiteStore { sites: vec![web, ftp] }
    }

    #[test]
    fn reads_commands() {
        let copy = Options::parse("copy", &strings(&["a", "b", "site:/x"])).unwrap();
        assert_eq!(copy.places, ["a", "b", "site:/x"]);
        assert!(Options::parse("copy", &strings(&["a"])).is_err());
        assert!(Options::parse("copy", &strings(&["a", "b", "--delete"])).is_err(), "sync only");
        assert!(Options::parse("copy", &strings(&["a", "b", "--dry-run"])).is_err());
        let sync =
            Options::parse("sync", &strings(&["--delete", "a", "--exclude", "*.log;.git", "b", "--dry-run"])).unwrap();
        assert_eq!(sync.places, ["a", "b"]);
        assert!(sync.delete && sync.dry_run && !sync.hidden);
        assert_eq!(sync.exclude.as_deref(), Some("*.log;.git"));
        assert!(Options::parse("sync", &strings(&["a", "b", "c"])).is_err());
        assert!(Options::parse("sync", &strings(&["a", "b", "--nope"])).is_err());
        let dashed = Options::parse("sync", &strings(&["--", "--odd", "b"])).unwrap();
        assert_eq!(dashed.places, ["--odd", "b"]);
        assert_eq!(Options::parse("synclet", &strings(&["Site"])).unwrap().places, ["Site"]);
        assert!(Options::parse("synclet", &strings(&["Site", "--watch"])).unwrap().watch);
        assert!(Options::parse("synclet", &strings(&["Site", "--watch", "--dry-run"])).is_err());
        assert!(Options::parse("sync", &strings(&["a", "b", "--watch"])).is_err(), "synclets only");
        assert!(Options::parse("synclet", &strings(&[])).is_err());
    }

    #[test]
    fn reads_places() {
        let sites = store();
        match parse_place("web server:public", &sites).unwrap() {
            Place::Remote(site, path) => {
                assert_eq!(site.name, "Web Server");
                assert_eq!(path, "public");
            }
            other => panic!("{other:?}"),
        }
        // An address finds the saved site, with its id (and password).
        match parse_place("ftp://me@files.example.com/in/a%20b", &sites).unwrap() {
            Place::Remote(site, path) => {
                assert_eq!(site.id, sites.sites[1].id);
                assert_eq!(path, "/in/a b");
            }
            other => panic!("{other:?}"),
        }
        // A new server, for protocols that need only a password.
        match parse_place("sftp://other.example.com:2222", &sites).unwrap() {
            Place::Remote(site, path) => {
                assert_eq!((site.host.as_str(), site.port, site.protocol), ("other.example.com", 2222, Protocol::Sftp));
                assert!(sites.get(&site.id).is_none());
                assert_eq!(path, "");
            }
            other => panic!("{other:?}"),
        }
        assert!(parse_place("dropbox://x/y", &sites).is_err(), "cloud services need a saved site");
        // Not a site's name: a path on the Mac (colons are allowed).
        assert_eq!(parse_place("/tmp/a:b", &sites).unwrap(), Place::Local(PathBuf::from("/tmp/a:b")));
        let here = std::env::current_dir().unwrap();
        assert_eq!(parse_place("notes.txt", &sites).unwrap(), Place::Local(here.join("notes.txt")));
    }

    #[test]
    fn copies_and_syncs_on_the_mac() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let (from, to) = (base.join("from"), base.join("to"));
        std::fs::create_dir_all(from.join("sub")).unwrap();
        std::fs::write(from.join("a.txt"), "a").unwrap();
        std::fs::write(from.join("sub/b.txt"), "b").unwrap();
        let mut cli = Cli {
            sites: SiteStore::default(),
            settings: Settings::default(),
            connections: HashMap::new(),
            password: Mutex::new(None),
            summary: String::new(),
        };
        let copy = Options { places: vec![from.display().to_string(), to.display().to_string()], ..Default::default() };
        assert_eq!(cli.copy(&copy), Ok(true));
        assert_eq!(std::fs::read_to_string(to.join("from/sub/b.txt")).unwrap(), "b");
        // Again: replaced and merged, not "from 2".
        assert_eq!(cli.copy(&copy), Ok(true));
        assert!(!to.join("from 2").exists());

        let mirror = base.join("mirror");
        std::fs::create_dir_all(&mirror).unwrap();
        std::fs::write(mirror.join("old.txt"), "old").unwrap();
        let mut sync = Options {
            places: vec![from.display().to_string(), mirror.display().to_string()],
            dry_run: true,
            delete: true,
            ..Default::default()
        };
        assert_eq!(cli.sync(&sync), Ok(true));
        assert!(!mirror.join("a.txt").exists(), "a dry run changes nothing");
        sync.dry_run = false;
        assert_eq!(cli.sync(&sync), Ok(true));
        assert_eq!(std::fs::read_to_string(mirror.join("sub/b.txt")).unwrap(), "b");
        assert!(!mirror.join("old.txt").exists(), "--delete");
    }
}
