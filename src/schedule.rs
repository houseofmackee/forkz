//! Scheduled synclets: each runs as a launchd job (a LaunchAgent in
//! ~/Library/LaunchAgents), so it runs on time even when forkz is closed.
//! The job runs `forkz synclet NAME` from the app; one that follows changes
//! keeps `forkz synclet NAME --watch` running. Each run's outcome is kept
//! in `synclet-runs.json` for the sidebar to show, and its output in
//! ~/Library/Logs/forkz.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::settings::Synclet;

/// When a synclet runs by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Schedule {
    /// Every so many minutes (while the Mac is awake; a run missed while
    /// it slept happens when it wakes).
    Every { minutes: u32 },
    /// Every day at a time.
    Daily { hour: u8, minute: u8 },
    /// A few seconds after files change in its folder on the Mac.
    OnChange,
}

impl Schedule {
    /// "every hour", "daily at 09:30", "when files change".
    pub fn describe(self) -> String {
        match self {
            Schedule::Every { minutes } if minutes % (24 * 60) == 0 && minutes > 0 => match minutes / (24 * 60) {
                1 => "every day".into(),
                n => format!("every {n} days"),
            },
            Schedule::Every { minutes } if minutes % 60 == 0 && minutes > 0 => match minutes / 60 {
                1 => "every hour".into(),
                n => format!("every {n} hours"),
            },
            Schedule::Every { minutes: 1 } => "every minute".into(),
            Schedule::Every { minutes } => format!("every {minutes} minutes"),
            Schedule::Daily { hour, minute } => format!("daily at {hour:02}:{minute:02}"),
            Schedule::OnChange => "when files change on the Mac".into(),
        }
    }
}

/// The start of every forkz job's name.
const PREFIX: &str = "io.github.houseofmackee.forkz.synclet.";

/// A synclet's job name: its name made safe, and a short hash so names
/// that only differ in characters left out stay apart.
pub fn label(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    // FNV-1a: stable across runs and Rust versions.
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    format!("{PREFIX}{}{:08x}", if slug.is_empty() { String::new() } else { format!("{slug}-") }, hash as u32)
}

fn agents_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents"))
}

fn logs_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/Logs/forkz"))
}

