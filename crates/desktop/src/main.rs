//! MyPaper Print Agent — desktop shell.
//!
//! Tauri hosts the window; everything that matters (spooler, measurement,
//! pricing, sync) is the same Rust core the CLI uses. The webview only talks to
//! the four commands in `commands.rs`, and it loads **only local assets**: the
//! process runs elevated to read the spool folder, so a remote page inside it
//! would be a privilege escalation waiting to happen.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod dto;

use mpa_agent::{
    default_state_path, load_into, load_policy, load_presets, load_roles, load_tiers, save_to,
    AgentCore, AgentRuntime, MockTickets,
};
use mpa_sync::{HttpTickets, PairingStore, SyncClient};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct AppState {
    pub core: Arc<Mutex<AgentCore>>,
    pub runtime: Mutex<Option<AgentRuntime>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The core has no Debug on purpose (it holds spool handles); the
        // window never prints this, but the workspace lint asks for it.
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

fn main() {
    let (policy, cfg_path) = load_policy(None).unwrap_or_else(|e| {
        eprintln!("mpa-desktop: config: {e}");
        std::process::exit(1);
    });

    let mut core = AgentCore::new(
        mpa_spool::default_backend().into(),
        Arc::new(MockTickets::demo()),
        load_tiers(&cfg_path),
    );
    let state_path = default_state_path().unwrap_or_else(|_| cfg_path.clone());
    let _ = load_into(&mut core, &state_path);
    core.set_roles(load_roles(&cfg_path));
    core.set_presets(load_presets(&cfg_path));

    // Live tickets when paired; the window shows the demo list otherwise.
    if let Ok(pairing) = PairingStore::load(None) {
        if let Ok(client) = SyncClient::new(&pairing.server_url, Some(pairing.agent_key.clone())) {
            let tickets: Arc<dyn mpa_agent::TicketSource> = HttpTickets::new(client);
            core.set_ticket_source(tickets);
        }
    }
    let _ = save_to(&core, &state_path);

    // Entitlement first: a blocked device must not pause a single job.
    let gate_dir = cfg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match mpa_sync::entitlement::apply_gate(&gate_dir, now) {
        mpa_sync::GateOutcome::Allowed(payload) => mpa_agent::log::info(format!(
            "entitlement: plan {} hasta {}",
            payload.plan.as_deref().unwrap_or("-"),
            payload.exp
        )),
        mpa_sync::GateOutcome::NoToken => {
            mpa_agent::log::warn("entitlement: sin token local; se opera sin control de plan".into())
        }
        mpa_sync::GateOutcome::Blocked(reason) => {
            mpa_agent::log::warn(format!("entitlement bloqueado: {reason}"))
        }
    }

    let core = Arc::new(Mutex::new(core));
    let runtime = AgentRuntime::start(Arc::clone(&core), policy, Duration::from_millis(2000));
    mpa_agent::log::info(format!(
        "starting mpa-desktop v{}",
        env!("CARGO_PKG_VERSION")
    ));

    tauri::Builder::default()
        .manage(AppState {
            core,
            runtime: Mutex::new(Some(runtime)),
        })
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::toggle_page,
            commands::complete_job,
            commands::cancel_job,
            commands::doctor,
        ])
        .run(tauri::generate_context!())
        .expect("the window failed to start");
}