//! Offline entitlement: a server-signed, absolute-expiry permission slip.
//!
//! The server is the authority on every request. This module answers the only
//! question the agent needs when there is no network: may this PC keep working
//! until a fixed moment in time?
//!
//! The moment is fixed by the server (`exp`) and never extended here. A local
//! "last successful check + 72 h" counter would be the wrong design: changing
//! the clock restarts it, and restoring a snapshot restarts it again, so the
//! grace period would be indefinite. With an absolute expiry the worst case is
//! one window that runs out.
//!
//! Rollback is a real (if limited) attack on a machine the customer owns, so we
//! also refuse to trust a clock that is earlier than the latest instant this
//! installation has already seen. That does not stop a VM snapshot restore —
//! nothing local can — but it stops the cheap trick.

use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Environment override, for the build machine and for tests.
pub const PUBLIC_KEY_ENV: &str = "MPA_ENTITLEMENT_PUBKEY";

/// Compiled in by the release build (`MPA_ENTITLEMENT_PUBKEY` at build time).
/// Absent means every token fails to verify, which blocks offline work instead
/// of allowing it: the safe direction.
const BUILTIN_PUBLIC_KEY: Option<&str> = option_env!("MPA_ENTITLEMENT_PUBKEY");

const TOKEN_VERSION: &str = "v1";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementPayload {
    pub agent_id: String,
    pub workspace_id: String,
    pub plan: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    pub iat: i64,
    pub exp: i64,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum EntitlementError {
    #[error("token malformed")]
    Malformed,
    #[error("token expired at {0}")]
    Expired(i64),
    #[error("token signature does not verify")]
    Signature,
    #[error("no entitlement public key configured in this build")]
    NoPublicKey,
    #[error("no entitlement token on this device: run `mpa entitlement`")]
    NoToken,
    #[error("system clock is behind the last instant this install saw ({seen}); refusing")]
    ClockRollback { seen: i64 },
}

/// The public key this build trusts: env first, then the compiled-in one.
fn trusted_public_key() -> Result<String, EntitlementError> {
    if let Ok(value) = std::env::var(PUBLIC_KEY_ENV) {
        if !value.trim().is_empty() {
            return Ok(value.trim().to_string());
        }
    }
    BUILTIN_PUBLIC_KEY
        .map(|value| value.to_string())
        .ok_or(EntitlementError::NoPublicKey)
}

/// Verify a token against this build's trusted key.
pub fn verify(token: &str, now: i64) -> Result<EntitlementPayload, EntitlementError> {
    verify_with_public(token, now, &trusted_public_key()?)
}

/// Verify against an explicit key. Kept separate so tests never have to touch
/// process-wide state (env vars are global and make parallel tests flaky).
pub fn verify_with_public(
    token: &str,
    now: i64,
    public_key_b64: &str,
) -> Result<EntitlementPayload, EntitlementError> {
    let mut parts = token.trim().split('.');
    let (Some(version), Some(payload_b64), Some(signature_b64), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(EntitlementError::Malformed);
    };
    if version != TOKEN_VERSION {
        return Err(EntitlementError::Malformed);
    }

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload_bytes = engine
        .decode(payload_b64)
        .map_err(|_| EntitlementError::Malformed)?;
    let signature_bytes = engine
        .decode(signature_b64)
        .map_err(|_| EntitlementError::Malformed)?;
    let payload: EntitlementPayload =
        serde_json::from_slice(&payload_bytes).map_err(|_| EntitlementError::Malformed)?;

    let public = base64::engine::general_purpose::STANDARD
        .decode(public_key_b64.trim())
        .map_err(|_| EntitlementError::NoPublicKey)?;
    let public: [u8; 32] = public
        .try_into()
        .map_err(|_| EntitlementError::NoPublicKey)?;
    let verifying = VerifyingKey::from_bytes(&public).map_err(|_| EntitlementError::NoPublicKey)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| EntitlementError::Signature)?;
    verifying
        .verify_strict(&payload_bytes, &signature)
        .map_err(|_| EntitlementError::Signature)?;

    if payload.exp <= now {
        return Err(EntitlementError::Expired(payload.exp));
    }
    Ok(payload)
}

