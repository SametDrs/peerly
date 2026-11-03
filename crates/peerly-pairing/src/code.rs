//! Human codes: `<nameplate>-<word>-<word>`, e.g. `42-river-candle`.
//! The nameplate routes on the rendezvous server; the whole string is the SPAKE2 password.
//! Words are lowercase ASCII so they are easy to type on any keyboard layout.
//!
//! OWNER: pairing agent.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub nameplate: u32,
    pub words: [String; 2],
}

impl Code {
    pub fn generate(nameplate: u32) -> Self {
        let _ = nameplate;
        todo!("pairing agent")
    }

    /// Lenient parse: trims, lowercases, accepts spaces instead of dashes.
    pub fn parse(input: &str) -> Option<Self> {
        let _ = input;
        todo!("pairing agent")
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}-{}", self.nameplate, self.words[0], self.words[1])
    }
}
