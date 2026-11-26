//! Splitting a large file into parts ("movie.mkv.001", ".002", and so on,
//! as 7-Zip, HJSplit, and `split` name them), and joining such parts back
//! into the file. Works on any file system (parts are streamed).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::vfs::FsRef;

/// "name.001" for part 1.
pub fn part_name(name: &str, number: usize) -> String {
    format!("{name}.{number:03}")
}

/// The file a first part makes when joined ("movie.mkv.001" makes
/// "movie.mkv"); None if it is not a first part.
pub fn joined_name(first: &str) -> Option<String> {
    let (name, number) = first.rsplit_once('.')?;
    (number.len() >= 3
        && number.chars().all(|c| c.is_ascii_digit())
        && number.parse::<u32>() == Ok(1)
        && !name.is_empty())
    .then(|| name.to_string())
}

/// Whether a name is a part ("x.001", "x.002", ...).
pub fn is_part(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, n)| !stem.is_empty() && n.len() >= 3 && n.chars().all(|c| c.is_ascii_digit()))
}

/// "100 MB", "1.5 GB", "700M", "4g", or a number of bytes.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim().to_lowercase().replace(' ', "");
    let digits = text.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let number: f64 = number.parse().ok()?;
    let multiplier: u64 = match unit.trim_end_matches("ib").trim_end_matches('b') {
        "" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => return None,
    };
    let bytes = (number * multiplier as f64) as u64;
    (bytes > 0).then_some(bytes)
}

/// The parts of a split file, from the first: "x.001", then "x.002" and
/// on as long as they exist.
pub fn parts_from(fs: &FsRef, first: &Path) -> Vec<PathBuf> {
    let Some(name) = first.file_name().map(|n| n.to_string_lossy().into_owned()) else { return Vec::new() };
    let Some(joined) = joined_name(&name) else { return Vec::new() };
    let width = name.len() - joined.len() - 1;
    let folder = first.parent().unwrap_or(Path::new("/"));
    let mut parts = Vec::new();
    for number in 1.. {
        let part = folder.join(format!("{joined}.{number:0width$}"));
        if fs.stat(&part).is_err() {
            break;
        }
        parts.push(part);
    }
    parts
}

/// Copies `limit` bytes (or all) from a reader to a writer, reporting
/// each chunk.
fn copy_some(from: &mut dyn Read, to: &mut dyn Write, limit: u64, progress: &mut dyn FnMut(u64)) -> io::Result<u64> {
    let mut buffer = vec![0u8; 1 << 20];
    let mut done = 0;
    while done < limit {
        let want = buffer.len().min((limit - done) as usize);
        let read = from.read(&mut buffer[..want])?;
        if read == 0 {
            break;
        }
        to.write_all(&buffer[..read])?;
        done += read as u64;
        progress(read as u64);
    }
    Ok(done)
}

/// Splits `path` (of `size` bytes) into parts of `part_size` beside it;
/// returns them. Parts already there are an error (nothing is replaced).
pub fn split(
    fs: &FsRef,
    path: &Path,
    size: u64,
    part_size: u64,
    progress: &mut dyn FnMut(u64),
) -> io::Result<Vec<PathBuf>> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let folder = path.parent().unwrap_or(Path::new("/"));
    let count = size.div_ceil(part_size.max(1)).max(1) as usize;
    let parts: Vec<PathBuf> = (1..=count).map(|n| folder.join(part_name(&name, n))).collect();
    if let Some(taken) = parts.iter().find(|p| fs.stat(p).is_ok()) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("\u{201c}{}\u{201d} is already there.", crate::pane::title_of(taken)),
        ));
    }
    let mut reader = fs.open_read(path)?;
    let mut written = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let this = part_size.min(size - index as u64 * part_size);
        let result = fs.open_write_sized(part, Some(this)).and_then(|mut writer| {
            let copied = copy_some(&mut reader, &mut writer, this, progress)?;
            writer.flush()?;
            if copied < this {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "The file ended early."));
            }
            Ok(())
        });
        if let Err(e) = result {
            // No half-split file left behind.
            for done in written.iter().chain([part]) {
                let _ = fs.remove_file(done);
            }
            return Err(e);
        }
        written.push(part.clone());
    }
    Ok(written)
}

/// Joins parts (in order) into `target`, which must not exist yet.
pub fn join(fs: &FsRef, parts: &[PathBuf], target: &Path, progress: &mut dyn FnMut(u64)) -> io::Result<()> {
    if fs.stat(target).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("\u{201c}{}\u{201d} is already there.", crate::pane::title_of(target)),
        ));
    }
    let mut total = 0;
    for part in parts {
        total += fs.stat(part)?.size;
    }
    let result = fs.open_write_sized(target, Some(total)).and_then(|mut writer| {
        for part in parts {
            let mut reader = fs.open_read(part)?;
            copy_some(&mut reader, &mut writer, u64::MAX, progress)?;
        }
        writer.flush()
    });
    if result.is_err() {
        let _ = fs.remove_file(target);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parts() {
        assert_eq!(part_name("movie.mkv", 2), "movie.mkv.002");
        assert_eq!(joined_name("movie.mkv.001").as_deref(), Some("movie.mkv"));
        assert_eq!(joined_name("movie.mkv.0001").as_deref(), Some("movie.mkv"));
        assert_eq!(joined_name("movie.mkv.002"), None);
        assert_eq!(joined_name("movie.mkv"), None);
        assert!(is_part("a.zip.017") && !is_part("a.mp3"));
        assert_eq!(parse_size("100 MB"), Some(100 << 20));
        assert_eq!(parse_size("1.5g"), Some(3 << 29));
        assert_eq!(parse_size("4 GiB"), Some(4 << 30));
        assert_eq!(parse_size("512"), Some(512));
        assert_eq!(parse_size("ten"), None);
        assert_eq!(parse_size("0"), None);
    }

    #[test]
    fn splits_and_joins() {
        let dir = tempfile::tempdir().unwrap();
        let fs = crate::vfs::local();
        let data: Vec<u8> = (0..2500u32).map(|i| (i % 251) as u8).collect();
        let path = dir.path().join("big.bin");
        std::fs::write(&path, &data).unwrap();
        let mut seen = 0;
        let parts = split(&fs, &path, data.len() as u64, 1000, &mut |n| seen += n).unwrap();
        assert_eq!(seen, 2500);
        let names: Vec<String> = parts.iter().map(|p| crate::pane::title_of(p)).collect();
        assert_eq!(names, ["big.bin.001", "big.bin.002", "big.bin.003"]);
        assert_eq!(std::fs::metadata(&parts[2]).unwrap().len(), 500);
        // Not twice.
        assert!(split(&fs, &path, data.len() as u64, 1000, &mut |_| {}).is_err());

        std::fs::remove_file(&path).unwrap();
        let found = parts_from(&fs, &parts[0]);
        assert_eq!(found, parts);
        join(&fs, &found, &path, &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert!(join(&fs, &found, &path, &mut |_| {}).is_err(), "nothing replaced");
    }
}
