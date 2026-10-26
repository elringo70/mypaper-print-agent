//! Whether this device may bill at all.
//!
//! Entitlement is checked once at startup (the CLI and the UI both do it) and
//! the answer is process-wide: there is one spooler, one install and one
//! answer. Keeping it here instead of on `AgentCore` avoids two sources of
//! truth and lets the CLI's watcher and the UI runtime consult the same flag.
//!
//! A blocked device must never pause a job. Holding a print it cannot price
//! leaves a customer waiting for something they already paid for, so jobs pass
//! through unmanaged and only the log records why.

use std::sync::{OnceLock, RwLock};

fn slot() -> &'static RwLock<Option<String>> {
    static SLOT: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Mark the device as unable to bill, with the reason shown to the operator.
/// `None` clears it (a valid token was loaded).
pub fn set(reason: Option<String>) {
    if let Ok(mut guard) = slot().write() {
        *guard = reason;
    }
}

/// Why this device may not bill, if it may not.
pub fn reason() -> Option<String> {
    slot().read().ok().and_then(|guard| guard.clone())
}

pub fn is_blocked() -> bool {
    reason().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_blocks_and_clearing_unblocks() {
        set(Some("token expirado".into()));
        assert!(is_blocked());
        assert_eq!(reason().as_deref(), Some("token expirado"));
        set(None);
        assert!(!is_blocked());
        assert_eq!(reason(), None);
    }
}
