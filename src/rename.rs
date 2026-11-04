//! Multi-rename: rules applied in order to each name, a preview with the
//! problems found (empty or invalid names, two items getting the same name,
//! a name already taken in the folder), and renaming that copes with names
//! swapping places ("a" to "b" while "b" goes to "a").

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::vfs::FsRef;

/// Where text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Place {
    Start,
    #[default]
    End,
    /// After this many characters.
    Index(usize),
    /// Instead of the name.
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Match {
    /// Text, ignoring case.
    #[default]
    Text,
    TextMatchCase,
    /// A regular expression; "$1" in the replacement is the first group.
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Case {
    #[default]
    Lower,
    Upper,
    Title,
    Sentence,
}

/// Which part of a name a rule changes. Folders have no extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    #[default]
    Name,
    Extension,
    Whole,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "kebab-case")]
pub enum Rule {
    Replace {
        find: String,
        #[serde(default)]
        with: String,
        #[serde(default)]
        matching: Match,
    },
    Insert {
        text: String,
        #[serde(default)]
        at: Place,
    },
    Remove {
        #[serde(default)]
        from: usize,
        count: usize,
        #[serde(default)]
        from_end: bool,
    },
    Case {
        to: Case,
    },
    Counter {
        #[serde(default = "one")]
        start: i64,
        #[serde(default = "one")]
        step: i64,
        #[serde(default)]
        digits: usize,
        #[serde(default)]
        separator: String,
        #[serde(default)]
        at: Place,
    },
    Date {
        #[serde(default = "date_format")]
        format: String,
        #[serde(default)]
        separator: String,
        #[serde(default)]
        at: Place,
    },
}

fn one() -> i64 {
    1
}

fn date_format() -> String {
    "YYYY-MM-DD".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    #[serde(flatten)]
    pub rule: Rule,
    #[serde(default)]
    pub scope: Scope,
}

/// Saved rules, in settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// An item to rename.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub modified: Option<SystemTime>,
    pub folder: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    Empty,
    /// "/" or ":" in the name, or "." or "..".
    Invalid,
    /// Another item in the batch gets this name too.
    Duplicate,
    /// Something else in the folder has this name.
    Taken,
}