/// Where a synclet's runs write what they did.
pub fn log_path(name: &str) -> Option<PathBuf> {
    logs_dir().map(|d| d.join(format!("{}.log", label(name).trim_start_matches(PREFIX))))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The launchd job for a scheduled synclet, as a property list.
pub fn plist(program: &Path, synclet: &Synclet, schedule: Schedule, log: &Path) -> String {
    let mut arguments = vec![program.display().to_string(), "synclet".into(), synclet.name.clone()];
    if schedule == Schedule::OnChange {
        arguments.push("--watch".into());
    }
    let arguments: String = arguments.iter().map(|a| format!("        <string>{}</string>\n", escape(a))).collect();
    let when = match schedule {
        Schedule::Every { minutes } => {
            format!("    <key>StartInterval</key>\n    <integer>{}</integer>\n", u64::from(minutes.max(1)) * 60)
        }
        Schedule::Daily { hour, minute } => format!(
            "    <key>StartCalendarInterval</key>\n    <dict>\n        <key>Hour</key>\n        <integer>{hour}</integer>\n        <key>Minute</key>\n        <integer>{minute}</integer>\n    </dict>\n"
        ),
        // Started at login and kept running (it waits for changes); after
        // a failure, started again, but not more than once a minute.
        Schedule::OnChange => "    <key>RunAtLoad</key>\n    <true/>\n    <key>KeepAlive</key>\n    <dict>\n        <key>SuccessfulExit</key>\n        <false/>\n    </dict>\n    <key>ThrottleInterval</key>\n    <integer>60</integer>\n".into(),
    };
    let log = escape(&log.display().to_string());
    // A build run from source keeps its HOME (a test profile), so its jobs
    // never read or write the real settings.
    let home = match std::env::var("HOME") {
        Ok(home) if cfg!(debug_assertions) => format!(
            "    <key>EnvironmentVariables</key>\n    <dict>\n        <key>HOME</key>\n        <string>{}</string>\n    </dict>\n",
            escape(&home)
        ),
        _ => String::new(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>Comment</key>
    <string>forkz runs the synclet {name} {describe}. Change or stop it in forkz (right-click the synclet).</string>
    <key>ProgramArguments</key>
    <array>
{arguments}    </array>
{when}{home}    <key>ProcessType</key>
    <string>Background</string>
    <key>LowPriorityIO</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = escape(&label(&synclet.name)),
        name = escape(&format!("\u{201c}{}\u{201d}", synclet.name)),
        describe = escape(&schedule.describe()),
    )
}

fn launchctl(args: &[&str]) -> io::Result<std::process::Output> {
    std::process::Command::new("/bin/launchctl").args(args).output()
}

fn domain() -> String {
    // SAFETY: getuid cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

/// Whether this copy of forkz may change launchd jobs: the app, or a
/// build run from source that asks for it (FORKZ_SCHEDULES=1), so trying
/// out a build does not leave jobs pointing at it.
pub fn manages_jobs() -> bool {
    !cfg!(debug_assertions) || std::env::var_os("FORKZ_SCHEDULES").is_some()
}

/// Makes the launchd jobs match the synclets' schedules: new and changed
/// ones written and loaded, and those of synclets that are gone or no
/// longer scheduled stopped and removed. `program` is forkz's executable.
pub fn apply(synclets: &[Synclet], program: &Path) -> io::Result<()> {
    if !manages_jobs() {
        return Ok(());
    }
    let (Some(agents), Some(logs)) = (agents_dir(), logs_dir()) else { return Ok(()) };
    let wanted: BTreeMap<String, String> = synclets
        .iter()
        .filter_map(|s| {
            let schedule = s.schedule?;
            let log = logs.join(format!("{}.log", label(&s.name).trim_start_matches(PREFIX)));
            Some((label(&s.name), plist(program, s, schedule, &log)))
        })
        .collect();
    let existing: Vec<String> = std::fs::read_dir(&agents)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().to_str()?.strip_suffix(".plist").map(str::to_string))
                .filter(|label| label.starts_with(PREFIX))
                .collect()
        })
        .unwrap_or_default();
    let domain = domain();
    for label in &existing {
        let file = agents.join(format!("{label}.plist"));
        let same = wanted.get(label).is_some_and(|text| std::fs::read_to_string(&file).ok().as_ref() == Some(text));
        if !same {
            let _ = launchctl(&["bootout", &format!("{domain}/{label}")]);
            std::fs::remove_file(&file)?;
            tracing::info!("stopped the scheduled job {label}");
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(&agents)?;
    std::fs::create_dir_all(&logs)?;
    for (label, text) in &wanted {
        let file = agents.join(format!("{label}.plist"));
        if std::fs::read_to_string(&file).ok().as_ref() == Some(text) {
            continue;
        }
        std::fs::write(&file, text)?;
        let output = launchctl(&["bootstrap", &domain, &file.display().to_string()])?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(io::Error::other(format!("macOS did not accept the schedule for {label}: {detail}")));
        }
        tracing::info!("scheduled {label}");
    }
    Ok(())
}

/// Whether this process is a scheduled synclet's job (launchd names the
/// job it runs).
pub fn is_scheduled_run() -> bool {
    std::env::var("XPC_SERVICE_NAME").is_ok_and(|name| name.starts_with(PREFIX))
}

/// What to tell the user after a scheduled run, given how the run before
/// went: that it stopped working, or works again (not every failure: a
/// synclet that fails every 15 minutes says so once).
pub fn news(previous: Option<bool>, ok: bool) -> Option<&'static str> {
    match (previous, ok) {
        (Some(true) | None, false) => Some("failed"),
        (Some(false), true) => Some("recovered"),
        _ => None,
    }
}

/// How a synclet's last run went (scheduled or from the command line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// Seconds since 1970.
    pub when: u64,
    pub ok: bool,
    /// "3 items copied", "Already in sync", or what failed.
    pub summary: String,
}

fn runs_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("forkz").join("synclet-runs.json"))
}

