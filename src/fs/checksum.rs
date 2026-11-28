//! Checksums of files on any file system, read in chunks (large files and
//! server files work without loading them whole).

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::vfs::FsRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha256,
    Md5,
    Sha1,
    Sha384,
    Sha512,
}

impl Algorithm {
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "SHA-256",
            Algorithm::Md5 => "MD5",
            Algorithm::Sha1 => "SHA-1",
            Algorithm::Sha384 => "SHA-384",
            Algorithm::Sha512 => "SHA-512",
        }
    }
}

/// The checksum of a file, as lowercase hex.
pub fn checksum(fs: &FsRef, path: &Path, algorithm: Algorithm) -> io::Result<String> {
    let mut reader = fs.open_read(path)?;
    match algorithm {
        Algorithm::Sha256 => digest::<Sha256>(&mut reader),
        Algorithm::Md5 => digest::<Md5>(&mut reader),
        Algorithm::Sha1 => digest::<Sha1>(&mut reader),
        Algorithm::Sha384 => digest::<Sha384>(&mut reader),
        Algorithm::Sha512 => digest::<Sha512>(&mut reader),
    }
}

fn digest<D: Digest>(reader: &mut dyn Read) -> io::Result<String> {
    let mut hasher = D::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

impl Algorithm {
    pub const ALL: [Algorithm; 5] =
        [Algorithm::Sha256, Algorithm::Md5, Algorithm::Sha1, Algorithm::Sha384, Algorithm::Sha512];

    /// The usual extension of its checksum files ("sha256").
    pub fn extension(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Md5 => "md5",
            Algorithm::Sha1 => "sha1",
            Algorithm::Sha384 => "sha384",
            Algorithm::Sha512 => "sha512",
        }
    }

    /// The algorithm a hex hash of this length comes from.
    pub fn for_hash(hash: &str) -> Option<Algorithm> {
        if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some(match hash.len() {
            32 => Algorithm::Md5,
            40 => Algorithm::Sha1,
            64 => Algorithm::Sha256,
            96 => Algorithm::Sha384,
            128 => Algorithm::Sha512,
            _ => return None,
        })
    }
}

/// Whether a file is a checksum file ("x.sha256", "SHA256SUMS").
pub fn is_checksum_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    Algorithm::ALL.iter().any(|a| lower.ends_with(&format!(".{}", a.extension())))
        || lower.ends_with("sums")
        || lower.ends_with("sums.txt")
}

/// A checksum file's lines: (hash, path relative to its folder). Reads
/// "hash  path" and "hash *path" (shasum, md5sum) and "SHA256 (path) =
/// hash" (BSD).
pub fn parse_checksum_file(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        // BSD: "SHA256 (name) = hash".
        if let Some((left, hash)) = line.rsplit_once(") = ")
            && let Some((_, path)) = left.split_once(" (")
            && Algorithm::for_hash(hash.trim()).is_some()
        {
            found.push((hash.trim().to_lowercase(), path.to_string()));
            continue;
        }
        // GNU: "hash  name" or "hash *name" (binary mode).
        if let Some((hash, rest)) = line.split_once(' ')
            && Algorithm::for_hash(hash).is_some()
        {
            let path = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('*')).unwrap_or(rest);
            if !path.is_empty() {
                found.push((hash.to_lowercase(), path.to_string()));
            }
        }
    }
    found
}

/// A checksum file: "hash  path" lines, as shasum writes them.
pub fn checksum_file_text(lines: &[(String, String)]) -> String {
    lines.iter().map(|(hash, path)| format!("{hash}  {path}\n")).collect()
}

/// The files to list for `items` (folders: everything in them), each with
/// its path relative to `base`, in order.
pub fn files_below(fs: &FsRef, base: &Path, items: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending: Vec<PathBuf> = items.iter().rev().cloned().collect();
    while let Some(path) = pending.pop() {
        let Ok(entry) = fs.stat(&path) else { continue };
        if entry.kind == crate::fs::EntryKind::Folder {
            let mut children = fs.list(&path).unwrap_or_default();
            children.sort_by(|a, b| crate::fs::natural_cmp(&b.name, &a.name));
            pending.extend(children.into_iter().map(|c| c.path));
        } else if entry.kind == crate::fs::EntryKind::File {
            let relative = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().into_owned();
            found.push((path, relative));
        }
    }
    found
}

