//! The operation log, kept in memory for this session: transfers,
//! connections, and changes to files, shown in the preview pane's Activity
//! (transfers) and Log (everything) tabs.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Transfer,
    Connection,
    Change,
    Problem,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Transfer => "transfer",
            Kind::Connection => "connection",
            Kind::Change => "change",
            Kind::Problem => "problem",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub time: SystemTime,
    pub kind: Kind,
    pub text: String,
    /// More about it ("" for nothing), for example what failed and why.
    pub detail: String,
    /// For transfers: whether it went wrong.
    pub failed: bool,
}

const LIMIT: usize = 2000;

thread_local! {
    static LOG: RefCell<VecDeque<LogEntry>> = const { RefCell::new(VecDeque::new()) };
}

/// Adds to the log (on the main thread).
pub fn record(kind: Kind, text: impl Into<String>, detail: impl Into<String>) {
    record_entry(kind, text.into(), detail.into(), kind == Kind::Problem);
}

/// A transfer, which may have gone wrong.
pub fn record_transfer(text: String, detail: String, failed: bool) {
    record_entry(Kind::Transfer, text, detail, failed);
}

fn record_entry(kind: Kind, text: String, detail: String, failed: bool) {
    tracing::info!(
        "[{}] {text}{}",
        kind.label(),
        if detail.is_empty() { String::new() } else { format!(": {detail}") }
    );
    LOG.with(|log| {
        let mut log = log.borrow_mut();
        log.push_back(LogEntry { time: SystemTime::now(), kind, text, detail, failed });
        while log.len() > LIMIT {
            log.pop_front();
        }
    });
    super::log_changed();
}

pub fn entries() -> Vec<LogEntry> {
    LOG.with(|log| log.borrow().iter().cloned().collect())
}

pub fn clear() {
    LOG.with(|log| log.borrow_mut().clear());
    super::log_changed();
}

/// The log as text, one entry per line.
pub fn as_text() -> String {
    entries()
        .iter()
        .map(|e| {
            let time = crate::rename::format_date("YYYY-MM-DD hh:mm:ss", e.time);
            let detail =
                if e.detail.is_empty() { String::new() } else { format!("\n    {}", e.detail.replace('\n', "\n    ")) };
            format!("{time}  {:<10}  {}{detail}", e.kind.label(), e.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
