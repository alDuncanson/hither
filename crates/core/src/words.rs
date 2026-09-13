//! Words instead of bytes, for things people say aloud or type by hand.
//!
//! The list is the 2048-word BIP-39 English list (MIT licensed, from the
//! Bitcoin BIPs repository): short, unambiguous, sorted, no word is a prefix
//! of another. Each word carries eleven bits. Two users:
//!
//! - spoken share codes, four words (44 bits);
//! - identity export, 24 words (32 key bytes plus one checksum byte).

use std::sync::LazyLock;

use anyhow::{Result, bail, ensure};

static LIST: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    include_str!("words/english.txt")
        .lines()
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .collect()
});

/// Bits per word: 2048 = 2^11.
const BITS: usize = 11;

pub fn word(index: u16) -> &'static str {
    LIST[index as usize]
}

/// Position of `word` in the list, case-insensitive. `None` if unknown.
pub fn index_of(word: &str) -> Option<u16> {
    let w = word.trim().to_ascii_lowercase();
    LIST.binary_search(&w.as_str()).ok().map(|i| i as u16)
}

/// Split free text into candidate words: spaces, dashes and commas all count
/// as separators.
pub fn split(text: &str) -> Vec<&str> {
    text.split(|c: char| c.is_whitespace() || c == '-' || c == ',')
        .filter(|w| !w.is_empty())
        .collect()
}

/// Encode bytes as words, eleven bits at a time, big-endian, zero-padded at
/// the end. `bytes.len() * 8` should be a multiple of eleven for a clean
/// round trip; callers pad with a checksum byte to arrange that.
pub fn encode(bytes: &[u8]) -> Vec<&'static str> {
    let total_bits = bytes.len() * 8;
    let mut out = Vec::with_capacity(total_bits.div_ceil(BITS));
    let mut acc: u32 = 0;
    let mut acc_bits = 0;
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        acc_bits += 8;
        while acc_bits >= BITS {
            acc_bits -= BITS;
            out.push(word(((acc >> acc_bits) & 0x7FF) as u16));
        }
    }
    if acc_bits > 0 {
        out.push(word(((acc << (BITS - acc_bits)) & 0x7FF) as u16));
    }
    out
}

/// Decode words back into exactly `byte_len` bytes.
pub fn decode(words: &[&str], byte_len: usize) -> Result<Vec<u8>> {
    ensure!(
        words.len() == (byte_len * 8).div_ceil(BITS),
        "expected {} words, got {}",
        (byte_len * 8).div_ceil(BITS),
        words.len()
    );
    let mut out = Vec::with_capacity(byte_len);
    let mut acc: u32 = 0;
    let mut acc_bits = 0;
    for w in words {
        let Some(i) = index_of(w) else {
            bail!("{w:?} is not a word in the list");
        };
        acc = (acc << BITS) | i as u32;
        acc_bits += BITS;
        while acc_bits >= 8 && out.len() < byte_len {
            acc_bits -= 8;
            out.push(((acc >> acc_bits) & 0xFF) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_well_formed() {
        assert_eq!(LIST.len(), 2048);
        assert!(LIST.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
        assert_eq!(index_of("Abandon"), Some(0));
        assert_eq!(index_of("zoo"), Some(2047));
        assert_eq!(index_of("nope-not-a-word"), None);
    }

    #[test]
    fn bytes_round_trip_through_words() {
        let bytes: Vec<u8> = (0..33).map(|i| (i * 37 + 11) as u8).collect(); // 264 bits = 24 words
        let words = encode(&bytes);
        assert_eq!(words.len(), 24);
        assert_eq!(decode(&words, 33).unwrap(), bytes);
        assert!(decode(&words[..23], 33).is_err());
    }

    #[test]
    fn split_accepts_spaces_dashes_and_commas() {
        assert_eq!(
            split("able-cactus river,moth"),
            vec!["able", "cactus", "river", "moth"]
        );
    }
}
