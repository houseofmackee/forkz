//! Your own tools (Settings > Tools): shell commands run on the selection,
//! from Commands > Tools, the toolbar, or the command palette. A command
//! runs in zsh in the pane's folder, with these filled in (each path
//! quoted for the shell):
//!
//! - `{selection}`: the selected items (the folder if nothing is selected)
//! - `{file}`: the first selected item
//! - `{names}`: the selected items' names
//! - `{folder}`: the pane's folder
//! - `{other-folder}` and `{other-selection}`: the same for the other pane
//!
//! The same are in the environment as FORKZ_SELECTION (one path a line),
//! FORKZ_FOLDER, and FORKZ_OTHER_FOLDER.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dispatch2::DispatchQueue;

use crate::settings::CustomTool;

/// What a tool runs on.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub selection: Vec<PathBuf>,
    pub folder: PathBuf,
    pub other_selection: Vec<PathBuf>,
    pub other_folder: PathBuf,
}

/// A path quoted for the shell.
fn quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn quote_all(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" ")
}

/// The command with its placeholders filled in.
pub fn expand(command: &str, context: &Context) -> String {
    let selection = if context.selection.is_empty() { vec![context.folder.clone()] } else { context.selection.clone() };
    let names: Vec<PathBuf> = selection.iter().map(|p| PathBuf::from(p.file_name().unwrap_or(p.as_os_str()))).collect();
    let other = if context.other_selection.is_empty() {
        vec![context.other_folder.clone()]
    } else {
        context.other_selection.clone()
    };
    command
        .replace("{selection}", &quote_all(&selection))
        .replace("{file}", &quote(&selection[0]))
        .replace("{names}", &quote_all(&names))
        .replace("{other-selection}", &quote_all(&other))
        .replace("{other-folder}", &quote(&context.other_folder))
        .replace("{folder}", &quote(&context.folder))
}

/// The tool's command, run in the background (Stop in the activity area
/// ends it); the panes show what changed afterwards.
pub fn run(tool: &CustomTool, context: Context) {
    let Some(activity) = super::activity_view() else { return };
    let (note, cancel) = activity.begin_task(&format!("Running \u{201c}{}\u{201d}\u{2026}", tool.name));
    let script = expand(&tool.command, &context);
    let tool = tool.clone();
    std::thread::spawn(move || {
        // Apps start with a short PATH: Homebrew's tools too.
        let path = format!(
            "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:{}",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into())
        );
        let selection = context.selection.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        let child = Command::new("/bin/zsh")
            .arg("-c")
            .arg(&script)
            .current_dir(&context.folder)
            .env("PATH", path)
            .env("FORKZ_SELECTION", selection)
            .env("FORKZ_FOLDER", &context.folder)
            .env("FORKZ_OTHER_FOLDER", &context.other_folder)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let result = child.and_then(|child| {
            // Stop ends it.
            let pid = child.id() as libc::pid_t;
            let done = Arc::new(AtomicBool::new(false));
            let watcher = {
                let (done, cancel) = (done.clone(), cancel.clone());
                std::thread::spawn(move || {
                    while !done.load(Ordering::Relaxed) {
                        if cancel.is_cancelled() {
                            unsafe { libc::kill(pid, libc::SIGTERM) };
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(150));
                    }
                })
            };
            let output = child.wait_with_output();
            done.store(true, Ordering::Relaxed);
            let _ = watcher.join();
            output
        });
        let stopped = cancel.is_cancelled();
        DispatchQueue::main().exec_async(move || {
            if let Some(activity) = super::activity_view() {
                activity.remove(note);
            }
            finished(&tool, result, stopped);
        });
    });
}

fn finished(tool: &CustomTool, result: std::io::Result<std::process::Output>, stopped: bool) {
    if let Some(app) = super::app() {
        for pane in &app.panes {
            if pane.is_local() {
                pane.reload();
            }
        }
    }
    if stopped {
        return;
    }
    let output = match result {
        Ok(output) => output,
        Err(e) => {
            super::dialogs::show_error(format!("\u{201c}{}\u{201d} could not be run.", tool.name), e.to_string());
            return;
        }
    };
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim_end().to_string();
    let (out, err) = (text(&output.stdout), text(&output.stderr));
    let shown = |s: String| {
        // An alert cannot show a book: the start, then a note.
        const MAX: usize = 6_000;
        if s.chars().count() > MAX {
            let cut: String = s.chars().take(MAX).collect();
            format!("{cut}\n\n(cut off: the output is longer)")
        } else {
            s
        }
    };
    if !output.status.success() {
        let code = output.status.code().map_or_else(|| "it was stopped".to_string(), |c| format!("exit code {c}"));
        let detail = [err, out].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
        super::dialogs::show_error(
            format!("\u{201c}{}\u{201d} did not succeed ({code}).", tool.name),
            shown(if detail.is_empty() { "It printed nothing.".into() } else { detail }),
        );
    } else if tool.show_output {
        let all = [out, err].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
        super::dialogs::show_info(
            format!("\u{201c}{}\u{201d} is done.", tool.name),
            shown(if all.is_empty() { "It printed nothing.".into() } else { all }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_in_placeholders() {
        let context = Context {
            selection: vec!["/a/one.txt".into(), "/a/it's here.md".into()],
            folder: "/a".into(),
            other_selection: Vec::new(),
            other_folder: "/b c".into(),
        };
        assert_eq!(expand("wc -l {selection}", &context), "wc -l '/a/one.txt' '/a/it'\\''s here.md'");
        assert_eq!(expand("open {file}", &context), "open '/a/one.txt'");
        assert_eq!(expand("echo {names}", &context), "echo 'one.txt' 'it'\\''s here.md'");
        assert!(expand("cp {selection} {other-folder}", &context).ends_with(" '/b c'"));
        assert_eq!(expand("ls {other-selection}", &context), "ls '/b c'");
        assert_eq!(expand("cd {folder}", &context), "cd '/a'");
        // Nothing selected: the folder.
        let empty = Context { folder: "/x".into(), ..Context::default() };
        assert_eq!(expand("du -sh {selection}", &empty), "du -sh '/x'");
    }

    #[test]
    fn runs_quoted_paths_in_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("it's a file.txt");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        let context = Context { selection: vec![file], folder: dir.path().to_path_buf(), ..Context::default() };
        let output = Command::new("/bin/zsh").arg("-c").arg(expand("wc -l < {file}", &context)).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "2");
    }
}
