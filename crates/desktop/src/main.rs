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
use mpa_sync::{HttpTickets, PairingStore};
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
    // Stamp the product version before any SyncClient exists: the server logs
    // it in `X-Agent-Version` and the support table shows the shipped build,
    // not mpa-sync's crate number.
    mpa_sync::set_agent_version(env!("CARGO_PKG_VERSION"));

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
        if let Ok(client) = mpa_sync::connect(&pairing.server_url, Some(pairing.agent_key.clone())) {
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
            // Price for the pilot journal. A failure (unpaired, offline) leaves
            // the measurement in place and the price unknown, which the report
            // shows as "sin precio" instead of inventing one.
            let quote_client = mpa_sync::PairingStore::load(None)
                .ok()
                .and_then(|pairing| {
                    mpa_sync::connect(&pairing.server_url, Some(pairing.agent_key)).ok()
                })
                .map(|client| Arc::new(Mutex::new(client)));
            let quote = move |pages: &[mpa_agent::pilot::PilotPage]| -> Option<(f64, f64)> {
                let client = quote_client.as_ref()?.lock().ok()?;
                let request: Vec<mpa_sync::QuotePage> = pages
                    .iter()
                    .map(|p| mpa_sync::QuotePage {
                        page_no: p.page_no,
                        c: p.c,
                        m: p.m,
                        y: p.y,
                        k: p.k,
                        include: true,
                    })
                    .collect();
                let quote = client.quote(&request).ok()?;
                Some((
                    quote.for_mode(mpa_domain::PrintMode::Bw).total,
                    quote.for_mode(mpa_domain::PrintMode::Color).total,
                ))
            };
            if let Err(e) =
                runtime.start_capture(listener, inbox.clone(), mpa_ipp::DEFAULT_PORT, quote)
            {
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
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // One capture endpoint per machine. A second launch — the installer
            // opens the window and setup-agent.ps1 also starts the agent — used
            // to run capture-less and leave two identical windows on screen;
            // closing the wrong one silently killed billing for every job.
            mpa_agent::log::info(format!(
                "ya hay una instancia escuchando en el puerto {}; salgo",
                mpa_ipp::DEFAULT_PORT
            ));
            return;
        }
        Err(e) => mpa_agent::log::warn(format!(
            "puerto {} ocupado, sin captura IPP: {e}",
            mpa_ipp::DEFAULT_PORT
        )),
    }

    spawn_ticket_refresh(Arc::clone(&core));
    spawn_tier_refresh(Arc::clone(&core), state_path.clone());
    spawn_outbox_drain(Arc::clone(&core));

    // Local bridge for the webapp running in the browser: the analysis is handed
    // over on loopback, never through the server. A busy port is reported, not
    // fatal — the agent still captures and holds jobs.
    match mpa_local_api::serve(
        Arc::clone(&core),
        mpa_local_api::DEFAULT_PORT,
        env!("CARGO_PKG_VERSION"),
    ) {
        Ok(api) => mpa_agent::log::info(format!("puente local en http://{}", api.addr)),
        Err(e) => mpa_agent::log::warn(format!("puente local no disponible: {e}")),
    }
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
            commands::version,
            commands::snapshot,
            commands::page_thumbnail,
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
            // Started by the logon task with --minimized: the agent captures
            // from the tray without covering the screen.
            if std::env::args().any(|arg| arg == "--minimized") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it to the tray: the agent keeps holding
            // jobs, and quitting is an explicit menu action.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
            // Minimising goes to the tray too: the window is the operator's view
            // of the agent, never the agent itself. Windows reports a minimised
            // window as 0x0.
            if let tauri::WindowEvent::Resized(size) = event {
                if size.width == 0 && size.height == 0 {
                    let _ = window.hide();
                }
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
                // The operator just opened the window: refresh the ticket list
                // now instead of on the next interval.
                request_refresh();
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
/// Set when the window is opened, so the refresh loops run on the next second
/// instead of waiting for their full interval.
static REFRESH_NOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn request_refresh() {
    REFRESH_NOW.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn spawn_ticket_refresh(core: Arc<Mutex<AgentCore>>) {
    std::thread::spawn(move || loop {
        // Cleared before the fetch: a request that arrives mid-flight is not
        // lost, it just lands on the next pass.
        REFRESH_NOW.store(false, std::sync::atomic::Ordering::Relaxed);
        // Fetch first: sleeping before the first call left the destination
        // picker empty for the first half minute, which reads as "no sync".
        let source = match core.lock() {
            Ok(core) => core.ticket_source(),
            Err(_) => {
                std::thread::sleep(Duration::from_secs(30));
                continue;
            }
        };
        match source.list_open_tickets() {
            Ok(tickets) => {
                if let Ok(mut core) = core.lock() {
                    core.set_cached_tickets(tickets);
                }
            }
            Err(e) => mpa_agent::log::warn(format!("tickets: {e}")),
        }
        // One-second slices so opening the window refreshes the list now
        // instead of up to 30 s later.
        for _ in 0..30 {
            if REFRESH_NOW.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

/// Pull the tariff ladder, metrics and rule set the POS prices with.
///
/// Without this the agent kept whatever `mpa tiers sync` last wrote — a manual
/// command nobody runs after editing the tabulador — so the window showed a
/// range and a price the ticket would never charge.
fn spawn_tier_refresh(core: Arc<Mutex<AgentCore>>, state_path: std::path::PathBuf) {
    std::thread::spawn(move || loop {
        if let Ok(pairing) = PairingStore::load(None) {
            if let Ok(client) = mpa_sync::connect(&pairing.server_url, Some(pairing.agent_key)) {
                match client.get_tiers_sync() {
                    Ok(sync) => {
                        let count = sync.tiers.len();
                        if let Ok(mut core) = core.lock() {
                            core.set_tiers(sync.tiers);
                            core.set_metrics(sync.metrics);
                            core.set_pricing(sync.pricing);
                            core.set_tier_prices(sync.prices);
                            let _ = save_to(&core, &state_path);
                        }
                        mpa_agent::log::info(format!("tarifas del servidor: {count} niveles"));
                    }
                    Err(e) => mpa_agent::log::warn(format!("tarifas: {e}")),
                }
            }
        }
        std::thread::sleep(Duration::from_secs(300));
    });
}

/// Push completed work to MyPaper and finish any pending reprint.
///
/// Nothing did this in the shipped window: "Imprimir" queued an outbox entry
/// that neither reached the server nor the printer, so the operator's work was
/// lost with the process. Same discipline as the ticket refresh: own thread,
/// never under the core lock (the drain locks briefly and works unlocked).
fn spawn_outbox_drain(core: Arc<Mutex<AgentCore>>) {
    std::thread::spawn(move || loop {
        let pending = core.lock().map(|c| c.pending_outbox()).unwrap_or(0);
        if pending > 0 {
            match PairingStore::load(None) {
                Ok(pairing) => {
                    match mpa_sync::connect(&pairing.server_url, Some(pairing.agent_key)) {
                        Ok(client) => {
                            let report = mpa_sync::drain_outbox(&core, &client);
                            if report.has_work_left() {
                                mpa_agent::log::warn(format!("outbox: {}", report.summary()));
                            } else {
                                mpa_agent::log::info(format!("outbox: {}", report.summary()));
                            }
                        }
                        Err(e) => mpa_agent::log::warn(format!("outbox: {e}")),
                    }
                }
                Err(e) => mpa_agent::log::warn(format!("outbox sin enlace: {e}")),
            }
        }
        std::thread::sleep(Duration::from_secs(10));
    });
}
