//! Color themes for the file lists and the sidebar: built-in ones, in the
//! manner of a code editor's, and the user's own, which are TOML files in
//! `~/Library/Application Support/forkz/themes` (made with Settings >
//! Appearance > Customize, or by hand).
//!
//! The "system" choices (same as macOS, light, dark) are not themes: they
//! keep macOS's own colors.

use std::fmt;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A color, written "#rrggbb" in theme files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// "#rrggbb" or "rrggbb".
    pub fn parse(text: &str) -> Option<Rgb> {
        let hex = text.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    /// Components from 0 to 1.
    pub fn unit(self) -> (f64, f64, f64) {
        (f64::from(self.0) / 255.0, f64::from(self.1) / 255.0, f64::from(self.2) / 255.0)
    }

    pub fn from_unit(r: f64, g: f64, b: f64) -> Rgb {
        let byte = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Rgb(byte(r), byte(g), byte(b))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Rgb, D::Error> {
        let text = String::deserialize(deserializer)?;
        Rgb::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("\"{text}\" is not a #rrggbb color")))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// Dark controls and menus (macOS's dark appearance) with it.
    #[serde(default)]
    pub dark: bool,
    /// The file lists.
    pub background: Rgb,
    /// Every other row, when alternating rows are on.
    pub alternate: Rgb,
    pub text: Rgb,
    /// Dates, sizes, kinds, and other quieter text.
    pub secondary: Rgb,
    pub selection: Rgb,
    pub selection_text: Rgb,
    /// The active pane's line.
    pub accent: Rgb,
    pub sidebar: Rgb,
}

/// Reaches one of a theme's colors.
pub type ColorField = fn(&mut Theme) -> &mut Rgb;

/// The color names, in the editor's order, with how to get and set each.
pub const COLORS: [(&str, ColorField); 8] = [
    ("Background", |t| &mut t.background),
    ("Alternate rows", |t| &mut t.alternate),
    ("Text", |t| &mut t.text),
    ("Secondary text", |t| &mut t.secondary),
    ("Selection", |t| &mut t.selection),
    ("Selected text", |t| &mut t.selection_text),
    ("Accent", |t| &mut t.accent),
    ("Sidebar", |t| &mut t.sidebar),
];

/// Built-in themes: id, name, dark, then background, alternate, text,
/// secondary, selection, selected text, accent, sidebar. The palettes are
/// the themes' published ones.
const BUILT_IN: &[(&str, &str, bool, [&str; 8])] = &[
    (
        "github-light",
        "GitHub Light",
        false,
        ["#ffffff", "#f6f8fa", "#1f2328", "#656d76", "#0969da", "#ffffff", "#0969da", "#f6f8fa"],
    ),
    (
        "solarized-light",
        "Solarized Light",
        false,
        ["#fdf6e3", "#eee8d5", "#586e75", "#93a1a1", "#268bd2", "#fdf6e3", "#b58900", "#eee8d5"],
    ),
    (
        "catppuccin-latte",
        "Catppuccin Latte",
        false,
        ["#eff1f5", "#e6e9ef", "#4c4f69", "#8c8fa1", "#1e66f5", "#eff1f5", "#8839ef", "#e6e9ef"],
    ),
    (
        "solarized-dark",
        "Solarized Dark",
        true,
        ["#002b36", "#073642", "#93a1a1", "#657b83", "#268bd2", "#fdf6e3", "#b58900", "#073642"],
    ),
    (
        "dracula",
        "Dracula",
        true,
        ["#282a36", "#2d2f3d", "#f8f8f2", "#6272a4", "#44475a", "#f8f8f2", "#bd93f9", "#21222c"],
    ),
    ("nord", "Nord", true, ["#2e3440", "#353b48", "#d8dee9", "#7b88a1", "#5e81ac", "#eceff4", "#88c0d0", "#272c36"]),
    (
        "one-dark",
        "One Dark",
        true,
        ["#282c34", "#2c313a", "#abb2bf", "#5c6370", "#3e4451", "#ffffff", "#61afef", "#21252b"],
    ),
    (
        "monokai",
        "Monokai",
        true,
        ["#272822", "#2e2f29", "#f8f8f2", "#75715e", "#49483e", "#f8f8f2", "#a6e22e", "#1e1f1c"],
    ),
    (
        "gruvbox-dark",
        "Gruvbox Dark",
        true,
        ["#282828", "#32302f", "#ebdbb2", "#928374", "#504945", "#fbf1c7", "#fabd2f", "#1d2021"],
    ),
    (
        "tokyo-night",
        "Tokyo Night",
        true,
        ["#1a1b26", "#1f2030", "#c0caf5", "#565f89", "#283457", "#c0caf5", "#7aa2f7", "#16161e"],
    ),
    (
        "catppuccin-mocha",
        "Catppuccin Mocha",
        true,
        ["#1e1e2e", "#232334", "#cdd6f4", "#7f849c", "#45475a", "#cdd6f4", "#cba6f7", "#181825"],
    ),
];