pub fn has_feature(payload: &EntitlementPayload, feature: &str) -> bool {
    payload.features.iter().any(|f| f == feature)
}

/// File names the agent keeps next to its config.
pub const TOKEN_FILE: &str = "entitlement.jwt";
pub const GUARD_FILE: &str = "entitlement-guard.json";

/// Authorize from the files this installation already has.
///
/// `config_dir` is the directory holding `config.toml`; the token and the
/// clock mark live beside it. Returns the payload when the device may work, and
/// a reason when it may not — the caller decides what "blocked" means, because
/// only it knows how to leave a customer's job unmanaged instead of trapped.
pub fn authorize_local(
    config_dir: &std::path::Path,
    now: i64,
) -> Result<EntitlementPayload, EntitlementError> {
    let token = std::fs::read_to_string(config_dir.join(TOKEN_FILE))
        .map_err(|_| EntitlementError::NoToken)?;
    let guard_path = config_dir.join(GUARD_FILE);
    let mut guard = ClockGuard::load(&guard_path);
    let payload = guard.authorize(token.trim(), now)?;
    guard.save(&guard_path);
    Ok(payload)
}

/// What the startup check concluded.
#[derive(Debug, Clone, PartialEq)]
pub enum GateOutcome {
    /// A valid token: the device may bill until `payload.exp`.
    Allowed(EntitlementPayload),
    /// No token on disk. Not an error while the feature rolls out: the device
    /// keeps working and the caller should say so out loud.
    NoToken,
    /// A token exists and does not authorize: expired, tampered, foreign key,
    /// or the clock moved backwards.
    Blocked(String),
}

/// Evaluate the offline entitlement and publish the answer to
/// [`mpa_agent::gate`], which both watchers consult before pausing a job.
///
/// Transition rule: "no token" keeps the old behaviour so an installation that
/// predates the feature is not bricked; anything else that fails blocks.
pub fn apply_gate(config_dir: &std::path::Path, now: i64) -> GateOutcome {
    let outcome = match authorize_local(config_dir, now) {
        Ok(payload) => GateOutcome::Allowed(payload),
        Err(EntitlementError::NoToken) => GateOutcome::NoToken,
        Err(e) => GateOutcome::Blocked(e.to_string()),
    };
    match &outcome {
        GateOutcome::Blocked(reason) => mpa_agent::gate::set(Some(reason.clone())),
        _ => mpa_agent::gate::set(None),
    }
    outcome
}

/// High-water mark of the wall clock, so a rolled-back clock is visible.
///
/// `seen` is the latest instant this installation has ever observed. It is
/// persisted next to the agent state; anything earlier than `seen - skew` means
/// the clock moved backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClockGuard {
    pub seen: i64,
}

/// Tolerance for NTP corrections and timezone-less clocks.
pub const CLOCK_SKEW_SECS: i64 = 300;

impl ClockGuard {
    /// Advance the mark and reject a clock that went backwards.
    pub fn observe(&mut self, now: i64) -> Result<(), EntitlementError> {
        if now < self.seen - CLOCK_SKEW_SECS {
            return Err(EntitlementError::ClockRollback { seen: self.seen });
        }
        if now > self.seen {
            self.seen = now;
        }
        Ok(())
    }

    /// Convenience: guard, then verify against this build's key. Both pass or
    /// neither does.
    pub fn authorize(
        &mut self,
        token: &str,
        now: i64,
    ) -> Result<EntitlementPayload, EntitlementError> {
        self.observe(now)?;
        verify(token, now)
    }

