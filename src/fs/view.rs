//! The viewer's text and hex forms of a file's bytes.

/// Text from bytes: UTF-16 with a byte order mark, UTF-8, or Latin-1 (every
/// byte a character) for the rest. Returns the text and the encoding's name.
pub fn decode(bytes: &[u8]) -> (String, &'static str) {
    let utf16 = |rest: &[u8], le: bool| -> String {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| if le { u16::from_le_bytes(*pair) } else { u16::from_be_bytes(*pair) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => (utf16(rest, true), "UTF-16"),
        [0xFE, 0xFF, rest @ ..] => (utf16(rest, false), "UTF-16"),
        [0xEF, 0xBB, 0xBF, rest @ ..] => (String::from_utf8_lossy(rest).into_owned(), "UTF-8"),
        _ => match std::str::from_utf8(bytes) {
            Ok(text) => (text.to_string(), "UTF-8"),
            // A file cut off at the size limit may end mid-character.
            Err(e) if e.valid_up_to() + 4 > bytes.len() && e.error_len().is_none() => {
                (String::from_utf8_lossy(bytes).into_owned(), "UTF-8")
            }
            Err(_) => (bytes.iter().map(|b| char::from(*b)).collect(), "Latin-1"),
        },
    }
}

/// Whether bytes look like a binary file (a zero byte early on, without a
/// UTF-16 byte order mark): the viewer starts in hex then.
pub fn looks_binary(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return false;
    }
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// Bytes per hex row.
pub const ROW: usize = 16;

/// One hex row: the offset, the bytes in hex (in two groups of eight), and
/// them as characters (dots for the unprintable).
pub fn hex_row(bytes: &[u8], row: usize) -> (String, String, String) {
    let start = row * ROW;
    let chunk = bytes.get(start..(start + ROW).min(bytes.len())).unwrap_or(&[]);
    let mut hex = String::with_capacity(ROW * 3 + 1);
    for (i, b) in chunk.iter().enumerate() {
        if i == ROW / 2 {
            hex.push(' ');
        }
        hex.push_str(&format!("{b:02X} "));
    }
    let ascii = chunk.iter().map(|b| if (0x20..0x7F).contains(b) { char::from(*b) } else { '.' }).collect();
    (format!("{start:08X}"), hex.trim_end().to_string(), ascii)
}

/// How many hex rows bytes need.
pub fn hex_rows(len: usize) -> usize {
    len.div_ceil(ROW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_common_encodings() {
        assert_eq!(decode("h\u{e9}llo".as_bytes()), ("h\u{e9}llo".to_string(), "UTF-8"));
        assert_eq!(decode(&[0xEF, 0xBB, 0xBF, b'a']), ("a".to_string(), "UTF-8"));
        assert_eq!(decode(&[0xFF, 0xFE, b'h', 0, b'i', 0]), ("hi".to_string(), "UTF-16"));
        assert_eq!(decode(&[0xFE, 0xFF, 0, b'h']), ("h".to_string(), "UTF-16"));
        assert_eq!(decode(&[b'c', 0xE9, b'!']), ("c\u{e9}!".to_string(), "Latin-1"));
        // Cut off in the middle of a character (at the size limit).
        let mut cut = "ab\u{1F600}".as_bytes().to_vec();
        cut.pop();
        assert_eq!(decode(&cut).1, "UTF-8");
    }

    #[test]
    fn spots_binary_files() {
        assert!(looks_binary(b"\x7fELF\x02\x01\x01\x00\x00"));
        assert!(!looks_binary(b"plain text\n"));
        assert!(!looks_binary(&[0xFF, 0xFE, b'a', 0]));
    }

    #[test]
    fn hex_rows_have_offsets_bytes_and_characters() {
        let bytes: Vec<u8> = (0u8..20).chain(*b"AZ").collect();
        assert_eq!(hex_rows(bytes.len()), 2);
        assert_eq!(hex_rows(0), 0);
        let (offset, hex, ascii) = hex_row(&bytes, 0);
        assert_eq!(offset, "00000000");
        assert_eq!(hex, "00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F");
        assert_eq!(ascii, "................");
        let (offset, hex, ascii) = hex_row(&bytes, 1);
        assert_eq!((offset.as_str(), hex.as_str(), ascii.as_str()), ("00000010", "10 11 12 13 41 5A", "....AZ"));
        assert_eq!(hex_row(&bytes, 5), ("00000050".into(), String::new(), String::new()));
    }
}