/// What checking files against their hashes found.
#[derive(Debug, Default, PartialEq)]
pub struct Verified {
    pub matching: Vec<String>,
    pub different: Vec<String>,
    /// Not there, or could not be read: (path, why).
    pub missing: Vec<(String, String)>,
}

/// Checks the files of a checksum file (paths relative to `base`).
pub fn verify(fs: &FsRef, base: &Path, lines: &[(String, String)]) -> Verified {
    let mut verified = Verified::default();
    for (hash, relative) in lines {
        let Some(algorithm) = Algorithm::for_hash(hash) else { continue };
        match checksum(fs, &base.join(relative), algorithm) {
            Ok(actual) if actual.eq_ignore_ascii_case(hash) => verified.matching.push(relative.clone()),
            Ok(_) => verified.different.push(relative.clone()),
            Err(e) => verified.missing.push((relative.clone(), e.to_string())),
        }
    }
    verified
}

/// A checksum a server keeps for its files, to check a copy against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Md5,
    Sha1,
    Sha256,
    /// Dropbox's content hash: SHA-256 of the SHA-256 of each 4 MiB block.
    Dropbox,
    /// Microsoft's QuickXorHash (OneDrive and SharePoint).
    QuickXor,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Md5 => "MD5",
            Kind::Sha1 => "SHA-1",
            Kind::Sha256 => "SHA-256",
            Kind::Dropbox => "content hash",
            Kind::QuickXor => "QuickXorHash",
        }
    }
}

/// Dropbox hashes files in blocks of this size.
const DROPBOX_BLOCK: usize = 4 * 1024 * 1024;

/// Hashes bytes as they stream past (a copy on its way).
pub enum StreamHasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
    Dropbox { block: Sha256, in_block: usize, digests: Vec<u8> },
    QuickXor(QuickXor),
}

impl StreamHasher {
    pub fn new(kind: Kind) -> StreamHasher {
        match kind {
            Kind::Md5 => StreamHasher::Md5(Md5::new()),
            Kind::Sha1 => StreamHasher::Sha1(Sha1::new()),
            Kind::Sha256 => StreamHasher::Sha256(Sha256::new()),
            Kind::Dropbox => StreamHasher::Dropbox { block: Sha256::new(), in_block: 0, digests: Vec::new() },
            Kind::QuickXor => StreamHasher::QuickXor(QuickXor::default()),
        }
    }

    pub fn update(&mut self, mut bytes: &[u8]) {
        match self {
            StreamHasher::Md5(h) => h.update(bytes),
            StreamHasher::Sha1(h) => h.update(bytes),
            StreamHasher::Sha256(h) => h.update(bytes),
            StreamHasher::QuickXor(h) => h.update(bytes),
            StreamHasher::Dropbox { block, in_block, digests } => {
                while !bytes.is_empty() {
                    let take = (DROPBOX_BLOCK - *in_block).min(bytes.len());
                    block.update(&bytes[..take]);
                    *in_block += take;
                    bytes = &bytes[take..];
                    if *in_block == DROPBOX_BLOCK {
                        digests.extend_from_slice(&std::mem::take(block).finalize());
                        *in_block = 0;
                    }
                }
            }
        }
    }

