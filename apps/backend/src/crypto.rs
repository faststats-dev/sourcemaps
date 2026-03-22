use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use rand::Rng;

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Wire format (matching Node.js `createCipheriv("aes-256-gcm")`):
///   iv (12 bytes) || authTag (16 bytes) || ciphertext
///
/// The `aes-gcm` crate uses a different internal layout (tag appended to
/// ciphertext), so we convert on the boundary.
#[derive(Clone)]
pub struct Crypto {
    cipher: Aes256Gcm,
}

impl Crypto {
    pub fn new(hex_key: &str) -> Result<Self, CryptoError> {
        let key_bytes = hex::decode(hex_key).map_err(|_| CryptoError::InvalidKey)?;
        if key_bytes.len() != 32 {
            return Err(CryptoError::InvalidKey);
        }
        let cipher = Aes256Gcm::new_from_slice(&key_bytes).map_err(|_| CryptoError::InvalidKey)?;
        Ok(Self { cipher })
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // aes-gcm returns ciphertext || tag
        let ct_with_tag = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(CryptoError::Aes)?;

        // Re-arrange to Node.js format: iv || tag || ciphertext
        let (ct, tag) = ct_with_tag.split_at(ct_with_tag.len() - TAG_LEN);

        let mut out = Vec::with_capacity(NONCE_LEN + TAG_LEN + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(tag);
        out.extend_from_slice(ct);
        Ok(out)
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if data.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptoError::TooShort);
        }

        // Wire format: iv (12) || tag (16) || ciphertext
        let nonce_bytes = &data[..NONCE_LEN];
        let tag = &data[NONCE_LEN..NONCE_LEN + TAG_LEN];
        let ct = &data[NONCE_LEN + TAG_LEN..];

        // aes-gcm expects ciphertext || tag
        let mut payload = Vec::with_capacity(ct.len() + TAG_LEN);
        payload.extend_from_slice(ct);
        payload.extend_from_slice(tag);

        let nonce = Nonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, payload.as_slice())
            .map_err(CryptoError::Aes)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid encryption key (must be 64 hex chars / 32 bytes)")]
    InvalidKey,
    #[error("ciphertext too short")]
    TooShort,
    #[error("AES-GCM error: {0}")]
    Aes(aes_gcm::Error),
}
