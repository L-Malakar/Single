//! End-to-end encryption for the P2P channel (the "bridge").
//!
//! After the plaintext HELLO / HELLO_ACK / ACK handshake — which now also
//! exchanges an ephemeral X25519 public key in the `e2ee` field — both sides
//! derive symmetric session keys and every following frame on the TCP stream
//! is sealed into an AES-256-GCM envelope:
//!
//! ```text
//!   line = {"type":"e","n":"<b64 12-byte nonce>","c":"<b64 ciphertext||tag>"}
//!          plaintext = the JSON serialization of the inner [`Wire`] frame
//! ```
//!
//! Key schedule (identical on both ends — no server, no CA, no trust anchor
//! needed):
//!
//! ```text
//!   shared = X25519(my_ephemeral_secret, their_ephemeral_public)
//!   salt   = utf8(initiator_nonce || responder_nonce)     # from the HELLOs
//!   k_i2r  = HKDF-SHA256(shared, salt, "P2PBRIDGE/E2EE/1/i2r")
//!   k_r2i  = HKDF-SHA256(shared, salt, "P2PBRIDGE/E2EE/1/r2i")
//! ```
//!
//! The initiator sends with `k_i2r` and receives with `k_r2i`; the responder
//! the other way round. Nonces are 96-bit and random per frame — never reused
//! with the same key. Peers that do not offer an `e2ee` key (older builds)
//! keep working over plaintext, and the dashboard shows that clearly.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// One side of an ephemeral X25519 keypair, created fresh for every
/// connection attempt and thrown away with it.
pub struct EphemeralKey {
    secret: StaticSecret,
}

impl EphemeralKey {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        // rand 0.9: fill the key with OS randomness.
        for b in bytes.iter_mut() {
            *b = rand::random::<u8>();
        }
        Self {
            secret: StaticSecret::from(bytes),
        }
    }

    /// Base64 public key to put into the `e2ee` field of HELLO / HELLO_ACK.
    pub fn public_b64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(PublicKey::from(&self.secret).as_bytes())
    }
}

/// The two derived session keys; one for sending, one for receiving.
#[derive(Clone)]
pub struct SessionKeys {
    pub send: [u8; 32],
    pub recv: [u8; 32],
}

/// Derive both direction keys from the ephemeral handshake.
///
/// `initiator_nonce` / `responder_nonce` are the handshake nonces from the
/// HELLO and HELLO_ACK frames (role-ordered, so both sides compute the same
/// salt regardless of who they are).
pub fn derive_keys(
    mine: &EphemeralKey,
    their_public_b64: &str,
    initiator_nonce: &str,
    responder_nonce: &str,
    i_am_initiator: bool,
) -> Result<SessionKeys, String> {
    let their = base64::engine::general_purpose::STANDARD
        .decode(their_public_b64.trim())
        .map_err(|e| format!("bad e2ee public key: {e}"))?;
    let their: [u8; 32] = their
        .try_into()
        .map_err(|_| "bad e2ee public key length".to_string())?;
    let shared = mine.secret.diffie_hellman(&PublicKey::from(their));

    let salt = format!("{initiator_nonce}{responder_nonce}").into_bytes();
    let k_i2r = expand(shared.as_bytes(), &salt, b"P2PBRIDGE/E2EE/1/i2r");
    let k_r2i = expand(shared.as_bytes(), &salt, b"P2PBRIDGE/E2EE/1/r2i");
    let (send, recv) = if i_am_initiator {
        (k_i2r, k_r2i)
    } else {
        (k_r2i, k_i2r)
    };
    Ok(SessionKeys { send, recv })
}

fn expand(ikm: &[u8; 32], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out).expect("32 bytes is a valid length");
    out
}

/// AES-256-GCM seal. Returns `(nonce_b64, ciphertext_b64)`.
pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> (String, String) {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    for b in nonce_bytes.iter_mut() {
        *b = rand::random::<u8>();
    }
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload { msg: plaintext, aad: b"p2p-bridge" },
        )
        .expect("aes-gcm encryption cannot fail with valid inputs");
    (
        base64::engine::general_purpose::STANDARD.encode(nonce_bytes),
        base64::engine::general_purpose::STANDARD.encode(ct),
    )
}

/// AES-256-GCM open. Fails (→ session killed) on any tampering.
pub fn open(key: &[u8; 32], nonce_b64: &str, ct_b64: &str) -> Result<Vec<u8>, String> {
    let nonce_bytes = base64::engine::general_purpose::STANDARD
        .decode(nonce_b64)
        .map_err(|e| format!("bad envelope nonce: {e}"))?;
    let ct = base64::engine::general_purpose::STANDARD
        .decode(ct_b64)
        .map_err(|e| format!("bad envelope payload: {e}"))?;
    if nonce_bytes.len() != 12 {
        return Err("bad envelope nonce length".into());
    }
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload { msg: &ct, aad: b"p2p-bridge" },
        )
        .map_err(|_| "decryption failed — tampered or corrupted frame".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_derive_the_same_direction_keys() {
        let a = EphemeralKey::generate();
        let b = EphemeralKey::generate();
        let ka = derive_keys(&a, &b.public_b64(), "aaa", "bbb", true).unwrap();
        let kb = derive_keys(&b, &a.public_b64(), "aaa", "bbb", false).unwrap();
        assert_eq!(ka.send, kb.recv);
        assert_eq!(ka.recv, kb.send);
    }

    #[test]
    fn seal_open_roundtrip_and_tamper_detection() {
        let a = EphemeralKey::generate();
        let b = EphemeralKey::generate();
        let ka = derive_keys(&a, &b.public_b64(), "n1", "n2", true).unwrap();
        let kb = derive_keys(&b, &a.public_b64(), "n1", "n2", false).unwrap();
        assert_eq!(ka.send, kb.recv); // same direction → same key

        // a seals with its send key; b opens with its recv key
        let (n, c) = seal(&ka.send, b"{\"type\":\"msg\",\"text\":\"hi\",\"ts\":1}");
        let opened = open(&kb.recv, &n, &c).unwrap();
        assert_eq!(opened, b"{\"type\":\"msg\",\"text\":\"hi\",\"ts\":1}");

        // flip one byte of the ciphertext → must fail
        let mut bad = base64::engine::general_purpose::STANDARD
            .decode(&c)
            .unwrap();
        bad[0] ^= 1;
        let bad_c = base64::engine::general_purpose::STANDARD.encode(bad);
        assert!(open(&kb.recv, &n, &bad_c).is_err());
        // wrong direction key → must fail
        assert!(open(&kb.send, &n, &c).is_err());
        assert!(open(&ka.recv, &n, &c).is_err());
    }

    #[test]
    fn nonce_is_never_reused() {
        let k = [7u8; 32];
        let (n1, _) = seal(&k, b"x");
        let (n2, _) = seal(&k, b"x");
        assert_ne!(n1, n2);
    }
}
