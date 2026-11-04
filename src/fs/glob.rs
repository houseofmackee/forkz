//! Name patterns for Quick Select and search: `*` (any text), `?` (one
//! character), and `[abc]` or `[a-z]` (one of these), ignoring case. A
//! pattern without wildcards matches names that contain it. Several
//! patterns can be given, separated by `;` or `,`.

/// Matches names against one or more patterns.
#[derive(Debug, Clone)]
pub struct Patterns {
    patterns: Vec<Vec<char>>,
}

impl Patterns {
    pub fn new(text: &str) -> Patterns {
        let patterns = text
            .split([';', ','])
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| {
                let lower = p.to_lowercase();
                // Plain text: names containing it.
                let wildcards = lower.contains(['*', '?']) || (lower.contains('[') && lower.contains(']'));
                let wild = if wildcards { lower } else { format!("*{lower}*") };
                wild.chars().collect()
            })
            .collect();
        Patterns { patterns }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.to_lowercase().chars().collect();
        self.patterns.iter().any(|p| matches(p, &name))
    }
}

fn matches(pattern: &[char], name: &[char]) -> bool {
    // Backtracking on the last `*`: linear for the usual patterns.
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        let set = if pattern.get(p) == Some(&'[') { class(pattern, p, name[n]) } else { None };
        if let Some((set_end, hit)) = set {
            if hit {
                p = set_end + 1;
                n += 1;
            } else if let Some((sp, sn)) = star {
                p = sp + 1;
                n = sn + 1;
                star = Some((sp, sn + 1));
            } else {
                return false;
            }
            continue;
        }
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some('?') => {
                p += 1;
                n += 1;
            }
            Some(c) if *c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((sp, sn)) => {
                    p = sp + 1;
                    n = sn + 1;
                    star = Some((sp, sn + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// A `[...]` class starting at `start`: where it ends, and whether `c` is
/// in it. None if it is not closed (then `[` is just a character).
fn class(pattern: &[char], start: usize, c: char) -> Option<(usize, bool)> {
    let end = start + 1 + pattern[start + 1..].iter().position(|x| *x == ']')?;
    let set = &pattern[start + 1..end];
    let mut hit = false;
    let mut i = 0;
    while i < set.len() {
        if i + 2 < set.len() && set[i + 1] == '-' {
            hit |= (set[i]..=set[i + 2]).contains(&c);
            i += 3;
        } else {
            hit |= set[i] == c;
            i += 1;
        }
    }
    Some((end, hit))
}

#[cfg(test)]
mod tests {
    use super::Patterns;

    #[test]
    fn wildcards_classes_and_plain_text() {
        let p = Patterns::new("*.jpg; *.PNG");
        assert!(p.matches("Photo.JPG") && p.matches("a.png") && !p.matches("a.jpeg") && !p.matches("jpg.txt"));
        let p = Patterns::new("IMG_00??.*");
        assert!(p.matches("img_0012.heic") && !p.matches("img_001.heic"));
        let p = Patterns::new("report[0-9]*");
        assert!(p.matches("report7 final.pdf") && !p.matches("reportx.pdf"));
        let p = Patterns::new("[");
        assert!(p.matches("a[b") && !p.matches("ab"));
        let p = Patterns::new("notes");
        assert!(p.matches("My Notes.txt") && !p.matches("note.txt"));
        let p = Patterns::new("a*b*c");
        assert!(p.matches("axxbyyc") && p.matches("abc") && !p.matches("acb"));
        assert!(Patterns::new(" ; ,").is_empty());
        assert!(Patterns::new("*").matches(""));
    }
}
