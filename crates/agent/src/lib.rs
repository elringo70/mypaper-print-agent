pub mod audit;
pub mod config;
pub mod core;
pub mod log;
pub mod persist;
pub mod reprint;
pub mod runtime;
pub mod tickets;

pub use audit::{default_audit_path, tail_lines};
pub use config::{
    config_path, load_metrics, load_policy, load_presets, load_roles, load_tiers, save_metrics,
    save_policy, save_presets, save_roles, save_tiers, ConfigError,
};
pub use core::{
    analyze_spool, detect, AgentCore, AgentError, AgentMode, AnalysisFailure, AnalyzedPage,
    AnalyzedSpool, Completion, Detected, OutboxEntry, OutboxPage, PauseOutcome, TargetSelection,
    WatchedJob, WatchedPage,
};
pub use persist::{default_state_path, load_into, save_to};
pub use reprint::{
    attempt_reprint, spool_cache_dir, sweep_one_reprint, ReprintError, SweepOutcome,
};
pub use runtime::AgentRuntime;
pub use tickets::{MockTickets, Ticket, TicketError, TicketSource};
