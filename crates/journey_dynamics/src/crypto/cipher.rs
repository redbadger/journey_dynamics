//! AES-256-GCM field encryption and AES-256-KWP key wrapping for GDPR crypto-shredding.
//!
//! # Design
//!
//! Each PII field is encrypted with a per-entity Data Encryption Key (DEK) using
//! AES-256-GCM.  The DEK itself is stored wrapped (encrypted) by a Key Encryption Key
//! (KEK) using AES-256-KWP (RFC 5649).  Deleting the DEK from the key store constitutes
//! "crypto-shredding": the PII becomes irrecoverable without touching individual events.

use aes::Aes256;
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, AeadCore, OsRng, Payload},
};
use aes_kw::Kek;
use uuid::Uuid;
use zeroize::Zeroizing;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// A 256-bit AES key used to encrypt PII fields in events.
pub struct KeyMaterial {
    pub key_id: Uuid,
    pub key: Zeroizing<Vec<u8>>, // always 32 bytes
}

/// The output of an encryption operation.
pub struct EncryptedPayload {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>, // 12 bytes (96-bit AES-GCM nonce)
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("Decryption failed: authentication tag mismatch or corrupt data")]
    DecryptionFailed,
    #[error("Key unwrap failed: the wrapped key is corrupt or the KEK is wrong")]
    KeyUnwrapFailed,
    #[error("Invalid KEK length: expected 32 bytes, got {0}")]
    InvalidKekLength(usize),
}

// ─────────────────────────────────────────────────────────────────────────────
// PiiCipher
// ─────────────────────────────────────────────────────────────────────────────

/// Handles AES-256-GCM field encryption/decryption and AES-256-KWP key wrapping.
pub struct PiiCipher {
    kek: Zeroizing<Vec<u8>>, // 32-byte Key Encryption Key
}

impl PiiCipher {
    /// Construct from raw KEK bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::InvalidKekLength`] if `kek` is not exactly 32 bytes.
    pub fn new(kek: Vec<u8>) -> Result<Self, CryptoError> {
        if kek.len() != 32 {
            return Err(CryptoError::InvalidKekLength(kek.len()));
        }
        Ok(Self {
            kek: Zeroizing::new(kek),
        })
    }

    /// Generate a fresh random 256-bit DEK (Data Encryption Key) using the OS CSPRNG.
    #[must_use]
    pub fn generate_dek() -> KeyMaterial {
        let key = Aes256Gcm::generate_key(OsRng);
        KeyMaterial {
            key_id: Uuid::new_v4(),
            key: Zeroizing::new(key.to_vec()),
        }
    }

    /// Encrypt `plaintext` with `dek` using AES-256-GCM.
    ///
    /// `aad` is authenticated additional data; callers should include the aggregate ID and
    /// event sequence number so that ciphertext cannot be transplanted between events.
    /// A fresh random 96-bit nonce is generated on every call.
    ///
    /// # Panics
    ///
    /// Panics if `dek.key` is not exactly 32 bytes — this is an invariant of [`KeyMaterial`]
    /// and is always satisfied when keys are produced by [`PiiCipher::generate_dek`] or
    /// [`PiiCipher::unwrap_dek`].
    pub fn encrypt(&self, dek: &KeyMaterial, plaintext: &[u8], aad: &[u8]) -> EncryptedPayload {
        let cipher = Aes256Gcm::new_from_slice(&dek.key)
            .expect("DEK is always 32 bytes — invariant of KeyMaterial");
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .expect("AES-256-GCM encryption cannot fail given a valid key and nonce");
        EncryptedPayload {
            ciphertext,
            nonce: nonce.to_vec(),
        }
    }

