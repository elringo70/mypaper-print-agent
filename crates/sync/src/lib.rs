pub mod client;
pub mod drain;
pub mod entitlement;
pub mod pairing;
pub mod sign;
pub mod tickets;

pub use client::{
    EntitlementToken, MaterializeAck, Pairing, Quote, QuoteLine, QuoteMode, QuoteModes, QuotePage,
    RemoteTier, SyncClient, SyncError, TierSync, VersionInfo,
};
pub use drain::{drain_outbox, DrainReport};
pub use entitlement::{
    apply_gate, has_feature, verify as verify_entitlement, ClockGuard, EntitlementError,
    EntitlementPayload, GateOutcome,
};
pub use pairing::{PairingError, PairingStore};
pub use tickets::HttpTickets;

pub use sign::{canonical_string, verify, DeviceKey, SignError, SignedRequest};
