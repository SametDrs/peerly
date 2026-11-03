//! Sealing `PeerInfo` with keys derived from the SPAKE2 output.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce};
use hkdf::Hkdf;
use peerly_proto::pairing::{PairingMsg, PeerInfo};
use sha2::Sha256;

use crate::PairingError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Sender,
    Receiver,
}

impl Role {
    fn info(self) -> &'static [u8] {
        match self {
            Role::Sender => b"peerly v1 peerinfo sender",
            Role::Receiver => b"peerly v1 peerinfo receiver",
        }
    }

    fn other(self) -> Role {
        match self {
            Role::Sender => Role::Receiver,
            Role::Receiver => Role::Sender,
        }
    }
}

/// One key per direction, so a reflected message never decrypts.
pub(crate) struct Keys {
    seal: ChaCha20Poly1305,
    open: ChaCha20Poly1305,
}

fn derive(hk: &Hkdf<Sha256>, role: Role) -> ChaCha20Poly1305 {
    let mut k = [0u8; 32];
    hk.expand(role.info(), &mut k)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    ChaCha20Poly1305::new_from_slice(&k).expect("32-byte key")
}

impl Keys {
    pub(crate) fn derive(shared: &[u8], me: Role) -> Self {
        let hk = Hkdf::<Sha256>::new(None, shared);
        Keys {
            seal: derive(&hk, me),
            open: derive(&hk, me.other()),
        }
    }

    pub(crate) fn seal(&self, info: &PeerInfo) -> Result<PairingMsg, PairingError> {
        let plain =
            peerly_proto::encode(info).map_err(|e| PairingError::Protocol(e.to_string()))?;
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| PairingError::Protocol(format!("rng: {e}")))?;
        let ciphertext = self
            .seal
            .encrypt(&Nonce::from(nonce), plain.as_slice())
            .map_err(|_| PairingError::Protocol("encryption failed".into()))?;
        Ok(PairingMsg::Sealed { nonce, ciphertext })
    }

    /// AEAD failure means the peer derived a different key, i.e. a wrong code.
    pub(crate) fn open(
        &self,
        nonce: [u8; 12],
        ciphertext: &[u8],
    ) -> Result<PeerInfo, PairingError> {
        let plain = self
            .open
            .decrypt(&Nonce::from(nonce), ciphertext)
            .map_err(|_| PairingError::WrongCode)?;
        peerly_proto::decode(&plain)
            .map_err(|e| PairingError::Protocol(format!("bad peer info: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip_and_direction() {
        let info = PeerInfo {
            cert_fingerprint: [7; 32],
            candidates: vec!["1.2.3.4:5".parse().unwrap()],
        };
        let s = Keys::derive(b"shared", Role::Sender);
        let r = Keys::derive(b"shared", Role::Receiver);
        let PairingMsg::Sealed { nonce, ciphertext } = s.seal(&info).unwrap() else {
            panic!()
        };
        assert_eq!(r.open(nonce, &ciphertext).unwrap(), info);
        // Reflection back to the sender must fail.
        assert!(matches!(
            s.open(nonce, &ciphertext),
            Err(PairingError::WrongCode)
        ));
        // A different shared key must fail.
        let wrong = Keys::derive(b"other", Role::Receiver);
        assert!(matches!(
            wrong.open(nonce, &ciphertext),
            Err(PairingError::WrongCode)
        ));
    }
}
