//! The startup gate, end to end over real token files.
//!
//! This is the code path both the CLI and the UI run before the watcher starts.
//! It is an integration test so it owns a process: `mpa_agent::gate` is global
//! state and the public key arrives through an environment variable.

use mpa_sync::entitlement::{apply_gate, GateOutcome};

const PUBLIC: &str = "uUIuEAG0xYmSSrIQpURHOdesr5Aq8x/AN6FKe0UDHWg=";
const VALID: &str = include_str!("fixtures/entitlement_valid.jwt");
const EXPIRED: &str = include_str!("fixtures/entitlement_expired.jwt");

fn workdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mpa-gate-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_token(dir: &std::path::Path, token: &str) {
    std::fs::write(dir.join("entitlement.jwt"), token).unwrap();
}

#[test]
fn a_valid_token_opens_the_gate() {
    std::env::set_var("MPA_ENTITLEMENT_PUBKEY", PUBLIC);
    let dir = workdir("valid");
    write_token(&dir, VALID);

    match apply_gate(&dir, 1_700_000_000) {
        GateOutcome::Allowed(payload) => {
            assert_eq!(payload.plan.as_deref(), Some("PREMIUM"));
            assert!(mpa_sync::has_feature(&payload, "print:agent"));
        }
        other => panic!("expected Allowed, got {other:?}"),
    }
    assert!(!mpa_agent::gate::is_blocked(), "a valid token unblocks");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_expired_token_blocks_and_never_pauses() {
    std::env::set_var("MPA_ENTITLEMENT_PUBKEY", PUBLIC);
    let dir = workdir("expired");
    write_token(&dir, EXPIRED);

    match apply_gate(&dir, 1_700_000_000) {
        GateOutcome::Blocked(reason) => assert!(reason.contains("expired"), "{reason}"),
        other => panic!("expected Blocked, got {other:?}"),
    }
    assert!(mpa_agent::gate::is_blocked());
    // The runtime consults this before pausing: blocked means pass through.
    assert!(mpa_agent::gate::reason().is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_token_is_not_a_block_while_the_feature_rolls_out() {
    std::env::set_var("MPA_ENTITLEMENT_PUBKEY", PUBLIC);
    let dir = workdir("missing");

    assert_eq!(apply_gate(&dir, 1_700_000_000), GateOutcome::NoToken);
    assert!(
        !mpa_agent::gate::is_blocked(),
        "an install that predates the feature must keep working"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_clock_that_moved_backwards_blocks() {
    std::env::set_var("MPA_ENTITLEMENT_PUBKEY", PUBLIC);
    let dir = workdir("rollback");
    write_token(&dir, VALID);

    // First run at T sees the mark.
    assert!(matches!(
        apply_gate(&dir, 1_700_000_000),
        GateOutcome::Allowed(_)
    ));
    // Restoring the clock to before the token's issuance is caught.
    match apply_gate(&dir, 1_600_000_000) {
        GateOutcome::Blocked(reason) => assert!(reason.contains("clock"), "{reason}"),
        other => panic!("expected Blocked, got {other:?}"),
    }
    assert!(mpa_agent::gate::is_blocked());
    let _ = std::fs::remove_dir_all(&dir);
}
