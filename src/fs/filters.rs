//! Filename filters (Settings > Filters): named sets of patterns that hide
//! items in the lists, leave them out of copies and moves, or both, like
//! FileZilla's filters. View > Use Filename Filters turns them all off.

use serde::{Deserialize, Serialize};

use super::glob::Patterns;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NameFilter {
    pub name: String,
    /// Patterns separated by `;` or `,`: `*` and `?` wildcards; a plain
    /// name matches only that name. Case does not matter.
    pub patterns: String,
    pub files: bool,
    pub folders: bool,
    /// Hide matching items in the lists.
    pub listings: bool,
    /// Leave matching items out of copies and moves (inside the folders
    /// copied: what is chosen directly is always copied).
    pub transfers: bool,
}

impl Default for NameFilter {
    fn default() -> Self {
        NameFilter {
            name: "New filter".into(),
            patterns: String::new(),
            files: true,
            folders: true,
            listings: false,
            transfers: false,
        }
    }
}

/// The filters forkz starts with (all off).
pub fn defaults() -> Vec<NameFilter> {
    let filter = |name: &str, patterns: &str, files: bool, folders: bool| NameFilter {
        name: name.into(),
        patterns: patterns.into(),
        files,
        folders,
        listings: false,
        transfers: false,
    };
    vec![
        filter("macOS metadata", ".DS_Store; ._*; .Spotlight-V100; .Trashes; .fseventsd; .TemporaryItems", true, true),
        filter("Version control folders", ".git; .svn; .hg; CVS", false, true),
        filter("Build output", "node_modules; target; build; dist; __pycache__; *.o; *.pyc; *.class", true, true),
        filter("Temporary and backup files", "*~; *.bak; *.tmp; *.swp; .#*", true, false),
    ]
}

/// What the filters in use leave out.
#[derive(Debug, Clone)]
pub struct Exclusions {
    files: Patterns,
    folders: Patterns,
}

impl Exclusions {
    /// From the filters `pick` chooses (those for listings, or for
    /// transfers); None when they leave nothing out.
    pub fn new(filters: &[NameFilter], pick: impl Fn(&NameFilter) -> bool) -> Option<Exclusions> {
        let join = |want: fn(&NameFilter) -> bool| {
            filters.iter().filter(|f| pick(f) && want(f)).map(|f| f.patterns.as_str()).collect::<Vec<_>>().join(";")
        };
        let files = Patterns::exact(&join(|f| f.files));
        let folders = Patterns::exact(&join(|f| f.folders));
        (!files.is_empty() || !folders.is_empty()).then_some(Exclusions { files, folders })
    }

    pub fn excludes(&self, name: &str, is_folder: bool) -> bool {
        if is_folder { self.folders.matches(name) } else { self.files.matches(name) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_leave_out_what_they_name() {
        let mut filters = defaults();
        assert!(Exclusions::new(&filters, |f| f.listings).is_none(), "all off to start");
        filters[1].listings = true; // version control folders
        filters[2].transfers = true; // build output
        let listing = Exclusions::new(&filters, |f| f.listings).unwrap();
        assert!(listing.excludes(".git", true));
        assert!(!listing.excludes(".git", false), "folders only");
        assert!(!listing.excludes(".github", true), "whole names");
        let transfer = Exclusions::new(&filters, |f| f.transfers).unwrap();
        assert!(transfer.excludes("node_modules", true) && transfer.excludes("Main.O", false));
        assert!(!transfer.excludes("targets.txt", false) && !transfer.excludes("src", true));
    }

    #[test]
    fn plain_and_exact_patterns() {
        assert!(Patterns::new("arg").matches("target"), "search: contains");
        assert!(!Patterns::exact("arg").matches("target"));
        assert!(Patterns::exact("TARGET; *.o").matches("target") && Patterns::exact("*.o").matches("a.o"));
    }
}
