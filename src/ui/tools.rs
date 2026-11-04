//! Other apps forkz hands work to: the terminal (Open in Terminal) and the
//! compare tool (Compare Files). Only installed ones are offered.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use objc2_app_kit::NSWorkspace;

use super::util::{ns, url_path};
use crate::settings::{COMPARE_TOOLS, TERMINALS};

/// Where an app is, by bundle identifier (several may be given, comma-
/// separated, for apps that changed theirs).
pub fn app_path(bundle_ids: &str) -> Option<PathBuf> {
    let workspace = NSWorkspace::sharedWorkspace();
    bundle_ids
        .split(',')
        .find_map(|id| workspace.URLForApplicationWithBundleIdentifier(&ns(id.trim())).and_then(|u| url_path(&u)))
}

/// FileMerge comes with Xcode, inside it.
fn filemerge_path() -> Option<PathBuf> {
    let output = Command::new("/usr/bin/xcode-select").arg("-p").output().ok()?;
    let developer = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    let app = developer.parent()?.join("Applications/FileMerge.app");
    app.exists().then_some(app).or_else(|| app_path("com.apple.FileMerge"))
}

/// The installed terminals: (key, name).
pub fn installed_terminals() -> Vec<(&'static str, &'static str)> {
    TERMINALS.iter().filter(|(_, _, id)| app_path(id).is_some()).map(|(key, name, _)| (*key, *name)).collect()
}

/// The installed compare tools: (key, name).
pub fn installed_compare_tools() -> Vec<(&'static str, &'static str)> {
    COMPARE_TOOLS
        .iter()
        .filter(|(key, _, id)| if *key == "filemerge" { filemerge_path().is_some() } else { app_path(id).is_some() })
        .map(|(key, name, _)| (*key, *name))
        .collect()
}

fn terminal_app(key: &str) -> (String, PathBuf) {
    let found =
        TERMINALS.iter().find(|(k, _, _)| *k == key).and_then(|(k, _, id)| Some((k.to_string(), app_path(id)?)));
    found.unwrap_or_else(|| ("terminal".into(), PathBuf::from("/System/Applications/Utilities/Terminal.app")))
}

fn open_app(app: &Path, extra: &[&str]) -> io::Result<()> {
    let mut command = Command::new("/usr/bin/open");
    command.arg("-a").arg(app).args(extra);
    command.spawn().map(|_| ())
}

/// Opens a terminal window in a local folder.
pub fn open_terminal(key: &str, folder: &Path) -> io::Result<()> {
    let (key, app) = terminal_app(key);
    let folder = folder.to_string_lossy();
    match key.as_str() {
        "ghostty" => open_app(&app, &["-n", "--args", &format!("--working-directory={folder}")]),
        "kitty" => open_app(&app, &["-n", "--args", "--directory", &folder]),
        _ => open_app(&app, &[&folder]),
    }
}

/// Runs a shell script (an SSH session) in a terminal window. Terminal and
/// iTerm open .command files; Ghostty and Kitty run it as their command;
/// Warp cannot, so Terminal does.
pub fn run_in_terminal(key: &str, script: &Path) -> io::Result<()> {
    let (key, app) = terminal_app(key);
    let script_text = script.to_string_lossy();
    match key.as_str() {
        "iterm" => open_app(&app, &[&script_text]),
        "ghostty" => open_app(&app, &["-n", "--args", "-e", "/bin/sh", &script_text]),
        "kitty" => open_app(&app, &["-n", "--args", "/bin/sh", &script_text]),
        _ => open_app(Path::new("/System/Applications/Utilities/Terminal.app"), &[&script_text]),
    }
}

/// Compares two local files in the chosen tool.
pub fn compare(key: &str, left: &Path, right: &Path) -> io::Result<()> {
    let tool = COMPARE_TOOLS.iter().find(|(k, _, _)| *k == key).or_else(|| COMPARE_TOOLS.first());
    let Some((key, name, id)) = tool else { return Err(io::Error::other("No compare tool is set.")) };
    let not_installed = || io::Error::new(io::ErrorKind::NotFound, format!("{name} is not installed."));
    let spawn = |program: &Path, args: &[&str]| -> io::Result<()> {
        Command::new(program).args(args).arg(left).arg(right).spawn().map(|_| ())
    };
    if *key == "filemerge" {
        filemerge_path().ok_or_else(not_installed)?;
        return spawn(Path::new("/usr/bin/opendiff"), &[]);
    }
    let app = app_path(id).ok_or_else(not_installed)?;
    // Each tool's own command line helper, inside the app.
    let helper = match *key {
        "kaleidoscope" => Some((app.join("Contents/MacOS/ksdiff"), vec![])),
        "beyondcompare" => Some((app.join("Contents/MacOS/bcomp"), vec![])),
        "bbedit" => Some((app.join("Contents/Helpers/bbdiff"), vec![])),
        "vscode" => Some((app.join("Contents/Resources/app/bin/code"), vec!["--diff"])),
        _ => None,
    };
    match helper {
        Some((program, args)) if program.exists() => spawn(&program, &args),
        // Otherwise the app gets both files and decides.
        _ => {
            let (l, r) = (left.to_string_lossy(), right.to_string_lossy());
            open_app(&app, &[&l, &r])
        }
    }
}
