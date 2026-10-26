#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gpui::{
    prelude::*, px, size, App, Application, AsyncApp, Bounds, Timer, WindowBounds, WindowOptions,
};
use mpa_agent::{
    default_state_path, load_into, load_policy, load_tiers, save_to, AgentCore, AgentRuntime,
    MockTickets,
};
use mpa_sync::{HttpTickets, PairingStore, SyncClient};
use mpa_ui::{tray, RootView, UiState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn usage() -> ! {
    eprintln!("usage: mpa-ui [--config <path>] [--hidden]");
    std::process::exit(2);
}

/// `MPA_DEMO_JOB=1` (plus `MPA_DEMO_PAGE=<png>`) seeds one held, already
/// analyzed job so the detail pane can be reviewed without printing anything.
/// Development aid for layout work: nothing sets these in a normal run.
fn seed_demo_job(core: &mut AgentCore) {
    let page_png = std::env::var("MPA_DEMO_PAGE")
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .unwrap_or_default();
    let pages = (1..=4)
        .map(|page_no| mpa_agent::WatchedPage {
            page_no,
            coverage: mpa_domain::Coverage::new(18.5, 6.0, 0.0, 62.0),
            tier_id: Some(if page_no == 4 { "FOTO" } else { "COLOR50" }.into()),
            included: true,
            thumbnail_png: page_png.clone(),
            area: None,
        })
        .collect();
    core.adopt_job(mpa_agent::WatchedJob {
        printer: "KONICA MINOLTA C368SeriesPCL".into(),
        job_id: 99,
        document: "Microsoft Word - demo.docx".into(),
        state: mpa_domain::JobState::Paused,
        pages,
        analysis_error: None,
        warnings: Vec::new(),
        target: mpa_agent::TargetSelection::Unassigned,
        format: None,
        spool_path: None,
        print_mode: mpa_domain::PrintMode::Bw,
        preset: None,
    });
}

fn main() {
    let mut config_arg: Option<std::path::PathBuf> = None;
    let mut hidden = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_arg = args.next().map(std::path::PathBuf::from),
            "--hidden" => hidden = true,
            "--help" | "-h" => usage(),
            _ => usage(),
        }
    }
    let (policy, cfg_path) = load_policy(config_arg.clone()).unwrap_or_else(|e| {
        eprintln!("mpa-ui: config: {e}");
        std::process::exit(1);
    });
    // Server-synced tiers win; config [[tiers]] is the offline fallback.
    let mut core = AgentCore::new(
        mpa_spool::default_backend().into(),
        Arc::new(MockTickets::demo()),
        load_tiers(&cfg_path),
    );
    let state_path = match config_arg {
        Some(mut p) => {
            p.set_file_name("agent-state.json");
            p
        }
        None => default_state_path().unwrap_or_else(|_| cfg_path.clone()),
    };
    let _ = load_into(&mut core, &state_path);
    // Mono / color roles and print presets live in config.toml: they describe
    // the installation, not the runtime state.
    core.set_roles(mpa_agent::load_roles(&cfg_path));
    core.set_presets(mpa_agent::load_presets(&cfg_path));

    // Layout/dev aid: seed one held, already-analyzed job so the detail pane can
    // be inspected without sending anything to a printer. `MPA_DEMO_PAGE` is a
    // PNG used as every page's preview. Never set in a real install.
    if std::env::var("MPA_DEMO_JOB").is_ok() {
        seed_demo_job(&mut core);
    }

    // Live tickets when paired, demo data otherwise.
    let mut refresh_credentials: Option<(String, String)> = None;
    if let Ok(pairing) = PairingStore::load(None) {
        if let Ok(client) = SyncClient::new(&pairing.server_url, Some(pairing.agent_key.clone())) {
            let tickets: Arc<dyn mpa_agent::TicketSource> = HttpTickets::new(client);
            core.set_ticket_source(tickets);
        }
        refresh_credentials = Some((pairing.server_url, pairing.agent_key));
    }
    let _ = save_to(&core, &state_path);

    mpa_agent::log::info(format!("starting mpa-ui v{}", env!("CARGO_PKG_VERSION")));

    // Offline entitlement: publish the answer process-wide before the watcher
    // starts, so a blocked device never pauses a job. Same helper the CLI uses.
    let gate_dir = cfg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let gate_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match mpa_sync::entitlement::apply_gate(&gate_dir, gate_now) {
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
    let runtime = AgentRuntime::start(Arc::clone(&core), policy, Duration::from_millis(2000));
    mpa_agent::log::info("watcher runtime started".into());

    // Ticket mirror for the UI. The source is a blocking HTTP call on paired
    // agents, so it is pumped here instead of on the UI thread (see
    // `UiState::spawn_ticket_refresh`).
    UiState::spawn_ticket_refresh(Arc::clone(&core));

    // Refresh the tabulador opportunistically; offline keeps going. On a
    // worker: this request used to run on the main thread BEFORE the window
    // existed, so a slow server stalled app start for the whole 15 s HTTP
    // timeout with nothing on screen.
    if let Some((server_url, agent_key)) = refresh_credentials {
        let core = Arc::clone(&core);
        std::thread::spawn(move || {
            let Ok(client) = SyncClient::new(&server_url, Some(agent_key)) else {
                return;
            };
            let Ok(remote) = client.get_tiers() else {
                return;
            };
            let tiers: Vec<mpa_domain::Tier> = remote.iter().map(|t| t.to_domain()).collect();
            if tiers.is_empty() {
                return;
            }
            if let Ok(mut core) = core.lock() {
                core.set_tiers(tiers);
            }
        });
    }

    // System tray (best effort: the window works without it). Built on the
    // main thread; polled from the UI tick since menu items are `!Send`.
    let tray = match tray::Tray::build(mpa_agent::AgentMode::Active) {
        Ok(tray) => {
            mpa_agent::log::info("tray icon ready".into());
            Some(tray)
        }
        Err(e) => {
            mpa_agent::log::warn(format!("tray unavailable: {e}"));
            None
        }
    };

    // Opportunistic update check shortly after launch (paired only).
    {
        let core = Arc::clone(&core);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(15));
            let Ok(pairing) = PairingStore::load(None) else {
                return;
            };
            let manifest_url = std::env::var("MPA_UPDATE_MANIFEST").ok();
            if let Ok(mpa_update::UpdateStatus::Available { manifest }) =
                mpa_update::check_for_update(
                    Some(&pairing.server_url),
                    manifest_url.as_deref(),
                    Some(&pairing.agent_key),
                    env!("CARGO_PKG_VERSION"),
                )
            {
                let version = manifest.version.clone();
                if let Ok(mut core) = core.lock() {
                    core.set_pending_update(Some(manifest));
                    core.push_notice(format!("nueva versión disponible: {version}"));
                }
            }
        });
    }

    mpa_agent::log::info("entering event loop".into());
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                show: !hidden,
                ..Default::default()
            },
            |_, cx| {
                let state_path = state_path.clone();
                let state = cx.new(|_| UiState::new(Arc::clone(&core), state_path, tray));
                let poll = state.clone();
                // Mirror the core at 2 Hz: watcher threads bump
                // `AgentCore::version` on every mutation. Tray events and the
                // quit request are also served here, on the UI thread.
                cx.spawn(async move |cx: &mut AsyncApp| {
                    // Heartbeat proves the UI thread is pumping messages; if
                    // these stop in mpa.log while the process lives, the event
                    // loop itself is stuck (platform/GPU), not our locks.
                    let mut ticks: u64 = 0;
                    loop {
                        Timer::after(Duration::from_millis(500)).await;
                        ticks += 1;
                        let step = cx.update(|cx| {
                            poll.update(cx, |s, cx| s.refresh(cx));
                            poll.update(cx, |s, _| s.take_quit_request())
                        });
                        match step {
                            Err(_) => break,
                            Ok(true) => {
                                let _ = cx.update(|cx| cx.quit());
                                break;
                            }
                            Ok(false) => {}
                        }
                        if ticks == 4 {
                            mpa_agent::log::info("window open, event loop alive".into());
                        } else if ticks.is_multiple_of(120) {
                            let jobs = cx
                                .update(|cx| poll.update(cx, |s, _| s.job_count()))
                                .unwrap_or(0);
                            mpa_agent::log::info(format!("heartbeat jobs={jobs}"));
                        }
                    }
                })
                .detach();
                cx.new(|cx| RootView::new(state, cx.focus_handle()))
            },
        )
        .expect("open window");
        cx.activate(true);
    });

    runtime.shutdown();
}
