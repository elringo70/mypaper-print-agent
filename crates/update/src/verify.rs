use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("sha256 mismatch: got {got}, want {want}")]
    Sha256 { got: String, want: String },
    #[error("bad public key: {0}")]
    PublicKey(String),
    #[error("bad signature: {0}")]
    Signature(String),
    #[error("signature invalid: {0}")]
    Invalid(String),
}

/// Verify installer bytes: sha256 first (cheap integrity), then the minisign
/// signature (authenticity). The public key is baked in at build time from
/// `MPA_UPDATE_PUBKEY` so a compromised channel cannot swap it.
pub fn verify_asset(bytes: &[u8], sha256: &str, signature: &str) -> Result<(), VerifyError> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let got = hex_encode(hasher.finalize().as_slice());
    if !constant_eq(&got, sha256) {
        return Err(VerifyError::Sha256 {
            got,
            want: sha256.into(),
        });
    }
    let key_b64 = option_env!("MPA_UPDATE_PUBKEY").unwrap_or(BUILTIN_PUBKEY);
    let public_key =
        PublicKey::from_base64(key_b64).map_err(|e| VerifyError::PublicKey(e.to_string()))?;
    let sig = Signature::decode(signature).map_err(|e| VerifyError::Signature(e.to_string()))?;
    public_key
        .verify(bytes, &sig, false)
        .map_err(|e| VerifyError::Invalid(e.to_string()))
}

/// Comparison that does not short-circuit on the first differing byte.
fn constant_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Release public key (`packaging/minisign.pub`). Overridable at build time
/// with `MPA_UPDATE_PUBKEY=... cargo build` for key rotation without edits.
const BUILTIN_PUBKEY: &str = "RWRvNgSnEs4jWtCrr0x7R4vdmnXOxqZJLSpA9oGRMdnuJnm/E1ITu8a+";

/// Tiny hex encoder to avoid pulling a hex crate for two call sites.
fn hex_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(ALPHABET[(b >> 4) as usize] as char);
        out.push(ALPHABET[(b & 0xF) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Self-consistent vector signed with rsign (content "test"). Base64 lines
    // must stay whole; minisign rejects re-wrapped signatures.
    const PUBKEY: &str = "RWTVtYyyIqnmnrZevBDd3Ggl9gPSBKT+ozfLym3/ajMpBCJ/fuKYgBZY";
    const SIGNATURE: &str = "untrusted comment: signature from rsign secret key\nRUTVtYyyIqnmntAr9S+QYBIgfRaRZh2zP2fxCvcB/5Qw7tL6XLIt7I2V3FDdbEBv/J5WfSp/8UxPtUUXgrlg1yTyP7N1g8IztwQ=\ntrusted comment: timestamp:1792131207\tfile:content.bin\tprehashed\ngou8Q/fLdZRGChFoDYz21wlrTN8B39au0mNO8mDiNU4JJpIVZbuWNemTV8+AeLXLyGqRzdxDZPOPqqR+L7c2Dw==\n";

    fn sha256_of(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hex_encode(hasher.finalize().as_slice())
    }

    #[test]
    fn accepts_a_genuine_signature() {
        let content = b"test";
        // Verify the doc vector directly (builtin key is the project key).
        let public_key = PublicKey::from_base64(PUBKEY).unwrap();
        let signature = Signature::decode(SIGNATURE).unwrap();
        public_key.verify(&content[..], &signature, false).unwrap();
        // And through the asset gate with a matching digest.
        let digest = sha256_of(content);
        let with_key = override_verify(content, &digest, SIGNATURE, PUBKEY);
        assert!(with_key.is_ok());
    }

    #[test]
    fn rejects_tampered_bytes_and_wrong_digest() {
        let digest = sha256_of(b"test");
        assert!(matches!(
            override_verify(b"test!", &digest, SIGNATURE, PUBKEY),
            Err(VerifyError::Sha256 { .. })
        ));
        assert!(matches!(
            override_verify(b"test", "0", SIGNATURE, PUBKEY),
            Err(VerifyError::Sha256 { .. })
        ));
    }

    #[test]
    fn rejects_wrong_key() {
        let content = b"test";
        let digest = sha256_of(content);
        assert!(matches!(
            override_verify(content, &digest, SIGNATURE, BUILTIN_PUBKEY),
            Err(VerifyError::Invalid(_))
        ));
    }

    /// Same gate as `verify_asset` but with an explicit key (tests only).
    fn override_verify(
        bytes: &[u8],
        sha256: &str,
        signature: &str,
        key_b64: &str,
    ) -> Result<(), VerifyError> {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let got = hex_encode(hasher.finalize().as_slice());
        if !constant_eq(&got, sha256) {
            return Err(VerifyError::Sha256 {
                got,
                want: sha256.into(),
            });
        }
        let public_key =
            PublicKey::from_base64(key_b64).map_err(|e| VerifyError::PublicKey(e.to_string()))?;
        let sig =
            Signature::decode(signature).map_err(|e| VerifyError::Signature(e.to_string()))?;
        public_key
            .verify(bytes, &sig, false)
            .map_err(|e| VerifyError::Invalid(e.to_string()))
    }
}