    /// Decrypt `encrypted` with `dek` using AES-256-GCM.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::DecryptionFailed`] if the authentication tag is invalid or the
    /// supplied `aad` does not match the `aad` that was used during encryption.
    ///
    /// # Panics
    ///
    /// Panics if `dek.key` is not exactly 32 bytes — see [`PiiCipher::encrypt`] for details.
    pub fn decrypt(
        &self,
        dek: &KeyMaterial,
        encrypted: &EncryptedPayload,
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        let cipher = Aes256Gcm::new_from_slice(&dek.key)
            .expect("DEK is always 32 bytes — invariant of KeyMaterial");
        let nonce = Nonce::from_slice(&encrypted.nonce);
        cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &encrypted.ciphertext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::DecryptionFailed)
    }

    /// Wrap (encrypt) a DEK with the KEK using AES-256-KWP (RFC 5649).
    ///
    /// Returns the wrapped key bytes (40 bytes: 32-byte key + 8-byte integrity block).
    ///
    /// # Panics
    ///
    /// Cannot panic because the KEK length is validated in [`PiiCipher::new`].
    #[must_use]
    pub fn wrap_dek(&self, dek: &KeyMaterial) -> Vec<u8> {
        let kek = Kek::<Aes256>::try_from(self.kek.as_slice())
            .expect("KEK is always 32 bytes — validated in PiiCipher::new");
        kek.wrap_with_padding_vec(&dek.key)
            .expect("AES-KWP wrapping cannot fail with a valid key and well-formed data")
    }

    /// Unwrap (decrypt) a previously-wrapped DEK.
    ///
    /// `key_id` is the UUID to assign to the returned [`KeyMaterial`]; it should match the
    /// `key_id` that was recorded alongside the wrapped bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::KeyUnwrapFailed`] if the wrapped data is corrupt or the KEK
    /// does not match the one that was used to wrap the key.
    ///
    /// # Panics
    ///
    /// Cannot panic because the KEK length is validated in [`PiiCipher::new`].
    pub fn unwrap_dek(&self, key_id: Uuid, wrapped_key: &[u8]) -> Result<KeyMaterial, CryptoError> {
        let kek = Kek::<Aes256>::try_from(self.kek.as_slice())
            .expect("KEK is always 32 bytes — validated in PiiCipher::new");
        let key_bytes = kek
            .unwrap_with_padding_vec(wrapped_key)
            .map_err(|_| CryptoError::KeyUnwrapFailed)?;
        Ok(KeyMaterial {
            key_id,
            key: Zeroizing::new(key_bytes),
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A reusable cipher whose KEK is 0x42-filled — distinct from the all-zero DEK
    /// used in some other tests so the two never accidentally coincide.
    fn test_cipher() -> PiiCipher {
        PiiCipher::new(vec![0x42u8; 32]).expect("0x42-filled 32-byte KEK must be valid")
    }

    // ── PiiCipher::new ────────────────────────────────────────────────────────

    #[test]
    fn test_new_accepts_32_byte_kek() {
        let result = PiiCipher::new(vec![0u8; 32]);
        assert!(result.is_ok(), "expected Ok for a 32-byte KEK");
    }

    #[test]
    fn test_new_rejects_short_kek() {
        let result = PiiCipher::new(vec![0u8; 31]);
        assert!(
            matches!(result, Err(CryptoError::InvalidKekLength(31))),
            "expected Err(InvalidKekLength(31)) for a 31-byte KEK"
        );
    }

    #[test]
    fn test_new_rejects_long_kek() {
        let result = PiiCipher::new(vec![0u8; 33]);
        assert!(
            matches!(result, Err(CryptoError::InvalidKekLength(33))),
            "expected Err(InvalidKekLength(33)) for a 33-byte KEK"
        );
    }

    // ── PiiCipher::generate_dek ───────────────────────────────────────────────

    #[test]
    fn test_generate_dek_is_32_bytes() {
        let dek = PiiCipher::generate_dek();
        assert_eq!(dek.key.len(), 32, "DEK key must be exactly 32 bytes");
    }

    #[test]
    fn test_generate_dek_is_unique() {
        let dek1 = PiiCipher::generate_dek();
        let dek2 = PiiCipher::generate_dek();
        assert_ne!(
            *dek1.key, *dek2.key,
            "consecutive generate_dek() calls must produce different key bytes"
        );
    }

    #[test]
    fn test_generate_dek_has_unique_key_id() {
        let dek1 = PiiCipher::generate_dek();
        let dek2 = PiiCipher::generate_dek();
        assert_ne!(
            dek1.key_id, dek2.key_id,
            "consecutive generate_dek() calls must produce different key_ids"
        );
    }

    // ── Encrypt / Decrypt ─────────────────────────────────────────────────────

    #[test]
    fn test_encrypt_decrypt_round_trip() {
        let cipher = test_cipher();
        let dek = PiiCipher::generate_dek();
        let plaintext = b"sensitive PII: Alice Smith, DOB 1990-01-01";
        let aad = b"agg-123:1";

        let encrypted = cipher.encrypt(&dek, plaintext, aad);
        let decrypted = cipher
            .decrypt(&dek, &encrypted, aad)
            .expect("decryption with the correct DEK and AAD must succeed");

        assert_eq!(
            decrypted, plaintext,
            "decrypted output must exactly match the original plaintext"
        );
    }

    #[test]
    fn test_encrypt_produces_unique_ciphertext() {
        let cipher = test_cipher();
        let dek = PiiCipher::generate_dek();
        let plaintext = b"same plaintext every time";
        let aad = b"agg-123:1";

        let enc1 = cipher.encrypt(&dek, plaintext, aad);
        let enc2 = cipher.encrypt(&dek, plaintext, aad);

        // A fresh random nonce is generated per call, so both the nonce and the
        // resulting ciphertext must differ even for identical inputs.
        assert_ne!(
            enc1.nonce, enc2.nonce,
            "each encryption must use a unique nonce"
        );
        assert_ne!(
            enc1.ciphertext, enc2.ciphertext,
            "unique nonces must produce unique ciphertexts"
        );
    }

    #[test]
    fn test_decrypt_fails_with_wrong_dek() {
        let cipher = test_cipher();
        let dek1 = PiiCipher::generate_dek();
        let dek2 = PiiCipher::generate_dek(); // deliberately different
        let plaintext = b"sensitive PII data";
        let aad = b"agg-123:1";

        let encrypted = cipher.encrypt(&dek1, plaintext, aad);
        let result = cipher.decrypt(&dek2, &encrypted, aad);

        assert!(
            matches!(result, Err(CryptoError::DecryptionFailed)),
            "decryption with the wrong DEK must return DecryptionFailed"
        );
    }

    #[test]
    fn test_decrypt_fails_with_wrong_aad() {
        let cipher = test_cipher();
        let dek = PiiCipher::generate_dek();
        let plaintext = b"sensitive PII data";
        let aad1 = b"agg-123:1";
        let aad2 = b"agg-999:2"; // deliberately different

        let encrypted = cipher.encrypt(&dek, plaintext, aad1);
        let result = cipher.decrypt(&dek, &encrypted, aad2);

        assert!(
            matches!(result, Err(CryptoError::DecryptionFailed)),
            "decryption with mismatched AAD must return DecryptionFailed"
        );
    }

    #[test]
    fn test_decrypt_fails_with_tampered_ciphertext() {
        let cipher = test_cipher();
        let dek = PiiCipher::generate_dek();
        let plaintext = b"sensitive PII data";
        let aad = b"agg-123:1";

        let mut encrypted = cipher.encrypt(&dek, plaintext, aad);
        // Flip the first byte to simulate a single-bit storage corruption or active attack.
        encrypted.ciphertext[0] ^= 0xFF;

        let result = cipher.decrypt(&dek, &encrypted, aad);

        assert!(
            matches!(result, Err(CryptoError::DecryptionFailed)),
            "decryption of a tampered ciphertext must return DecryptionFailed"
        );
    }

    // ── Wrap / Unwrap ─────────────────────────────────────────────────────────

    #[test]
    fn test_wrap_unwrap_dek_round_trip() {
        let cipher = test_cipher();
        let dek = PiiCipher::generate_dek();
        let original_key_id = dek.key_id;
        // Capture raw bytes before lending dek to wrap_dek (KeyMaterial is not Clone).
        let original_key_bytes: Vec<u8> = dek.key.to_vec();

        let wrapped = cipher.wrap_dek(&dek);
        let unwrapped = cipher
            .unwrap_dek(original_key_id, &wrapped)
            .expect("unwrap_dek with the same KEK must succeed");

        assert_eq!(
            unwrapped.key_id, original_key_id,
            "the key_id supplied to unwrap_dek must be returned verbatim"
        );
        assert_eq!(
            *unwrapped.key, original_key_bytes,
            "unwrapped key bytes must exactly match the original DEK bytes"
        );
    }

    #[test]
    fn test_unwrap_dek_fails_with_wrong_kek() {
        let cipher1 = PiiCipher::new(vec![0x42u8; 32]).expect("cipher1 KEK valid");
        let cipher2 = PiiCipher::new(vec![0xDEu8; 32]).expect("cipher2 KEK valid");

        let dek = PiiCipher::generate_dek();
        let key_id = dek.key_id;
        let wrapped = cipher1.wrap_dek(&dek);

        let result = cipher2.unwrap_dek(key_id, &wrapped);

        assert!(
            matches!(result, Err(CryptoError::KeyUnwrapFailed)),
            "unwrap_dek with the wrong KEK must return KeyUnwrapFailed"
        );
    }

    // ── AAD convention ────────────────────────────────────────────────────────

    #[test]
    fn test_aad_includes_aggregate_and_sequence() {
        // Documents the AAD format used throughout the system:
        //   "<aggregate_id>:<sequence_number>"  (UTF-8 encoded)
        //
        // Both the writer (encrypt) and the reader (decrypt) must derive the AAD
        // identically.  Binding the ciphertext to the aggregate ID and sequence number
        // prevents an adversary from copying a valid ciphertext into a different event.
        let cipher = test_cipher();
        let dek = PiiCipher::generate_dek();
        let plaintext = b"passenger name: Alice Smith";

        let aggregate_id = Uuid::new_v4();
        let sequence: u64 = 7;
        let aad = format!("{aggregate_id}:{sequence}").into_bytes();

        let encrypted = cipher.encrypt(&dek, plaintext, &aad);
        let decrypted = cipher
            .decrypt(&dek, &encrypted, &aad)
            .expect("round-trip with aggregate_id:sequence AAD must succeed");

        assert_eq!(
            decrypted, plaintext,
            "plaintext must survive a round-trip through the aggregate_id:sequence AAD convention"
        );
    }
}
