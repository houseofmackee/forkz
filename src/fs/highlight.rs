//! Syntax highlighting for the viewer and the preview: comments, strings,
//! numbers, keywords, and keys (in JSON, YAML, TOML, and INI files), tags
//! (HTML and XML), and headings (Markdown). A simple scanner, line by line
//! with block comments and multi-line strings carried over: fast, and good
//! enough to read and edit code and settings, without a grammar per
//! language. Ranges are in UTF-16 units, as AppKit's text counts them.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Comment,
    String,
    Number,
    Keyword,
    /// A key in JSON, YAML, TOML, or INI; a tag or attribute in HTML.
    Key,
    /// A Markdown heading, a section in TOML or INI.
    Heading,
}

/// A highlighted range: (start, length) in UTF-16 units, and its kind.
pub type Span = (usize, usize, Kind);

/// How a language's text is scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    pub name: &'static str,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    /// Quote characters for strings ('`' too for JavaScript and Go).
    quotes: &'static [char],
    /// Python's and Swift's """ strings.
    triple_quotes: bool,
    keywords: &'static [&'static str],
    style: Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Code,
    /// Keys before ":" (JSON, YAML) or "=" (TOML, INI), and [sections].
    Config,
    /// Tags and attributes.
    Markup,
    Markdown,
}

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
    "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "Some", "None", "Ok",
    "Err",
];
const PYTHON: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
    "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None", "nonlocal", "not", "or",
    "pass", "raise", "return", "self", "True", "try", "while", "with", "yield",
];
const JS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
];
const C: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "if",
    "include",
    "define",
    "int",
    "long",
    "namespace",
    "nullptr",
    "public",
    "private",
    "protected",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "true",
    "typedef",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "while",
    "import",
    "interface",
    "implementation",
    "end",
    "self",
    "nil",
    "YES",
    "NO",
];
const SWIFT: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "do",
    "else",
    "enum",
    "extension",
    "false",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "is",
    "let",
    "nil",
    "private",
    "protocol",
    "public",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "switch",
    "throw",
    "throws",
    "true",
    "try",
    "var",
    "where",
    "while",
];
const GO: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "false",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];
const JAVA: &[&str] = &[
    "abstract",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "data",
    "default",
    "do",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "for",
    "fun",
    "if",
    "implements",
    "import",
    "interface",
    "is",
    "new",
    "null",
    "object",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "val",
    "var",
    "void",
    "when",
    "while",
];
const RUBY: &[&str] = &[
    "alias", "and", "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure", "false", "for",
    "if", "in", "module", "next", "nil", "not", "or", "require", "rescue", "return", "self", "then", "true", "unless",
    "until", "when", "while", "yield",
];
const PHP: &[&str] = &[
    "array",
    "as",
    "break",
    "case",
    "class",
    "const",
    "continue",
    "default",
    "echo",
    "else",
    "elseif",
    "extends",
    "false",
    "foreach",
    "for",
    "function",
    "if",
    "implements",
    "include",
    "namespace",
    "new",
    "null",
    "private",
    "protected",
    "public",
    "require",
    "return",
    "static",
    "switch",
    "this",
    "true",
    "use",
    "while",
];
const SHELL: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local", "return",
    "then", "until", "while", "echo", "exit", "set", "unset", "source",
];
const SQL: &[&str] = &[
    "SELECT", "FROM", "WHERE", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "CREATE", "TABLE", "DROP",
    "ALTER", "INDEX", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "ON", "AS", "AND", "OR", "NOT", "NULL", "IS", "IN",
    "ORDER", "BY", "GROUP", "HAVING", "LIMIT", "PRIMARY", "KEY", "DISTINCT", "UNION", "select", "from", "where",
    "insert", "into", "values", "update", "set", "delete", "create", "table", "join", "on", "as", "and", "or", "not",
    "null", "order", "by", "group", "limit",
];
const CSS: &[&str] = &["important", "media", "import", "keyframes", "font-face", "root"];
const CONFIG: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

const fn code(
    name: &'static str,
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
    keywords: &'static [&'static str],
) -> Language {
    Language { name, line_comments, block_comment, quotes, triple_quotes: false, keywords, style: Style::Code }
}

const SLASHES: Option<(&str, &str)> = Some(("/*", "*/"));

