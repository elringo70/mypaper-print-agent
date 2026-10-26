//! Per-device request signing.
//!
//! A pairing key alone is a bearer secret: anyone who copies `agent.json` can
//! talk to MyPaper. Each device therefore keeps an Ed25519 keypair and signs
//! every request, so the server can reject a replay or a request made with a
//! copied key.
//!
//! ## Canonical form (must match the server byte for byte)
//!
//! ```text
//! METHOD \n path \n timestamp \n nonce \n sha256_hex(body)
//! ```
//!
//! * `METHOD` is uppercase (`GET`, `POST`).
//! * `path` is the request path **without** scheme or host, including the query
//!   string exactly as sent (`/api/agent/tiers?x=1`).
//! * `timestamp` is Unix seconds, decimal, no padding.
//! * `nonce` is 16 random bytes, lowercase hex (32 chars).
//! * `sha256_hex` is the lowercase hex digest of the body, or of the empty
//!   string when there is no body.
//!
//! The server rebuilds the same string from the headers and the raw body; any
//! divergence simply fails verification, which is the safe direction.

use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

/// How far the server's clock may differ from ours, in seconds.
pub const MAX_CLOCK_SKEW_SECS: i64 = 120;

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("device key is not valid: {0}")]
    Key(String),
}

/// A device identity: what signs requests and what the server stores.
#[derive(Clone)]
pub struct DeviceKey {
    signing: SigningKey,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret, even in a panic trace.
        f.debug_struct("DeviceKey")
            .field("public", &self.public_b64())
            .finish()
    }
}

impl DeviceKey {
    /// Fresh keypair from the OS entropy source.
    pub fn generate() -> Self {
        let mut rng = rand_core::OsRng;
        Self {
            signing: SigningKey::generate(&mut rng),
        }
    }

    /// Rebuild from the stored 32-byte secret.
    pub fn from_secret(secret: &[u8]) -> Result<Self, SignError> {
        let bytes: [u8; 32] = secret
            .try_into()
            .map_err(|_| SignError::Key(format!("expected 32 bytes, got {}", secret.len())))?;
        Ok(Self {
            signing: SigningKey::from_bytes(&bytes),
        })
    }

    pub fn secret(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn verifying(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    /// Base64 (standard, padded) of the 32-byte public key. The server stores
    /// this verbatim, so the encoding must not drift.
    pub fn public_b64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.verifying().to_bytes())
    }

    /// Sign one request and return the headers to attach.
    pub fn sign_request(&self, method: &str, path: &str, body: &[u8]) -> SignedRequest {
        let timestamp = now_unix();
        let nonce = nonce_hex();
        let canonical = canonical_string(method, path, timestamp, &nonce, body);
        let signature = self.signing.sign(canonical.as_bytes());
        SignedRequest {
            timestamp,
            nonce,
            signature_b64: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        }
    }
}

/// Headers a signed request carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedRequest {
    pub timestamp: i64,
    pub nonce: String,
    pub signature_b64: String,
}

impl SignedRequest {
    /// `(header, value)` pairs, in the names the server reads.
    pub fn headers(&self) -> [(&'static str, String); 3] {
        [
            ("x-mpa-ts", self.timestamp.to_string()),
            ("x-mpa-nonce", self.nonce.clone()),
            ("x-mpa-sig", self.signature_b64.clone()),
        ]
    }
}

/// The exact bytes the signature covers. See the module docs.
pub fn canonical_string(
    method: &str,
    path: &str,
    timestamp: i64,
    nonce: &str,
    body: &[u8],
) -> String {
    let digest = Sha256::digest(body);
    format!(
        "{}\n{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        path,
        timestamp,
        nonce,
        hex::encode(digest)
    )
}

/// Verify a signature the way the server does. Exposed for tests and for any
/// self-check that wants to rule the network out before blaming it.
pub fn verify(
    public_key_b64: &str,
    method: &str,
    path: &str,
    timestamp: i64,
    nonce: &str,
    body: &[u8],
    signature_b64: &str,
) -> Result<(), SignError> {
    let public = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64)
        .map_err(|e| SignError::Key(e.to_string()))?;
    let public: [u8; 32] = public
        .try_into()
        .map_err(|_| SignError::Key("public key is not 32 bytes".into()))?;
    let verifying = VerifyingKey::from_bytes(&public).map_err(|e| SignError::Key(e.to_string()))?;

