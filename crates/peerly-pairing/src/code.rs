//! Human codes: `<nameplate>-<word>-<word>`, e.g. `42-river-candle`.
//! The nameplate routes on the rendezvous server; the whole string is the SPAKE2 password.
//! Words are lowercase ASCII so they are easy to type on any keyboard layout.
//!
//! OWNER: pairing agent.

use std::sync::OnceLock;

/// Number of words in the list. A power of two, so random indices are unbiased.
pub const WORDLIST_LEN: usize = 1024;

/// The word list (1024 short, common, lowercase ASCII words). Part of the
/// protocol in practice: both sides must accept the same words.
pub fn wordlist() -> &'static [&'static str] {
    static WORDS: OnceLock<Vec<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| include_str!("words.txt").split_ascii_whitespace().collect())
}

fn is_word(w: &str) -> bool {
    wordlist().binary_search(&w).is_ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub nameplate: u32,
    pub words: [String; 2],
}

impl Code {
    /// Picks two random words with the OS CSPRNG.
    pub fn generate(nameplate: u32) -> Self {
        let words = wordlist();
        let mut buf = [0u8; 4];
        getrandom::fill(&mut buf).expect("OS random number generator unavailable");
        let pick = |b: [u8; 2]| words[usize::from(u16::from_le_bytes(b)) % words.len()].to_string();
        Code { nameplate, words: [pick([buf[0], buf[1]]), pick([buf[2], buf[3]])] }
    }

    /// Lenient parse: trims, lowercases, accepts spaces/underscores instead of
    /// dashes and repeated separators. Words must be from [`wordlist`].
    pub fn parse(input: &str) -> Option<Self> {
        let s = input.trim().to_ascii_lowercase();
        let parts: Vec<&str> = s
            .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .collect();
        let [nameplate, w1, w2] = parts.as_slice() else { return None };
        if nameplate.len() > 9 || !nameplate.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let nameplate = nameplate.parse().ok()?;
        if !is_word(w1) || !is_word(w2) {
            return None;
        }
        Some(Code { nameplate, words: [w1.to_string(), w2.to_string()] })
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}-{}", self.nameplate, self.words[0], self.words[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordlist_is_sane() {
        let w = wordlist();
        assert_eq!(w.len(), WORDLIST_LEN);
        assert!(w.windows(2).all(|p| p[0] < p[1]), "sorted and unique");
        assert!(w.iter().all(|x| (3..=7).contains(&x.len()) && x.bytes().all(|b| b.is_ascii_lowercase())));
    }

    #[test]
    fn generate_roundtrips() {
        for n in [0, 7, 42, 123_456] {
            let c = Code::generate(n);
            assert_eq!(c.nameplate, n);
            assert_eq!(Code::parse(&c.to_string()), Some(c));
        }
    }

    #[test]
    fn generate_is_random() {
        let a: Vec<_> = (0..8).map(|_| Code::generate(1).to_string()).collect();
        assert!(a.iter().any(|c| c != &a[0]));
    }

    #[test]
    fn parse_is_lenient() {
        let want = Code { nameplate: 42, words: ["river".into(), "candle".into()] };
        for s in ["42-river-candle", "  42-River-CANDLE \n", "42 river candle", "42--river - candle", "42_river_candle"] {
            assert_eq!(Code::parse(s).as_ref(), Some(&want), "{s:?}");
        }
        assert_eq!(want.to_string(), "42-river-candle");
    }

    #[test]
    fn parse_rejects_garbage() {
        for s in ["", "42", "42-river", "42-river-candle-x", "x-river-candle","+4-river-candle",
            "42-river-notaword", "99999999999-river-candle", "42-rivér-candle"] {
            assert_eq!(Code::parse(s), None, "{s:?}");
        }
    }
}