    /// Read the mark from disk; a missing or unreadable file means "no mark".
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(raw.trim()).ok())
            .unwrap_or_default()
    }

    /// Persist the mark. Best effort: losing it costs one rollback check, not
    /// the entitlement.
    pub fn save(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(raw) = serde_json::to_string(self) {
            let _ = std::fs::write(path, raw);
        }
    }

    /// Same, with an explicit key (tests and the doctor command).
    pub fn authorize_with_public(
        &mut self,
        token: &str,
        now: i64,
        public_key_b64: &str,
    ) -> Result<EntitlementPayload, EntitlementError> {
        self.observe(now)?;
        verify_with_public(token, now, public_key_b64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixtures were minted by the TypeScript implementation
    /// (`issueEntitlement`) with this key, so this suite is also the
    /// cross-language check that both sides read the same format.
    const PUBLIC: &str = "uUIuEAG0xYmSSrIQpURHOdesr5Aq8x/AN6FKe0UDHWg=";

    fn valid_token() -> String {
        include_str!("../tests/fixtures/entitlement_valid.jwt")
            .trim()
            .to_string()
    }

    fn expired_token() -> String {
        include_str!("../tests/fixtures/entitlement_expired.jwt")
            .trim()
            .to_string()
    }

    fn check(token: &str, now: i64) -> Result<EntitlementPayload, EntitlementError> {
        verify_with_public(token, now, PUBLIC)
    }

    #[test]
    fn a_fresh_token_verifies() {
        let payload = check(&valid_token(), 1_700_000_000).expect("verifies");
        assert_eq!(payload.agent_id, "a1");
        assert_eq!(payload.workspace_id, "w1");
        assert!(has_feature(&payload, "print:agent"));
        assert!(!has_feature(&payload, "otra:cosa"));
    }

    #[test]
    fn an_expired_token_does_not() {
        assert_eq!(
            check(&expired_token(), 1_700_000_001),
            Err(EntitlementError::Expired(1_600_000_000))
        );
    }

    #[test]
    fn a_tampered_token_does_not() {
        let token = valid_token();
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            br#"{"agentId":"a1","workspaceId":"w1","plan":"PREMIUM","features":["print:agent"],"iat":1700000000,"exp":9999999999}"#,
        );
        parts[1] = &forged;
        assert_eq!(
            check(&parts.join("."), 1_700_000_000),
            Err(EntitlementError::Signature)
        );
    }

    #[test]
    fn a_malformed_token_does_not() {
        assert_eq!(check("", 0), Err(EntitlementError::Malformed));
        assert_eq!(check("v2.a.b", 0), Err(EntitlementError::Malformed));
        assert_eq!(check("v1.only-two", 0), Err(EntitlementError::Malformed));
        assert_eq!(check("v1.@@@.###", 0), Err(EntitlementError::Malformed));
    }

    #[test]
    fn a_token_from_another_key_does_not() {
        let other = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        assert!(
            check(&valid_token(), 1_700_000_000).is_ok(),
            "the honest key works"
        );
        assert!(verify_with_public(&valid_token(), 1_700_000_000, other).is_err());
    }

    #[test]
    fn without_a_key_everything_fails_closed() {
        std::env::remove_var(PUBLIC_KEY_ENV);
        // The test build has no compiled-in key, so this is the production
        // behaviour of an unconfigured server.
        if BUILTIN_PUBLIC_KEY.is_none() {
            assert_eq!(
                verify(&valid_token(), 1_700_000_000),
                Err(EntitlementError::NoPublicKey)
            );
        }
    }

    #[test]
    fn the_clock_guard_notices_a_rollback_beyond_the_skew() {
        let mut guard = ClockGuard::default();
        guard.observe(1_700_000_000).unwrap();
        // Small corrections are fine.
        guard.observe(1_700_000_000 - CLOCK_SKEW_SECS).unwrap();
        // A jump backwards beyond the tolerance is not.
        assert_eq!(
            guard.observe(1_700_000_000 - CLOCK_SKEW_SECS - 1),
            Err(EntitlementError::ClockRollback {
                seen: 1_700_000_000
            })
        );
    }

    #[test]
    fn the_guard_never_moves_backwards() {
        let mut guard = ClockGuard::default();
        guard.observe(100).unwrap();
        guard.observe(50).unwrap();
        assert_eq!(guard.seen, 100);
    }

    #[test]
    fn authorize_enforces_both_rules() {
        let mut guard = ClockGuard::default();
        guard.observe(1_700_000_000).unwrap();
        // Expired token, fresh clock: rejected on expiry.
        assert!(matches!(
            guard.authorize_with_public(&expired_token(), 1_700_000_000, PUBLIC),
            Err(EntitlementError::Expired(_))
        ));
        // Rolled-back clock: rejected before the token is even considered.
        assert!(matches!(
            guard.authorize_with_public(&valid_token(), 1_600_000_000, PUBLIC),
            Err(EntitlementError::ClockRollback { .. })
        ));
    }
}