    let signature = base64::engine::general_purpose::STANDARD
        .decode(signature_b64)
        .map_err(|e| SignError::Key(e.to_string()))?;
    let signature = Signature::from_slice(&signature).map_err(|e| SignError::Key(e.to_string()))?;

    let canonical = canonical_string(method, path, timestamp, nonce, body);
    verifying
        .verify_strict(canonical.as_bytes(), &signature)
        .map_err(|_| SignError::Key("signature does not match".into()))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn nonce_hex() -> String {
    let mut bytes = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_form_is_stable() {
        // Shared with the server implementation; changing this breaks the
        // contract on both sides at once, which is exactly the point.
        let got = canonical_string("post", "/api/agent/jobs", 1_700_000_000, "aabb", b"{}");
        assert_eq!(
            got,
            "POST\n/api/agent/jobs\n1700000000\naabb\n\
             44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[test]
    fn empty_body_hashes_the_empty_string() {
        let got = canonical_string("GET", "/api/agent/version", 1, "00", b"");
        assert!(got.ends_with("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
    }

    #[test]
    fn a_signature_verifies_and_tampering_breaks_it() {
        let key = DeviceKey::generate();
        let signed = key.sign_request("POST", "/api/agent/jobs", b"{\"a\":1}");

        verify(
            &key.public_b64(),
            "POST",
            "/api/agent/jobs",
            signed.timestamp,
            &signed.nonce,
            b"{\"a\":1}",
            &signed.signature_b64,
        )
        .expect("honest request verifies");

        // Body, path, timestamp and nonce are all covered.
        let cases: [(&str, &str, i64, &[u8]); 3] = [
            ("body", "/api/agent/jobs", signed.timestamp, b"{\"a\":2}"),
            ("path", "/api/agent/quote", signed.timestamp, b"{\"a\":1}"),
            (
                "time",
                "/api/agent/jobs",
                signed.timestamp + 1,
                b"{\"a\":1}",
            ),
        ];
        for (label, path, ts, body) in cases {
            assert!(
                verify(
                    &key.public_b64(),
                    "POST",
                    path,
                    ts,
                    &signed.nonce,
                    body,
                    &signed.signature_b64
                )
                .is_err(),
                "{label} must break the signature"
            );
        }

        assert!(verify(
            &key.public_b64(),
            "POST",
            "/api/agent/jobs",
            signed.timestamp,
            "other-nonce",
            b"{\"a\":1}",
            &signed.signature_b64
        )
        .is_err());
    }

    #[test]
    fn a_copied_secret_reproduces_the_same_public_key() {
        // Exactly why the secret file must be readable only by the device user:
        // copying it clones the identity.
        let key = DeviceKey::generate();
        let restored = DeviceKey::from_secret(&key.secret()).unwrap();
        assert_eq!(key.public_b64(), restored.public_b64());
    }

    #[test]
    fn nonces_do_not_repeat() {
        let key = DeviceKey::generate();
        let a = key.sign_request("GET", "/x", b"");
        let b = key.sign_request("GET", "/x", b"");
        assert_ne!(a.nonce, b.nonce);
        assert_eq!(a.nonce.len(), 32);
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let key = DeviceKey::generate();
        let text = format!("{key:?}");
        assert!(!text.contains(&hex::encode(key.secret())));
    }

    #[test]
    fn headers_use_the_names_the_server_reads() {
        let key = DeviceKey::generate();
        let signed = key.sign_request("GET", "/api/agent/version", b"");
        let names: Vec<&str> = signed.headers().iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["x-mpa-ts", "x-mpa-nonce", "x-mpa-sig"]);
    }
}