/// Every synclet's last run, by name.
pub fn runs() -> BTreeMap<String, Run> {
    runs_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Notes a synclet's run (written whole, then renamed into place: a
/// reader never sees half of it).
pub fn record(name: &str, ok: bool, summary: &str) {
    let Some(path) = runs_path() else { return };
    let mut all = runs();
    let when = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    all.insert(name.to_string(), Run { when, ok, summary: summary.to_string() });
    let result = (|| -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&all).map_err(io::Error::other)?)?;
        std::fs::rename(&temporary, &path)
    })();
    if let Err(e) = result {
        tracing::warn!("could not note the synclet's run: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::WorkspaceTab;

    fn synclet(name: &str, schedule: Option<Schedule>) -> Synclet {
        Synclet {
            name: name.into(),
            left: WorkspaceTab { site: None, path: "/tmp/a".into() },
            right: WorkspaceTab { site: Some("s1".into()), path: "/www".into() },
            options: Default::default(),
            schedule,
        }
    }

    #[test]
    fn labels_are_safe_and_stable() {
        let a = label("My Website & Blog");
        assert!(a.starts_with("io.github.houseofmackee.forkz.synclet.my-website-blog-"), "{a}");
        assert_eq!(a, label("My Website & Blog"));
        assert_ne!(a, label("My Website  Blog"), "names that differ stay apart");
        assert!(label("Fotos ä").chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c)));
        assert!(label("ääää").starts_with(PREFIX));
    }

    #[test]
    fn tells_of_changes_only() {
        assert_eq!(news(None, false), Some("failed"));
        assert_eq!(news(Some(true), false), Some("failed"));
        assert_eq!(news(Some(false), false), None, "still failing: said once");
        assert_eq!(news(Some(false), true), Some("recovered"));
        assert_eq!(news(Some(true), true), None);
        assert_eq!(news(None, true), None);
    }

    #[test]
    fn describes_schedules() {
        assert_eq!(Schedule::Every { minutes: 15 }.describe(), "every 15 minutes");
        assert_eq!(Schedule::Every { minutes: 60 }.describe(), "every hour");
        assert_eq!(Schedule::Every { minutes: 360 }.describe(), "every 6 hours");
        assert_eq!(Schedule::Every { minutes: 1440 }.describe(), "every day");
        assert_eq!(Schedule::Daily { hour: 9, minute: 5 }.describe(), "daily at 09:05");
        assert_eq!(Schedule::OnChange.describe(), "when files change on the Mac");
    }

    #[test]
    fn writes_launchd_jobs() {
        let program = Path::new("/Applications/forkz.app/Contents/MacOS/forkz");
        let log = Path::new("/Users/me/Library/Logs/forkz/site.log");
        let every = plist(program, &synclet("Site <1>", None), Schedule::Every { minutes: 30 }, log);
        assert!(every.contains("<string>Site &lt;1&gt;</string>"), "{every}");
        assert!(every.contains("<key>StartInterval</key>\n    <integer>1800</integer>"));
        assert!(!every.contains("--watch"));
        let daily = plist(program, &synclet("Site", None), Schedule::Daily { hour: 7, minute: 30 }, log);
        assert!(daily.contains("<key>Hour</key>\n        <integer>7</integer>"));
        let watch = plist(program, &synclet("Site", None), Schedule::OnChange, log);
        assert!(watch.contains("<string>--watch</string>") && watch.contains("<key>KeepAlive</key>"));
        // Well-formed property lists (Python's reader parses them).
        let dir = tempfile::tempdir().unwrap();
        for (i, text) in [every, daily, watch].iter().enumerate() {
            let file = dir.path().join(format!("{i}.plist"));
            std::fs::write(&file, text).unwrap();
            let read = std::process::Command::new("python3")
                .args([
                    "-I",
                    "-c",
                    "import plistlib, sys; d = plistlib.load(open(sys.argv[1], 'rb')); print(d['Label'])",
                ])
                .arg(&file)
                .output()
                .unwrap();
            assert!(read.status.success(), "{}", String::from_utf8_lossy(&read.stderr));
            assert!(String::from_utf8_lossy(&read.stdout).starts_with(PREFIX));
        }
    }

    #[test]
    fn settings_keep_schedules() {
        let with = synclet("A", Some(Schedule::Daily { hour: 23, minute: 0 }));
        let text = toml::to_string(&with).unwrap();
        assert!(text.contains("kind = \"daily\""), "{text}");
        let back: Synclet = toml::from_str(&text).unwrap();
        assert_eq!(back, with);
        // Synclets saved before schedules have none.
        let old: Synclet =
            toml::from_str("name = \"A\"\n[left]\npath = \"/tmp/a\"\n[right]\nsite = \"s1\"\npath = \"/www\"\n")
                .unwrap();
        assert_eq!(old.schedule, None);
    }
}