/// The built-in themes, with their ids.
pub fn built_in() -> Vec<(String, Theme)> {
    BUILT_IN
        .iter()
        .map(|(id, name, dark, c)| {
            let color = |i: usize| Rgb::parse(c[i]).expect("built-in colors are valid");
            let theme = Theme {
                name: (*name).to_string(),
                dark: *dark,
                background: color(0),
                alternate: color(1),
                text: color(2),
                secondary: color(3),
                selection: color(4),
                selection_text: color(5),
                accent: color(6),
                sidebar: color(7),
            };
            ((*id).to_string(), theme)
        })
        .collect()
}

/// Where the user's themes are.
pub fn folder() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("forkz").join("themes"))
}

/// The user's themes ("user:" and the file's name, without ".toml"), by
/// name. Files that cannot be read are skipped (and logged).
pub fn user_themes() -> Vec<(String, Theme)> {
    folder().map(|dir| user_themes_in(&dir)).unwrap_or_default()
}

fn user_themes_in(dir: &std::path::Path) -> Vec<(String, Theme)> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut themes: Vec<(String, Theme)> = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .filter_map(|path| {
            let stem = path.file_stem()?.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).ok()?;
            match toml::from_str::<Theme>(&text) {
                Ok(theme) => Some((format!("user:{stem}"), theme)),
                Err(e) => {
                    tracing::warn!("theme {}: {e}", path.display());
                    None
                }
            }
        })
        .collect();
    themes.sort_by_key(|(_, t)| t.name.to_lowercase());
    themes
}

/// A theme by id (built-in or "user:..."); None for the system choices
/// and themes that are gone.
pub fn find(id: &str) -> Option<Theme> {
    if id.starts_with("user:") {
        user_themes().into_iter().find(|(i, _)| i == id).map(|(_, t)| t)
    } else {
        built_in().into_iter().find(|(i, _)| i == id).map(|(_, t)| t)
    }
}

/// A file name for a theme's name: letters, digits, and dashes.
pub fn file_stem(name: &str) -> String {
    let mut stem = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            stem.extend(c.to_lowercase());
        } else if !stem.ends_with('-') && !stem.is_empty() {
            stem.push('-');
        }
    }
    let stem = stem.trim_end_matches('-').to_string();
    if stem.is_empty() { "theme".into() } else { stem }
}

/// Saves a user theme (replacing one with the same file name); returns its
/// id.
pub fn save_user(theme: &Theme) -> io::Result<String> {
    let dir = folder().ok_or_else(|| io::Error::other("no Application Support folder"))?;
    save_user_in(&dir, theme)
}

fn save_user_in(dir: &std::path::Path, theme: &Theme) -> io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let stem = file_stem(&theme.name);
    let text = toml::to_string_pretty(theme).map_err(io::Error::other)?;
    std::fs::write(dir.join(format!("{stem}.toml")), text)?;
    Ok(format!("user:{stem}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_and_print() {
        assert_eq!(Rgb::parse("#0969DA"), Some(Rgb(9, 105, 218)));
        assert_eq!(Rgb::parse("ffffff"), Some(Rgb(255, 255, 255)));
        assert_eq!(Rgb::parse("#fff"), None);
        assert_eq!(Rgb::parse("#gggggg"), None);
        assert_eq!(Rgb(9, 105, 218).to_string(), "#0969da");
        assert_eq!(Rgb::from_unit(1.0, 0.5, -1.0), Rgb(255, 128, 0));
    }

    #[test]
    fn built_in_themes_are_complete_and_distinct() {
        let themes = built_in();
        assert!(themes.len() >= 10);
        let mut ids: Vec<&str> = themes.iter().map(|(i, _)| i.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), themes.len());
        assert!(themes.iter().any(|(_, t)| t.dark) && themes.iter().any(|(_, t)| !t.dark));
        assert!(find("dracula").is_some_and(|t| t.dark) && find("system").is_none());
    }

    #[test]
    fn user_themes_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes");
        let mut theme = find("nord").unwrap();
        theme.name = "My Nord!".into();
        *(COLORS[0].1)(&mut theme) = Rgb(1, 2, 3);
        let id = save_user_in(&themes, &theme).unwrap();
        assert_eq!(id, "user:my-nord");
        assert_eq!(user_themes_in(&themes), [(id, theme.clone())]);
        std::fs::write(themes.join("broken.toml"), "name = 3").unwrap();
        assert_eq!(user_themes_in(&themes).len(), 1, "a broken file is skipped");
        assert_eq!(file_stem("  Solarized / Dark  "), "solarized-dark");
        assert_eq!(file_stem("!!!"), "theme");
    }
}
