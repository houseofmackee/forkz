//! Checksums of files on any file system, read in chunks (large files and
//! server files work without loading them whole).

use std::io::{self, Read};
use std::path::Path;

use md5::Md5;
use sha2::{Digest, Sha256};

use crate::vfs::FsRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    Sha256,
    Md5,
}

impl Algorithm {
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "SHA-256",
            Algorithm::Md5 => "MD5",
        }
    }
}

/// The checksum of a file, as lowercase hex.
pub fn checksum(fs: &FsRef, path: &Path, algorithm: Algorithm) -> io::Result<String> {
    let mut reader = fs.open_read(path)?;
    match algorithm {
        Algorithm::Sha256 => digest::<Sha256>(&mut reader),
        Algorithm::Md5 => digest::<Md5>(&mut reader),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Larger than one chunk.
        let big = dir.path().join("big.bin");
        std::fs::write(&big, vec![7u8; 600_000]).unwrap();
        let expected: String = Sha256::digest(vec![7u8; 600_000]).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(checksum(&fs, &big, Algorithm::Sha256).unwrap(), expected);
        assert!(checksum(&fs, &dir.path().join("missing"), Algorithm::Md5).is_err());
    }
}