    /// The checksum, as lowercase hex.
    pub fn finish(self) -> String {
        let bytes: Vec<u8> = match self {
            StreamHasher::Md5(h) => h.finalize().to_vec(),
            StreamHasher::Sha1(h) => h.finalize().to_vec(),
            StreamHasher::Sha256(h) => h.finalize().to_vec(),
            StreamHasher::QuickXor(h) => h.finish().to_vec(),
            StreamHasher::Dropbox { block, in_block, mut digests } => {
                if in_block > 0 {
                    digests.extend_from_slice(&block.finalize());
                }
                Sha256::digest(&digests).to_vec()
            }
        };
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Microsoft's QuickXorHash, as OneDrive reports it: 160 bits, each byte
/// XORed in 11 bits further along (wrapping around), then the length
/// XORed into the last 8 bytes. Ported from Microsoft's published C#.
#[derive(Debug, Clone, Default)]
pub struct QuickXor {
    /// 160 bits: two 64-bit cells and a last one of 32.
    data: [u64; 3],
    /// Where the next byte goes, in bits.
    shift: usize,
    length: u64,
}

const QUICK_XOR_WIDTH: usize = 160;
const QUICK_XOR_SHIFT: usize = 11;

impl QuickXor {
    pub fn update(&mut self, bytes: &[u8]) {
        let mut index = self.shift / 64;
        let mut offset = self.shift % 64;
        // Each of the first 160 bytes starts a stride through the rest:
        // bytes 160 apart land on the same bits.
        for i in 0..bytes.len().min(QUICK_XOR_WIDTH) {
            let last = index == self.data.len() - 1;
            let bits = if last { 32 } else { 64 };
            if offset <= bits - 8 {
                for &byte in bytes[i..].iter().step_by(QUICK_XOR_WIDTH) {
                    self.data[index] ^= u64::from(byte) << offset;
                }
            } else {
                // Across the end of a cell: the rest goes into the next one.
                let next = if last { 0 } else { index + 1 };
                let folded = bytes[i..].iter().step_by(QUICK_XOR_WIDTH).fold(0u8, |acc, &b| acc ^ b);
                self.data[index] ^= u64::from(folded) << offset;
                self.data[next] ^= u64::from(folded) >> (bits - offset);
            }
            offset += QUICK_XOR_SHIFT;
            while offset >= bits {
                index = if last { 0 } else { index + 1 };
                offset -= bits;
            }
        }
        self.shift = (self.shift + QUICK_XOR_SHIFT * (bytes.len() % QUICK_XOR_WIDTH)) % QUICK_XOR_WIDTH;
        self.length += bytes.len() as u64;
    }

    pub fn finish(&self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[..8].copy_from_slice(&self.data[0].to_le_bytes());
        out[8..16].copy_from_slice(&self.data[1].to_le_bytes());
        out[16..].copy_from_slice(&self.data[2].to_le_bytes()[..4]);
        for (i, byte) in self.length.to_le_bytes().iter().enumerate() {
            out[12 + i] ^= byte;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quick_xor_hash() {
        use base64::Engine;
        let b64 = |parts: &[&[u8]]| {
            let mut hash = QuickXor::default();
            for part in parts {
                hash.update(part);
            }
            base64::engine::general_purpose::STANDARD.encode(hash.finish())
        };
        assert_eq!(b64(&[]), "AAAAAAAAAAAAAAAAAAAAAAAAAAA=");
        // One byte: it, and the length in the last 8 bytes.
        assert_eq!(b64(&[b"a"]), "YQAAAAAAAAAAAAAAAQAAAAAAAAA=");
        // The same however the bytes arrive.
        let data: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 256) as u8).collect();
        let whole = b64(&[&data]);
        assert_eq!(b64(&[&data[..1], &data[1..161], &data[161..5000], &data[5000..]]), whole);
        let pieces: Vec<&[u8]> = data.chunks(97).collect();
        assert_eq!(b64(&pieces), whole);
        assert_ne!(b64(&[&data[..data.len() - 1]]), whole);
    }

    #[test]
    fn streamed_checksums() {
        let hash = |kind: Kind, parts: &[&[u8]]| {
            let mut hasher = StreamHasher::new(kind);
            for part in parts {
                hasher.update(part);
            }
            hasher.finish()
        };
        assert_eq!(hash(Kind::Md5, &[b"ab", b"c"]), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hash(Kind::Sha1, &[b"abc"]), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hash(Kind::Sha256, &[b"a", b"bc"]),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Dropbox: SHA-256 over the blocks' SHA-256s, wherever the reads split.
        let data = vec![7u8; DROPBOX_BLOCK + 10];
        let expected = {
            let mut digests = Sha256::digest(&data[..DROPBOX_BLOCK]).to_vec();
            digests.extend_from_slice(&Sha256::digest(&data[DROPBOX_BLOCK..]));
            Sha256::digest(&digests).iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        assert_eq!(hash(Kind::Dropbox, &[&data]), expected);
        assert_eq!(
            hash(Kind::Dropbox, &[&data[..3], &data[3..DROPBOX_BLOCK + 1], &data[DROPBOX_BLOCK + 1..]]),
            expected
        );
        // An empty file: the hash of no blocks.
        assert_eq!(hash(Kind::Dropbox, &[]), hash(Kind::Sha256, &[]));
    }

    use super::*;

    #[test]
    fn writes_reads_and_verifies_checksum_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("photos/2026")).unwrap();
        std::fs::write(root.join("a.txt"), "abc").unwrap();
        std::fs::write(root.join("photos/2026/b.jpg"), "bee").unwrap();
        std::fs::write(root.join("photos/c d.txt"), "sea").unwrap();
        let fs = crate::vfs::local();
        let files = files_below(&fs, root, &[root.join("a.txt"), root.join("photos")]);
        let names: Vec<&str> = files.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(names, ["a.txt", "photos/2026/b.jpg", "photos/c d.txt"]);
        let lines: Vec<(String, String)> = files
            .iter()
            .map(|(path, relative)| (checksum(&fs, path, Algorithm::Sha256).unwrap(), relative.clone()))
            .collect();
        let text = checksum_file_text(&lines);
        assert!(text.starts_with("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  a.txt\n"));
        assert_eq!(parse_checksum_file(&text), lines);
        assert_eq!(verify(&fs, root, &lines).matching.len(), 3);
        // A changed file and a missing one.
        std::fs::write(root.join("photos/2026/b.jpg"), "changed").unwrap();
        std::fs::remove_file(root.join("photos/c d.txt")).unwrap();
        let result = verify(&fs, root, &lines);
        assert_eq!(result.matching, ["a.txt"]);
        assert_eq!(result.different, ["photos/2026/b.jpg"]);
        assert_eq!(result.missing.len(), 1);
        // Other tools' formats.
        let bsd = "MD5 (a.txt) = 900150983cd24fb0d6963f7d28e17f72\n";
        assert_eq!(parse_checksum_file(bsd), [("900150983cd24fb0d6963f7d28e17f72".to_string(), "a.txt".to_string())]);
        let binary = "a9993e364706816aba3e25717850c26c9cd0d89d *a.txt\r\n# comment\n";
        assert_eq!(parse_checksum_file(binary)[0].1, "a.txt");
        assert_eq!(Algorithm::for_hash("abc"), None);
        assert_eq!(Algorithm::for_hash(&"a".repeat(64)), Some(Algorithm::Sha256));
        assert!(is_checksum_file("release.SHA256") && is_checksum_file("SHA256SUMS") && !is_checksum_file("a.txt"));
    }

    #[test]
    fn known_values() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("abc.txt");
        std::fs::write(&file, "abc").unwrap();
        let fs = crate::vfs::local();
        assert_eq!(
            checksum(&fs, &file, Algorithm::Sha256).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(checksum(&fs, &file, Algorithm::Md5).unwrap(), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(checksum(&fs, &file, Algorithm::Sha1).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert!(checksum(&fs, &file, Algorithm::Sha384).unwrap().starts_with("cb00753f45a35e8b"));
        assert!(checksum(&fs, &file, Algorithm::Sha512).unwrap().starts_with("ddaf35a193617aba"));
        // Larger than one chunk.
        let big = dir.path().join("big.bin");
        std::fs::write(&big, vec![7u8; 600_000]).unwrap();
        let expected: String = Sha256::digest(vec![7u8; 600_000]).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(checksum(&fs, &big, Algorithm::Sha256).unwrap(), expected);
        assert!(checksum(&fs, &dir.path().join("missing"), Algorithm::Md5).is_err());
    }
}