/// The language of a file, by its name (None: plain text).
pub fn language_for(name: &str) -> Option<Language> {
    let lower = name.to_lowercase();
    let extension = lower.rsplit_once('.').map_or(lower.as_str(), |(_, e)| e);
    let special = match lower.as_str() {
        "makefile" | "dockerfile" | ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" | ".zprofile" => {
            Some(code("Shell", &["#"], None, &['"', '\''], SHELL))
        }
        ".gitignore" | ".gitattributes" | ".editorconfig" | ".env" => Some(config("Config", &["#"], CONFIG)),
        _ => None,
    };
    if special.is_some() {
        return special;
    }
    Some(match extension {
        "rs" => code("Rust", &["//"], SLASHES, &['"'], RUST),
        "py" | "pyw" => Language { triple_quotes: true, ..code("Python", &["#"], None, &['"', '\''], PYTHON) },
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" => code("JavaScript", &["//"], SLASHES, &['"', '\'', '`'], JS),
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" | "cs" => {
            code("C", &["//"], SLASHES, &['"', '\''], C)
        }
        "swift" => Language { triple_quotes: true, ..code("Swift", &["//"], SLASHES, &['"'], SWIFT) },
        "go" => code("Go", &["//"], SLASHES, &['"', '`'], GO),
        "java" | "kt" | "kts" | "scala" | "groovy" | "gradle" => code("Java", &["//"], SLASHES, &['"', '\''], JAVA),
        "rb" | "rake" | "gemspec" => code("Ruby", &["#"], None, &['"', '\''], RUBY),
        "php" => code("PHP", &["//", "#"], SLASHES, &['"', '\''], PHP),
        "sh" | "bash" | "zsh" | "fish" | "command" => code("Shell", &["#"], None, &['"', '\''], SHELL),
        "sql" => code("SQL", &["--"], SLASHES, &['\''], SQL),
        "css" | "scss" | "less" => code("CSS", &["//"], SLASHES, &['"', '\''], CSS),
        "lua" => code(
            "Lua",
            &["--"],
            None,
            &['"', '\''],
            &[
                "and", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local", "nil", "not",
                "or", "return", "then", "true", "while",
            ],
        ),
        "json" | "jsonc" | "json5" => config("JSON", &["//"], CONFIG),
        "yaml" | "yml" => config("YAML", &["#"], CONFIG),
        "toml" | "ini" | "cfg" | "conf" | "properties" | "plist-text" => config("Config", &["#", ";"], CONFIG),
        "html" | "htm" | "xml" | "plist" | "svg" | "xhtml" | "vue" => Language {
            name: "Markup",
            line_comments: &[],
            block_comment: Some(("<!--", "-->")),
            quotes: &['"', '\''],
            triple_quotes: false,
            keywords: &[],
            style: Style::Markup,
        },
        "md" | "markdown" => Language {
            name: "Markdown",
            line_comments: &[],
            block_comment: Some(("<!--", "-->")),
            quotes: &['`'],
            triple_quotes: false,
            keywords: &[],
            style: Style::Markdown,
        },
        _ => return None,
    })
}

const fn config(
    name: &'static str,
    line_comments: &'static [&'static str],
    keywords: &'static [&'static str],
) -> Language {
    Language {
        name,
        line_comments,
        block_comment: None,
        quotes: &['"', '\''],
        triple_quotes: false,
        keywords,
        style: Style::Config,
    }
}