impl Problem {
    pub fn describe(&self) -> &'static str {
        match self {
            Problem::Empty => "The new name is empty.",
            Problem::Invalid => "Names cannot contain \u{201c}/\u{201d} or \u{201c}:\u{201d}.",
            Problem::Duplicate => "Another item gets the same name.",
            Problem::Taken => "An item with this name is already in the folder.",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub old: String,
    pub new: String,
    pub problem: Option<Problem>,
}

impl Planned {
    pub fn changes(&self) -> bool {
        self.old != self.new
    }
}

/// (name, extension without the dot). Folders, ".hidden", and names
/// without a dot have no extension.
fn split_name(name: &str, folder: bool) -> (String, String) {
    if folder {
        return (name.to_string(), String::new());
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => (name[..dot].to_string(), name[dot + 1..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

fn join_name(stem: &str, extension: &str) -> String {
    if extension.is_empty() { stem.to_string() } else { format!("{stem}.{extension}") }
}

fn place(text: &str, insert: &str, at: Place) -> String {
    match at {
        Place::Start => format!("{insert}{text}"),
        Place::End => format!("{text}{insert}"),
        Place::Replace => insert.to_string(),
        Place::Index(index) => {
            let split = text.char_indices().nth(index).map_or(text.len(), |(i, _)| i);
            format!("{}{insert}{}", &text[..split], &text[split..])
        }
    }
}

/// Text with a separator on the side facing the name.
fn with_separator(value: &str, separator: &str, at: Place) -> String {
    match at {
        Place::Start => format!("{value}{separator}"),
        Place::End => format!("{separator}{value}"),
        Place::Index(_) | Place::Replace => value.to_string(),
    }
}

fn change_case(text: &str, case: Case) -> String {
    match case {
        Case::Lower => text.to_lowercase(),
        Case::Upper => text.to_uppercase(),
        Case::Title => {
            let mut out = String::with_capacity(text.len());
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    out.extend(c.to_lowercase());
                    if !c.is_alphanumeric() && c != '\'' && c != '\u{2019}' {
                        start = true;
                    } else if c.is_alphanumeric() {
                        start = false;
                    }
                }
            }
            out
        }
        Case::Sentence => {
            let lower = text.to_lowercase();
            let mut out = String::with_capacity(lower.len());
            let mut done = false;
            for c in lower.chars() {
                if !done && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    done = true;
                } else {
                    out.push(c);
                }
            }
            out
        }
    }
}

/// The local date and time of `time` as (year, month, day, hour, minute, second).
fn local_parts(time: SystemTime) -> (i32, u32, u32, u32, u32, u32) {
    let seconds = time.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as libc::time_t).unwrap_or(0);
    // SAFETY: localtime_r only writes the struct it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&seconds, &mut tm) };
    (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
}

/// "YYYY-MM-DD hh.mm.ss" style formats: YYYY, YY, MM, DD, hh, mm, ss.
pub fn format_date(format: &str, time: SystemTime) -> String {
    let (year, month, day, hour, minute, second) = local_parts(time);
    let tokens: [(&str, String); 7] = [
        ("YYYY", format!("{year:04}")),
        ("YY", format!("{:02}", year % 100)),
        ("MM", format!("{month:02}")),
        ("DD", format!("{day:02}")),
        ("hh", format!("{hour:02}")),
        ("mm", format!("{minute:02}")),
        ("ss", format!("{second:02}")),
    ];
    let mut out = String::new();
    let mut rest = format;
    'outer: while !rest.is_empty() {
        for (token, value) in &tokens {
            if let Some(after) = rest.strip_prefix(token) {
                out.push_str(value);
                rest = after;
                continue 'outer;
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// A rule ready to apply (with its pattern compiled).
enum Ready<'a> {
    Pattern(regex::Regex, &'a str),
    Other(&'a Rule),
}

fn prepare(steps: &[Step]) -> Result<Vec<(Ready<'_>, Scope)>, String> {
    steps
        .iter()
        .map(|step| {
            let ready = match &step.rule {
                Rule::Replace { find, .. } if find.is_empty() => Ready::Other(&step.rule),
                Rule::Replace { find, with, matching } => {
                    let pattern = if *matching == Match::Regex { find.clone() } else { regex::escape(find) };
                    let regex = RegexBuilder::new(&pattern)
                        .case_insensitive(*matching == Match::Text)
                        .build()
                        .map_err(|e| format!("The pattern \u{201c}{find}\u{201d} is not valid: {e}"))?;
                    // Literal text: "$" in the replacement is just a dollar.
                    let with: &str = with;
                    Ready::Pattern(regex, with)
                }
                other => Ready::Other(other),
            };
            Ok((ready, step.scope))
        })
        .collect()
}

fn apply_one(ready: &Ready, text: &str, index: usize, item: &Item, literal: bool) -> String {
    match ready {
        Ready::Pattern(regex, with) => {
            if literal {
                regex.replace_all(text, regex::NoExpand(with)).into_owned()
            } else {
                regex.replace_all(text, *with).into_owned()
            }
        }
        Ready::Other(rule) => match rule {
            Rule::Replace { .. } => text.to_string(),
            Rule::Insert { text: insert, at } => place(text, insert, *at),
            Rule::Remove { from, count, from_end } => {
                let chars: Vec<char> = text.chars().collect();
                let (start, end) = if *from_end {
                    let end = chars.len().saturating_sub(*from);
                    (end.saturating_sub(*count), end)
                } else {
                    let start = (*from).min(chars.len());
                    (start, start.saturating_add(*count).min(chars.len()))
                };
                chars[..start].iter().chain(&chars[end..]).collect()
            }
            Rule::Case { to } => change_case(text, *to),
            Rule::Counter { start, step, digits, separator, at } => {
                let value = start.saturating_add(step.saturating_mul(index as i64));
                let number = if value < 0 {
                    format!("-{:0width$}", value.unsigned_abs(), width = *digits)
                } else {
                    format!("{value:0width$}", width = *digits)
                };
                place(text, &with_separator(&number, separator, *at), *at)
            }
            Rule::Date { format, separator, at } => {
                let date = format_date(format, item.modified.unwrap_or_else(SystemTime::now));
                place(text, &with_separator(&date, separator, *at), *at)
            }
        },
    }
}

/// The new names for `items` (in order; counters count in this order).
/// `existing` is every name in the folder. Errors are bad rules.
pub fn plan(items: &[Item], steps: &[Step], existing: &[String]) -> Result<Vec<Planned>, String> {
    let ready = prepare(steps)?;
    // Names whose part before the extension ends up empty (".txt").
    let mut empty_stems = Vec::with_capacity(items.len());
    let mut planned: Vec<Planned> = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let (mut stem, mut extension) = split_name(&item.name, item.folder);
            for ((rule, scope), step) in ready.iter().zip(steps) {
                let literal = matches!(step.rule, Rule::Replace { matching, .. } if matching != Match::Regex);
                match scope {
                    Scope::Name => stem = apply_one(rule, &stem, index, item, literal),
                    Scope::Extension if !item.folder => extension = apply_one(rule, &extension, index, item, literal),
                    Scope::Extension => {}
                    Scope::Whole => {
                        let whole = apply_one(rule, &join_name(&stem, &extension), index, item, literal);
                        (stem, extension) = split_name(&whole, item.folder);
                    }
                }
            }
            empty_stems.push(stem.trim().is_empty());
            Planned { old: item.name.clone(), new: join_name(&stem, &extension), problem: None }
        })
        .collect();

    // macOS volumes usually ignore case: "A.txt" and "a.txt" clash.
    let key = |name: &str| name.to_lowercase();
    let renamed: HashSet<String> = planned.iter().map(|p| key(&p.old)).collect();
    let others: HashSet<String> = existing.iter().map(|n| key(n)).filter(|n| !renamed.contains(n)).collect();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for p in &planned {
        *counts.entry(key(&p.new)).or_default() += 1;
    }
    for (p, empty_stem) in planned.iter_mut().zip(empty_stems) {
        p.problem = if empty_stem || p.new.trim().is_empty() {
            Some(Problem::Empty)
        } else if p.new.contains('/') || p.new.contains(':') || p.new == "." || p.new == ".." {
            Some(Problem::Invalid)
        } else if counts.get(&key(&p.new)).copied().unwrap_or(0) > 1 {
            Some(Problem::Duplicate)
        } else if p.changes() && others.contains(&key(&p.new)) {
            Some(Problem::Taken)
        } else {
            None
        };
    }
    Ok(planned)
}

/// Renames in `dir` as planned (only changed names without problems).
/// Names that another item is leaving go through a temporary name first.
/// Returns the new paths and the failures.
pub fn apply(fs: &FsRef, dir: &Path, planned: &[Planned]) -> (Vec<PathBuf>, Vec<(String, io::Error)>) {
    let moves: Vec<&Planned> = planned.iter().filter(|p| p.changes() && p.problem.is_none()).collect();
    let leaving: HashSet<String> = moves.iter().map(|p| p.old.to_lowercase()).collect();
    let mut done = Vec::new();
    let mut failed = Vec::new();
    // First, items whose new name is still in use move aside. (A change
    // of case only also goes through a temporary name, for volumes that
    // ignore case.)
    let mut second: Vec<(PathBuf, &Planned)> = Vec::new();
    for (n, p) in moves.iter().enumerate() {
        let from = dir.join(&p.old);
        if leaving.contains(&p.new.to_lowercase()) {
            let aside = dir.join(format!(".forkz-rename-{}-{n}", std::process::id()));
            match fs.rename(&from, &aside) {
                Ok(()) => second.push((aside, p)),
                Err(e) => failed.push((p.old.clone(), e)),
            }
        } else {
            match fs.rename(&from, &dir.join(&p.new)) {
                Ok(()) => done.push(dir.join(&p.new)),
                Err(e) => failed.push((p.old.clone(), e)),
            }
        }
    }
    for (aside, p) in second {
        let to = dir.join(&p.new);
        match fs.rename(&aside, &to) {
            Ok(()) => done.push(to),
            Err(e) => {
                // Put it back where it was.
                let _ = fs.rename(&aside, &dir.join(&p.old));
                failed.push((p.old.clone(), e));
            }
        }
    }
    (done, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[&str]) -> Vec<Item> {
        names.iter().map(|n| Item { name: n.to_string(), modified: None, folder: !n.contains('.') }).collect()
    }

    fn step(rule: Rule) -> Step {
        Step { rule, scope: Scope::Name }
    }

    fn new_names(names: &[&str], steps: &[Step]) -> Vec<String> {
        plan(&items(names), steps, &[]).unwrap().into_iter().map(|p| p.new).collect()
    }

    #[test]
    fn replace_text_and_patterns() {
        let replace =
            |find: &str, with: &str, matching| step(Rule::Replace { find: find.into(), with: with.into(), matching });
        assert_eq!(
            new_names(&["IMG_001.JPG", "img_002.jpg"], &[replace("img_", "Photo ", Match::Text)]),
            ["Photo 001.JPG", "Photo 002.jpg"]
        );
        assert_eq!(
            new_names(&["IMG_001.JPG", "img_002.jpg"], &[replace("img_", "x", Match::TextMatchCase)]),
            ["IMG_001.JPG", "x002.jpg"]
        );
        // Dots and dollars are just text unless it is a pattern.
        assert_eq!(new_names(&["a.b.c.txt"], &[replace(".", "$1", Match::Text)]), ["a$1b$1c.txt"]);
        assert_eq!(
            new_names(&["2026-11-04 notes.md"], &[replace(r"(\d+)-(\d+)-(\d+)", "$3.$2.$1", Match::Regex)]),
            ["04.11.2026 notes.md"]
        );
        assert!(plan(&items(&["a.txt"]), &[replace("(", "", Match::Regex)], &[]).is_err());
        // An empty search changes nothing.
        assert_eq!(new_names(&["a.txt"], &[replace("", "x", Match::Text)]), ["a.txt"]);
    }

    #[test]
    fn insert_remove_and_case() {
        let insert = |text: &str, at| step(Rule::Insert { text: text.into(), at });
        assert_eq!(new_names(&["report.pdf"], &[insert("2026 ", Place::Start)]), ["2026 report.pdf"]);
        assert_eq!(new_names(&["report.pdf"], &[insert(" final", Place::End)]), ["report final.pdf"]);
        assert_eq!(new_names(&["report.pdf"], &[insert("-", Place::Index(3))]), ["rep-ort.pdf"]);
        assert_eq!(new_names(&["\u{e4}bc.txt"], &[insert("_", Place::Index(1))]), ["\u{e4}_bc.txt"]);
        let remove = |from, count, from_end| step(Rule::Remove { from, count, from_end });
        assert_eq!(new_names(&["IMG_0001.jpg"], &[remove(0, 4, false)]), ["0001.jpg"]);
        assert_eq!(new_names(&["photo copy.jpg"], &[remove(0, 5, true)]), ["photo.jpg"]);
        assert_eq!(new_names(&["ab.txt"], &[remove(5, 3, false)]), ["ab.txt"]);
        let case = |to| step(Rule::Case { to });
        assert_eq!(
            new_names(&["the QUICK-brown fox's den.TXT"], &[case(Case::Title)]),
            ["The Quick-Brown Fox's Den.TXT"]
        );
        assert_eq!(new_names(&["the QUICK brown.txt"], &[case(Case::Sentence)]), ["The quick brown.txt"]);
        let mut upper_extension = case(Case::Lower);
        upper_extension.scope = Scope::Extension;
        assert_eq!(new_names(&["Photo.JPG", "Folder"], &[upper_extension]), ["Photo.jpg", "Folder"]);
    }

    #[test]
    fn counters_and_dates() {
        let counter = |start, step_by, digits, at| {
            step(Rule::Counter { start, step: step_by, digits, separator: " ".into(), at })
        };
        assert_eq!(
            new_names(&["a.jpg", "b.jpg", "c.jpg"], &[counter(1, 1, 3, Place::Start)]),
            ["001 a.jpg", "002 b.jpg", "003 c.jpg"]
        );
        assert_eq!(new_names(&["a.jpg", "b.jpg"], &[counter(10, 5, 0, Place::End)]), ["a 10.jpg", "b 15.jpg"]);
        let mut whole = step(Rule::Insert { text: "Trip".into(), at: Place::Replace });
        whole.scope = Scope::Name;
        assert_eq!(
            new_names(&["x.jpg", "y.png"], &[whole, counter(1, 1, 2, Place::End)]),
            ["Trip 01.jpg", "Trip 02.png"]
        );

        // 2026-11-04 12:00 UTC: the date, in local time, is near it.
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_793_793_600);
        let formatted = format_date("YYYY-MM-DD hh.mm.ss (YY)", time);
        assert!(formatted.starts_with("2026-11-0"), "{formatted}");
        assert!(formatted.ends_with("(26)"), "{formatted}");
        let item = Item { name: "scan.pdf".into(), modified: Some(time), folder: false };
        let date = step(Rule::Date { format: "YYYY".into(), separator: "_".into(), at: Place::Start });
        assert_eq!(plan(&[item], &[date], &[]).unwrap()[0].new, "2026_scan.pdf");
    }

    #[test]
    fn whole_name_scope_can_change_the_extension() {
        let mut rule = step(Rule::Replace { find: ".jpeg".into(), with: ".jpg".into(), matching: Match::Text });
        rule.scope = Scope::Whole;
        assert_eq!(new_names(&["a.jpeg"], &[rule]), ["a.jpg"]);
    }

    #[test]
    fn problems() {
        let to = |text: &str| step(Rule::Insert { text: text.into(), at: Place::Replace });
        let planned = plan(&items(&["a.txt", "b.txt"]), &[to("same")], &[]).unwrap();
        assert!(planned.iter().all(|p| p.problem == Some(Problem::Duplicate)));
        let planned = plan(&items(&["a.txt"]), &[to("")], &[]).unwrap();
        assert_eq!(planned[0].problem, Some(Problem::Empty));
        let planned = plan(&items(&["a.txt"]), &[to("x/y")], &[]).unwrap();
        assert_eq!(planned[0].problem, Some(Problem::Invalid));
        // Taken by an item that is not being renamed (whatever its case).
        let existing = vec!["a.txt".to_string(), "B.TXT".to_string()];
        let planned = plan(&items(&["a.txt"]), &[to("b")], &existing).unwrap();
        assert_eq!(planned[0].problem, Some(Problem::Taken));
        // Not taken when that item is renamed too (here, a swap).
        let existing = vec!["a.txt".to_string(), "b.txt".to_string()];
        let swap = [
            step(Rule::Replace { find: "^(a|b)$".into(), with: "${1}_".into(), matching: Match::Regex }),
            step(Rule::Replace { find: "^a_$".into(), with: "b".into(), matching: Match::Regex }),
            step(Rule::Replace { find: "^b_$".into(), with: "a".into(), matching: Match::Regex }),
        ];
        let planned = plan(&items(&["a.txt", "b.txt"]), &swap, &existing).unwrap();
        assert_eq!(planned.iter().map(|p| p.new.as_str()).collect::<Vec<_>>(), ["b.txt", "a.txt"]);
        assert!(planned.iter().all(|p| p.problem.is_none()), "{planned:?}");
    }

    #[test]
    fn applying_handles_swaps_and_case_changes() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "Case.txt", "keep.txt"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let planned = vec![
            Planned { old: "a.txt".into(), new: "b.txt".into(), problem: None },
            Planned { old: "b.txt".into(), new: "a.txt".into(), problem: None },
            Planned { old: "Case.txt".into(), new: "case.txt".into(), problem: None },
            Planned { old: "keep.txt".into(), new: "bad".into(), problem: Some(Problem::Taken) },
        ];
        let (done, failed) = apply(&crate::vfs::local(), dir.path(), &planned);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(done.len(), 3);
        assert_eq!(std::fs::read_to_string(dir.path().join("b.txt")).unwrap(), "a.txt");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "b.txt");
        let mut names: Vec<String> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        names.sort();
        assert_eq!(names, ["a.txt", "b.txt", "case.txt", "keep.txt"]);
    }

    #[test]
    fn presets_round_trip_through_toml() {
        #[derive(Serialize, Deserialize)]
        struct Holder {
            preset: Vec<Preset>,
        }
        let preset = Preset {
            name: "Photos".into(),
            steps: vec![
                Step {
                    rule: Rule::Date { format: "YYYY-MM-DD".into(), separator: " ".into(), at: Place::Start },
                    scope: Scope::Name,
                },
                Step { rule: Rule::Case { to: Case::Lower }, scope: Scope::Extension },
                Step { rule: Rule::Insert { text: "x".into(), at: Place::Index(2) }, scope: Scope::Whole },
            ],
        };
        let text = toml::to_string(&Holder { preset: vec![preset.clone()] }).unwrap();
        let back: Holder = toml::from_str(&text).unwrap();
        assert_eq!(back.preset[0], preset);
    }
}
