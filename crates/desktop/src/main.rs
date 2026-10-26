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
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;

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
        mpa_sync::GateOutcome::NoToken => mpa_agent::log::warn(
            "entitlement: sin token local; se opera sin control de plan".into(),
        ),
        mpa_sync::GateOutcome::Blocked(reason) => {
            mpa_agent::log::warn(format!("entitlement bloqueado: {reason}"))
        }
    }

    let core = Arc::new(Mutex::new(core));
    // The spool watcher is retired from the normal flow: it can only ever see
    // the driver's PCL/PostScript, which cannot be measured. `mpa watch` still
    // runs it as a diagnostic, and the config can bring it back.
    let watched_policy = if mpa_agent::config::legacy_spool_watch(None) {
        policy
    } else {
        mpa_agent::log::info("vigilante de spool desactivado: la captura es la cola IPP".into());
        mpa_domain::PrinterPolicy {
            mode: mpa_domain::PolicyMode::Allowlist,
            watched: Vec::new(),
            ignored: Vec::new(),
        }
    };
    let mut runtime = AgentRuntime::start(
        Arc::clone(&core),
        watched_policy,
        Duration::from_millis(2000),
    );

    // Capture endpoint. This is the path that replaced watching the physical
    // printer: Windows renders the job and hands it over as PDF. A busy port is
    // reported, not fatal — the spool watcher keeps working while it is fixed.
    let inbox = mpa_agent::config::config_path(None)
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("ipp-inbox")))
        .unwrap_or_else(std::env::temp_dir);
    match std::net::TcpListener::bind(("127.0.0.1", mpa_ipp::DEFAULT_PORT)) {
        Ok(listener) => {
            if let Err(e) = runtime.start_capture(listener, inbox.clone(), mpa_ipp::DEFAULT_PORT) {
                mpa_agent::log::warn(format!("captura IPP no arrancó: {e}"));
            } else {
                mpa_agent::log::info(format!(
                    "captura IPP en http://127.0.0.1:{}{} -> {}",
                    mpa_ipp::DEFAULT_PORT,
                    mpa_ipp::PRINT_PATH,
                    inbox.display()
                ));
            }
        }
        Err(e) => mpa_agent::log::warn(format!(
            "puerto {} ocupado, sin captura IPP: {e}",
            mpa_ipp::DEFAULT_PORT
        )),
    }

    spawn_ticket_refresh(Arc::clone(&core));
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
            commands::select_target,
            commands::set_preset,
            commands::set_print_mode,
            commands::quote,
            commands::doctor,
        ])
        .setup(|app| {
            build_tray(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it to the tray: the agent keeps holding
            // jobs, and quitting is an explicit menu action.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("the window failed to start");
}

/// Tray icon and menu: show the window, or quit for real.
fn build_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let open = MenuItem::with_id(app, "open", "Abrir MyPaper", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or("el binario no tiene icono embebido")?;

    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("MyPaper Print Agent")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Refresh the open-ticket list off the core lock, the same discipline the
/// watcher and the reprint sweep follow: the window polls every second and must
/// never wait on an HTTP call.
fn spawn_ticket_refresh(core: Arc<Mutex<AgentCore>>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(30));
        let source = match core.lock() {
            Ok(core) => core.ticket_source(),
            Err(_) => continue,
        };
        if let Ok(tickets) = source.list_open_tickets() {
            if let Ok(mut core) = core.lock() {
                core.set_cached_tickets(tickets);
            }
        }
    });
}