/// UTF-16 length of a string slice.
fn units(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The highlighted ranges of a text.
pub fn highlight(text: &str, language: &Language) -> Vec<Span> {
    let mut spans = Vec::new();
    // Inside a block comment or a triple-quoted string from an earlier line.
    let mut open_block: Option<(&str, Kind)> = None;
    let mut line_start = 0usize; // UTF-16 offset of the line
    for line in text.split_inclusive('\n') {
        highlight_line(line, line_start, language, &mut open_block, &mut spans);
        line_start += units(line);
    }
    spans
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn highlight_line<'a>(
    line: &'a str,
    base: usize,
    language: &Language,
    open_block: &mut Option<(&'a str, Kind)>,
    spans: &mut Vec<Span>,
) where
    'static: 'a,
{
    // (byte index, char) pairs, and UTF-16 offsets for byte indexes.
    let at = |byte: usize| base + units(&line[..byte]);
    let mut i = 0;
    // An open block comment or string continues to its end.
    if let Some((end, kind)) = *open_block {
        match line.find(end) {
            Some(found) => {
                let stop = found + end.len();
                spans.push((base, at(stop) - base, kind));
                *open_block = None;
                i = stop;
            }
            None => {
                spans.push((base, units(line.trim_end_matches('\n')), kind));
                return;
            }
        }
    }
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    // Whole-line forms.
    match language.style {
        Style::Markdown if trimmed.starts_with('#') => {
            spans.push((at(indent), units(trimmed.trim_end()), Kind::Heading));
            return;
        }
        Style::Config if trimmed.starts_with('[') && !language.name.eq("JSON") && !language.name.eq("YAML") => {
            spans.push((at(indent), units(trimmed.trim_end()), Kind::Heading));
            return;
        }
        _ => {}
    }
    let mut expect_key = language.style == Style::Config && i == 0;
    while i < line.len() {
        let rest = &line[i..];
        let c = rest.chars().next().unwrap_or(' ');
        if c == '\n' {
            break;
        }
        // Line comments (not inside words, so URLs keep their "//").
        if let Some(marker) = language.line_comments.iter().find(|m| rest.starts_with(**m)) {
            let after_colon = marker == &"//" && i > 0 && line[..i].ends_with(':');
            if !after_colon {
                spans.push((at(i), units(rest.trim_end_matches('\n')), Kind::Comment));
                return;
            }
        }
        if let Some((start, end)) = language.block_comment
            && rest.starts_with(start)
        {
            match rest[start.len()..].find(end) {
                Some(found) => {
                    let stop = i + start.len() + found + end.len();
                    spans.push((at(i), at(stop) - at(i), Kind::Comment));
                    i = stop;
                    continue;
                }
                None => {
                    spans.push((at(i), units(rest.trim_end_matches('\n')), Kind::Comment));
                    *open_block = Some((end, Kind::Comment));
                    return;
                }
            }
        }
        if language.triple_quotes && (rest.starts_with("\"\"\"") || rest.starts_with("'''")) {
            let quote = &rest[..3];
            let end: &'static str = if quote == "\"\"\"" { "\"\"\"" } else { "'''" };
            match rest[3..].find(quote) {
                Some(found) => {
                    let stop = i + 3 + found + 3;
                    spans.push((at(i), at(stop) - at(i), Kind::String));
                    i = stop;
                    continue;
                }
                None => {
                    spans.push((at(i), units(rest.trim_end_matches('\n')), Kind::String));
                    *open_block = Some((end, Kind::String));
                    return;
                }
            }
        }
        if language.quotes.contains(&c) {
            // To the closing quote (backslash escapes), or the line's end.
            let mut stop = i + c.len_utf8();
            let mut escaped = false;
            for (offset, ch) in line[stop..].char_indices() {
                if ch == '\n' {
                    break;
                }
                stop = i + c.len_utf8() + offset + ch.len_utf8();
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == c {
                    break;
                }
            }
            // A quoted key: "name": or "name" =
            let after = line[stop..].trim_start();
            let kind = if language.style == Style::Config && (after.starts_with(':') || after.starts_with('=')) {
                Kind::Key
            } else if language.style == Style::Markdown {
                Kind::Number
            } else {
                Kind::String
            };
            spans.push((at(i), at(stop) - at(i), kind));
            i = stop;
            expect_key = false;
            continue;
        }
        if language.style == Style::Markup && c == '<' {
            // The tag's name, then attribute names (strings by quotes).
            let name_end = rest[1..]
                .find(|ch: char| ch.is_whitespace() || ch == '>' || ch == '/' && !rest[1..].starts_with('/'))
                .map_or(rest.len(), |p| p + 1);
            let stop = i + name_end.max(1);
            spans.push((at(i), at(stop) - at(i), Kind::Keyword));
            i = stop;
            continue;
        }
        if language.style == Style::Markup && is_word(c) && line[i..].contains('=') {
            let word_end = rest.find(|ch: char| !(is_word(ch) || ch == '-' || ch == ':')).unwrap_or(rest.len());
            if rest[word_end..].starts_with('=') {
                spans.push((at(i), units(&rest[..word_end]), Kind::Key));
                i += word_end;
                continue;
            }
        }
        if c.is_ascii_digit() && (i == 0 || !is_word(line[..i].chars().last().unwrap_or(' '))) {
            let end =
                rest.find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_')).unwrap_or(rest.len());
            spans.push((at(i), units(&rest[..end]), Kind::Number));
            i += end;
            expect_key = false;
            continue;
        }
        if is_word(c) {
            let end = rest.find(|ch: char| !(is_word(ch) || ch == '-' || ch == '.')).unwrap_or(rest.len());
            let word = &rest[..end];
            let after = rest[end..].trim_start();
            if language.style == Style::Config && expect_key && (after.starts_with(':') || after.starts_with('=')) {
                spans.push((at(i), units(word), Kind::Key));
            } else if language.keywords.contains(&word) {
                spans.push((at(i), units(word), Kind::Keyword));
            }
            i += end.max(c.len_utf8());
            expect_key = false;
            continue;
        }
        if !c.is_whitespace() && c != '-' {
            expect_key = false;
        }
        i += c.len_utf8();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str, file: &str) -> Vec<(String, Kind)> {
        let language = language_for(file).unwrap();
        let utf16: Vec<u16> = text.encode_utf16().collect();
        highlight(text, &language)
            .into_iter()
            .map(|(start, len, kind)| (String::from_utf16_lossy(&utf16[start..start + len]), kind))
            .collect()
    }

    #[test]
    fn highlights_code() {
        let spans = kinds("let s = \"a // b\"; // note\nfn x() { 42 }\n/* one\ntwo */ pub", "main.rs");
        assert_eq!(
            spans,
            vec![
                ("let".into(), Kind::Keyword),
                ("\"a // b\"".into(), Kind::String),
                ("// note".into(), Kind::Comment),
                ("fn".into(), Kind::Keyword),
                ("42".into(), Kind::Number),
                ("/* one".into(), Kind::Comment),
                ("two */".into(), Kind::Comment),
                ("pub".into(), Kind::Keyword),
            ]
        );
        // Python's triple-quoted strings across lines.
        let spans = kinds("def f():\n    \"\"\"Doc\n    more\"\"\"\n    return 1", "a.py");
        assert!(spans.contains(&("\"\"\"Doc".into(), Kind::String)));
        assert!(spans.contains(&("    more\"\"\"".into(), Kind::String)));
        assert!(spans.contains(&("return".into(), Kind::Keyword)));
        // Not part of a word, and URLs are not comments.
        assert!(kinds("x1 = letter", "a.js").is_empty());
        assert_eq!(kinds("u = \"x\"; // ok", "a.js")[1], ("// ok".into(), Kind::Comment));
        assert!(kinds("see https://example.com", "a.rs").is_empty());
    }

    #[test]
    fn highlights_settings_and_markup() {
        assert_eq!(
            kinds("[server]\nport = 8080 # main\nname = \"x\"", "a.toml"),
            vec![
                ("[server]".into(), Kind::Heading),
                ("port".into(), Kind::Key),
                ("8080".into(), Kind::Number),
                ("# main".into(), Kind::Comment),
                ("name".into(), Kind::Key),
                ("\"x\"".into(), Kind::String),
            ]
        );
        assert_eq!(
            kinds("{\"a\": true, \"b\": \"c\"}", "x.json"),
            vec![
                ("\"a\"".into(), Kind::Key),
                ("true".into(), Kind::Keyword),
                ("\"b\"".into(), Kind::Key),
                ("\"c\"".into(), Kind::String),
            ]
        );
        assert_eq!(kinds("  - name: web", "a.yml")[0], ("name".into(), Kind::Key));
        let html = kinds("<a href=\"/x\">é</a> <!-- c -->", "i.html");
        assert_eq!(html[0], ("<a".into(), Kind::Keyword));
        assert_eq!(html[1], ("href".into(), Kind::Key));
        assert_eq!(html[2], ("\"/x\"".into(), Kind::String));
        assert!(html.contains(&("<!-- c -->".into(), Kind::Comment)));
        assert_eq!(kinds("# Title\ntext `code`", "r.md")[0], ("# Title".into(), Kind::Heading));
        // Offsets count UTF-16 units (an emoji is two).
        let language = language_for("a.rs").unwrap();
        assert_eq!(highlight("\u{1f600} 7", &language), vec![(3, 1, Kind::Number)]);
        assert!(language_for("photo.jpg").is_none());
        assert_eq!(language_for("Makefile").map(|l| l.name), Some("Shell"));
    }
}
