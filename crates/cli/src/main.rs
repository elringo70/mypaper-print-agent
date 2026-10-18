use clap::{Parser, Subcommand, ValueEnum};
use mpa_agent::{
    default_state_path, load_into, load_policy, load_tiers, save_policy, save_to, AgentCore,
    ConfigError, MockTickets, OutboxEntry, OutboxPage, TargetSelection,
};
use mpa_analyzer::{analyze_file, capture_spool_file, Ghostscript};
use mpa_domain::{classify_coverage, transition, Coverage, JobEvent, JobState, PolicyMode, Tier};
use mpa_spool::{default_backend, SpoolBackend, SpoolError};
use mpa_sync::{drain_outbox, PairingStore, SyncClient};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "mpa", about = "MyPaper print agent — Phase 1: spool watcher")]
struct Cli {
    /// Config file override (default: OS config dir + MPA_CONFIG env).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List printers visible to the spooler.
    ListPrinters,
    /// Watch printers and pause every new job (Ctrl-C stops).
    Watch {
        /// Poll slice per printer in ms (notification wait quantum).
        #[arg(long, default_value_t = 2000)]
        quantum_ms: u64,
    },
    /// Control a single job: pause | resume | cancel.
    Job {
        action: JobAction,
        printer: String,
        job_id: u32,
    },
    /// Show or edit the watched/ignored printer lists.
    Printers {
        #[command(subcommand)]
        action: Option<PrinterAction>,
    },
    /// Analyze a file (image/PDF/PS) into per-page CMYK coverage.
    Analyze {
        file: PathBuf,
        /// Tiers as id:min:max,... (e.g. "t6:0:50,t10:50.01:90").
        #[arg(long)]
        tiers: Option<String>,
        /// Write page thumbnails (thumb-<n>.png) into this dir.
        #[arg(long)]
        thumbs_dir: Option<PathBuf>,
    },
    /// Copy the raw spool file of a paused job for inspection.
    Capture {
        printer: String,
        job_id: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// Windows logon autostart for the UI (on | off | status).
    Autostart { action: AutostartAction },
    /// Pair this PC with MyPaper using the web QR code.
    Pair {
        code: String,
        #[arg(long)]
        pc_name: String,
        #[arg(long)]
        server: String,
    },
    /// Push pending drafts to MyPaper and retry pending reprints.
    Sync,
    /// Send a sample print analysis to MyPaper and read back what it stored
    /// (link test: needs pairing; no printer, no spooler, nothing printed).
    PushTest {
        /// Server base URL (default: the paired server).
        #[arg(long)]
        server: Option<String>,
        /// Agent key (default: the paired key).
        #[arg(long)]
        key: Option<String>,
        /// How many analyzed pages to send.
        #[arg(long, default_value_t = 2)]
        pages: u32,
        /// Per-page CMYK coverage percent, e.g. "12,8,0,60".
        #[arg(long)]
        coverage: Option<String>,
        /// Attach the draft to this ticket (order id) and materialize it.
        #[arg(long)]
        ticket: Option<String>,
        /// Reuse an idempotency key (a retry must return the same draft).
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Tier catalog: show local or sync from the server.
    Tiers {
        #[command(subcommand)]
        action: Option<TierAction>,
    },
    /// Show pending outbox drafts.
    Outbox,
    /// Release channel: check for a newer build or install it.
    Update {
        #[command(subcommand)]
        action: UpdateAction,
    },
    /// Show the local accountability trail (who held what, when).
    Audit {
        /// How many recent entries to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Self-diagnostics: spooler, tools, pairing, server, disk, backlog.
    Doctor,
}

#[derive(Subcommand)]
enum UpdateAction {
    /// Check the release channel without downloading anything.
    Check,
    /// Download, verify (sha256 + minisign) and hand off to the installer.
    Apply {
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum TierAction {
    /// Download the tabulador from MyPaper (needs pairing).
    Sync,
}

#[derive(Clone, Copy, ValueEnum)]
enum AutostartAction {
    On,
    Off,
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum JobAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Subcommand)]
enum PrinterAction {
    /// Only these printers are watched.
    Watch { name: String },
    /// This printer is skipped.
    Ignore { name: String },
    /// Remove a printer from both lists.
    Clear { name: String },
    /// Switch policy: allowlist (only watched) or denylist (all but ignored).
    Mode { mode: ModeArg },
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Allowlist,
    Denylist,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("mpa: error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), AppError> {
    let cli = Cli::parse();
    let config_override = cli.config.clone();
    let backend = default_backend();

    match cli.command {
        Commands::ListPrinters => {
            let printers = backend.list_printers()?;
            if printers.is_empty() {
                println!("no printers found");
            }
            for p in printers {
                println!("{}\t{}", p.name, p.status);
            }
        }
        Commands::Job {
            action,
            printer,
            job_id,
        } => {
            match action {
                JobAction::Pause => backend.pause_job(&printer, job_id)?,
                JobAction::Resume => backend.resume_job(&printer, job_id)?,
                JobAction::Cancel => backend.cancel_job(&printer, job_id)?,
            }
            let verb = match action {
                JobAction::Pause => "paused",
                JobAction::Resume => "resumed",
                JobAction::Cancel => "cancelled",
            };
            println!("job {job_id} on {printer} {verb}");
        }
        Commands::Printers { action } => {
            let (mut policy, path) = load_policy(cli.config)?;
            match action {
                None => {
                    let mode = match policy.mode {
                        PolicyMode::Allowlist => "allowlist",
                        PolicyMode::Denylist => "denylist",
                    };
                    println!("mode: {mode}");
                    println!("watched: {}", policy.watched.join(", "));
                    println!("ignored: {}", policy.ignored.join(", "));
                    println!("config: {}", path.display());
                }
                Some(PrinterAction::Watch { name }) => {
                    policy.ignored.retain(|n| n != &name);
                    if !policy.watched.contains(&name) {
                        policy.watched.push(name);
                    }
                    save_policy(&policy, &path)?;
                    println!("saved to {}", path.display());
                }
                Some(PrinterAction::Ignore { name }) => {
                    policy.watched.retain(|n| n != &name);
                    if !policy.ignored.contains(&name) {
                        policy.ignored.push(name);
                    }
                    save_policy(&policy, &path)?;
                    println!("saved to {}", path.display());
                }
                Some(PrinterAction::Clear { name }) => {
                    policy.watched.retain(|n| n != &name);
                    policy.ignored.retain(|n| n != &name);
                    save_policy(&policy, &path)?;
                    println!("saved to {}", path.display());
                }
                Some(PrinterAction::Mode { mode }) => {
                    policy.mode = match mode {
                        ModeArg::Allowlist => PolicyMode::Allowlist,
                        ModeArg::Denylist => PolicyMode::Denylist,
                    };
                    save_policy(&policy, &path)?;
                    println!("saved to {}", path.display());
                }
            }
        }
        Commands::Watch { quantum_ms } => {
            watch_loop(
                backend,
                cli.config,
                Duration::from_millis(quantum_ms.max(250)),
            )?;
        }
        Commands::Analyze {
            file,
            tiers,
            thumbs_dir,
        } => {
            analyze_command(file, tiers, thumbs_dir)?;
        }
        Commands::Capture {
            printer,
            job_id,
            out,
        } => {
            let spool = capture_spool_file(job_id, &printer).map_err(AppError::Capture)?;
            std::fs::write(&out, &spool.data)?;
            println!(
                "captured\tjob {job_id}\t{} bytes\t{:?}\t-> {}",
                spool.data.len(),
                spool.format,
                out.display()
            );
        }
        Commands::Autostart { action } => {
            autostart_command(action)?;
        }
        Commands::Pair {
            code,
            pc_name,
            server,
        } => {
            pair_command(&code, &pc_name, &server)?;
        }
        Commands::Sync => {
            sync_command(config_override)?;
        }
        Commands::PushTest {
            server,
            key,
            pages,
            coverage,
            ticket,
            idempotency_key,
        } => {
            push_test_command(server, key, pages, coverage, ticket, idempotency_key)?;
        }
        Commands::Tiers { action } => {
            tiers_command(action, config_override)?;
        }
        Commands::Outbox => {
            outbox_command(config_override)?;
        }
        Commands::Update { action } => {
            update_command(action)?;
        }
        Commands::Audit { limit } => {
            audit_command(limit)?;
        }
        Commands::Doctor => {
            doctor_command(config_override)?;
        }
    }
    Ok(())
}

fn audit_command(limit: usize) -> Result<(), AppError> {
    let path = mpa_agent::audit::default_audit_path().map_err(AppError::Config)?;
    if !path.is_file() {
        println!("sin registros todavía ({})", path.display());
        return Ok(());
    }
    for line in mpa_agent::audit::tail_lines(&path, limit) {
        println!("{line}");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Health {
    Ok,
    Warn,
    Fail,
}

fn doctor_command(config_override: Option<PathBuf>) -> Result<(), AppError> {
    let mut rows: Vec<(Health, String)> = Vec::new();
    let check = |rows: &mut Vec<(Health, String)>, health: Health, msg: String| {
        rows.push((health, msg));
    };

    check(
        &mut rows,
        Health::Ok,
        format!("{} on {}", std::env::consts::OS, std::env::consts::ARCH),
    );

    // Spooler reachability.
    let backend = default_backend();
    match backend.list_printers() {
        Ok(printers) => check(
            &mut rows,
            Health::Ok,
            format!("spooler visible con {} impresora(s)", printers.len()),
        ),
        Err(e) => check(&mut rows, Health::Fail, format!("spooler inaccesible: {e}")),
    };

    // Analysis toolchain.
    match mpa_analyzer::Ghostscript::discover() {
        Ok(gs) => check(
            &mut rows,
            Health::Ok,
            format!("ghostscript: {}", gs.program().display()),
        ),
        Err(_) => check(
            &mut rows,
            Health::Warn,
            "ghostscript ausente: sin análisis PDF/PS/XPS (solo imágenes)".into(),
        ),
    };
    match mpa_analyzer::find_pcl_interpreter() {
        Some(path) => check(
            &mut rows,
            Health::Ok,
            format!("ghostpcl: {}", path.display()),
        ),
        None => check(
            &mut rows,
            Health::Warn,
            "gpcl6 ausente: las reimpresiones PCL quedarán pendientes".into(),
        ),
    };

    // Pairing + server.
    let pairing = mpa_sync::PairingStore::load(None).ok();
    match &pairing {
        Some(p) => {
            check(
                &mut rows,
                Health::Ok,
                format!(
                    "enlazado con {} (workspace {})",
                    p.server_url, p.workspace_id
                ),
            );
            match mpa_sync::SyncClient::new(&p.server_url, Some(p.agent_key.clone())) {
                Ok(client) => match client.get_version() {
                    Ok(v) => check(
                        &mut rows,
                        Health::Ok,
                        format!("servidor responde (contrato {})", v.contract_version),
                    ),
                    Err(e) => check(
                        &mut rows,
                        Health::Fail,
                        format!("servidor no responde: {e}"),
                    ),
                },
                Err(e) => check(&mut rows, Health::Fail, format!("cliente sync: {e}")),
            }
        }
        None => check(
            &mut rows,
            Health::Warn,
            "sin enlace: ejecuta `mpa pair <código>` (solo funciona local)".into(),
        ),
    };

    // State + backlog.
    match load_core(config_override) {
        Ok((core, _)) => {
            let pending = core.pending_outbox();
            check(
                &mut rows,
                if pending == 0 {
                    Health::Ok
                } else {
                    Health::Warn
                },
                format!("borradores pendientes: {pending}"),
            );
            check(
                &mut rows,
                Health::Ok,
                format!("niveles cargados: {}", core.tiers().len()),
            );
        }
        Err(e) => check(
            &mut rows,
            Health::Fail,
            format!("estado local ilegible: {e}"),
        ),
    };

    // Spool cache writability (proxy for disk space where it matters).
    let probe = mpa_agent::spool_cache_dir().join(".writability-probe");
    match std::fs::create_dir_all(mpa_agent::spool_cache_dir())
        .and_then(|()| std::fs::write(&probe, b"ok"))
    {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            check(&mut rows, Health::Ok, "caché de spool escribible".into());
        }
        Err(e) => check(
            &mut rows,
            Health::Fail,
            format!("caché de spool no escribible: {e}"),
        ),
    };

    for (health, msg) in &rows {
        let tag = match health {
            Health::Ok => "ok  ",
            Health::Warn => "warn",
            Health::Fail => "FAIL",
        };
        println!("{tag}\t{msg}");
    }
    if rows.iter().any(|(h, _)| *h == Health::Fail) {
        return Err(AppError::Tiers("doctor encontró fallos".into()));
    }
    Ok(())
}

struct UpdateChannel {
    server_url: Option<String>,
    manifest_url: Option<String>,
    agent_key: Option<String>,
}

fn update_channel() -> Result<UpdateChannel, AppError> {
    let manifest_url = std::env::var("MPA_UPDATE_MANIFEST").ok();
    match PairingStore::load(None) {
        Ok(pairing) => Ok(UpdateChannel {
            server_url: Some(pairing.server_url),
            manifest_url,
            agent_key: Some(pairing.agent_key),
        }),
        Err(_) if manifest_url.is_some() => Ok(UpdateChannel {
            server_url: None,
            manifest_url,
            agent_key: None,
        }),
        Err(e) => Err(AppError::Pairing(e)),
    }
}

fn update_command(action: UpdateAction) -> Result<(), AppError> {
    let channel = update_channel()?;
    let status = mpa_update::check_for_update(
        channel.server_url.as_deref(),
        channel.manifest_url.as_deref(),
        channel.agent_key.as_deref(),
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|e| AppError::Tiers(format!("update: {e}")))?;
    match (action, status) {
        (_, mpa_update::UpdateStatus::NoChannel) => {
            println!("el servidor no publica releases todavía");
            Ok(())
        }
        (_, mpa_update::UpdateStatus::UpToDate { version }) => {
            println!("estás en la última versión ({version})");
            Ok(())
        }
        (UpdateAction::Check, mpa_update::UpdateStatus::Available { manifest }) => {
            println!(
                "nueva versión disponible: {} ({}){}",
                manifest.version,
                manifest.url,
                manifest
                    .notes
                    .as_ref()
                    .map(|n| format!(" — {n}"))
                    .unwrap_or_default()
            );
            Ok(())
        }
        (UpdateAction::Apply { yes }, mpa_update::UpdateStatus::Available { manifest }) => {
            if !yes {
                println!(
                    "va a descargar y verificar {} — confirma con --yes",
                    manifest.version
                );
                return Ok(());
            }
            // Manifest URLs are absolute; the client base is only auth context.
            let base = channel
                .manifest_url
                .or(channel.server_url)
                .unwrap_or_default();
            let client = mpa_sync::SyncClient::new(&base, channel.agent_key)?;
            let staged = mpa_update::fetch_verified(&client, &manifest)
                .map_err(|e| AppError::Tiers(format!("descarga/verificación: {e}")))?;
            println!("verificado: {}", staged.display());
            let name = staged.to_string_lossy().to_lowercase();
            if name.ends_with(".msi") || name.ends_with(".exe") {
                mpa_update::launch_installer_and_exit(&staged, &["/VERYSILENT".into()])
                    .map_err(|e| AppError::Tiers(format!("instalador: {e}")))?;
            } else {
                println!("artefacto no instalable; quedó en {}", staged.display());
            }
            Ok(())
        }
    }
}

/// Core hydrated from disk: persisted state first, config-file tiers as
/// fallback for first runs. The state file sits next to the config file.
fn load_core(config_override: Option<PathBuf>) -> Result<(AgentCore, PathBuf), AppError> {
    let mut core = AgentCore::new(
        default_backend().into(),
        Arc::new(MockTickets::demo()),
        Vec::new(),
    );
    let state_path = match config_override.clone() {
        Some(mut p) => {
            p.set_file_name("agent-state.json");
            p
        }
        None => default_state_path().map_err(AppError::Config)?,
    };
    let _ = load_into(&mut core, &state_path);
    if core.tiers().is_empty() {
        if let Ok(cfg) = mpa_agent::config_path(config_override) {
            core.set_tiers(load_tiers(&cfg));
        }
    }
    Ok((core, state_path))
}

fn save_core(core: &AgentCore, path: &PathBuf) -> Result<(), AppError> {
    save_to(core, path).map_err(AppError::Io)
}

fn pair_command(code: &str, pc_name: &str, server: &str) -> Result<(), AppError> {
    let pairing = SyncClient::pair(server, code, pc_name)?;
    let store = PairingStore::from_pairing(server, pairing);
    let path = store.save(None)?;
    println!(
        "paired\tworkspace {}\tagent {}\nkey saved to {}",
        store.workspace_id,
        store.agent_id,
        path.display()
    );
    println!("next: `mpa tiers sync` to download the tabulador");
    Ok(())
}

fn sync_command(config_override: Option<PathBuf>) -> Result<(), AppError> {
    let (core, state_path) = load_core(config_override)?;
    let core = Arc::new(Mutex::new(core));
    let pairing = PairingStore::load(None)?;
    let client = SyncClient::new(&pairing.server_url, Some(pairing.agent_key))?;
    let report = drain_outbox(&core, &client);
    save_core(&core.lock().expect("agent core"), &state_path)?;
    println!("{}", report.summary());
    for (key, err) in &report.push_failed {
        println!("  push failed\t{key}\t{err}");
    }
    for (key, err) in &report.materialize_failed {
        println!("  ticket pending\t{key}\t{err}");
    }
    for (key, err) in &report.reprint_failed {
        println!("  reprint pending\t{key}\t{err}");
    }
    Ok(())
}

/// `mpa push-test`: send a sample analysis through the real `/api/agent/jobs`
/// contract and read back what MyPaper stored. Prints nothing and touches no
/// spooler, so the webapp link can be checked on its own — with `--ticket` it
/// also materializes the lines into that ticket, which is the whole flow minus
/// the printer.
fn push_test_command(
    server: Option<String>,
    key: Option<String>,
    pages: u32,
    coverage: Option<String>,
    ticket: Option<String>,
    idempotency_key: Option<String>,
) -> Result<(), AppError> {
    let (server_url, agent_key) = match (server, key) {
        (Some(server), Some(key)) => (server, key),
        (server, key) => {
            let pairing = PairingStore::load(None)?;
            (
                server.unwrap_or(pairing.server_url),
                key.unwrap_or(pairing.agent_key),
            )
        }
    };
    let coverage = parse_coverage(coverage.as_deref())?;
    let idempotency_key = idempotency_key.unwrap_or_else(|| {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        format!("push-test-{stamp}")
    });
    let target = match &ticket {
        Some(id) => TargetSelection::Ticket(id.clone()),
        None => TargetSelection::Unassigned,
    };
    let entry = OutboxEntry {
        idempotency_key: idempotency_key.clone(),
        printer: "mpa-push-test".into(),
        job_id: 0,
        target,
        pages: (1..=pages.max(1))
            .map(|page_no| OutboxPage {
                page_no,
                coverage,
                tier_id: None,
                included: true,
            })
            .collect(),
        format: None,
        spool_path: None,
        reprint_pending: false,
        synced: false,
        remote_id: None,
        materialized: false,
    };
    let client = SyncClient::new(&server_url, Some(agent_key))?;
    println!(
        "enviando\t{server_url}\tpaginas {}\tcobertura C{} M{} Y{} K{}\tidempotency {idempotency_key}",
        entry.pages.len(),
        coverage.c,
        coverage.m,
        coverage.y,
        coverage.k
    );
    let job_id = client.post_job(&entry)?;
    println!("ok\tdraft {job_id}\taceptado por el servidor");
    if ticket.is_some() {
        let ack = client.materialize_job(&job_id)?;
        println!(
            "materializado\tticket {} ({})\tagregado {}\ttotal nuevo {}",
            ack.ticket_number, ack.order_id, ack.added_total, ack.new_total
        );
    }
    println!("lo que el servidor ve:");
    for job in client.get_recent_jobs()? {
        println!(
            "  {}\t{}\tticket {}\tpaginas {}\ttotal {}\t{}",
            job.id,
            job.status,
            job.target_order_id.as_deref().unwrap_or("-"),
            job.page_count,
            job.total,
            job.created_at.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

/// `"12,8,0,60"` → CMYK coverage. Default: a mostly-black text page.
fn parse_coverage(spec: Option<&str>) -> Result<Coverage, AppError> {
    let Some(spec) = spec else {
        return Ok(Coverage::new(6.0, 4.0, 2.0, 55.0));
    };
    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(AppError::Test(format!(
            "cobertura inválida '{spec}': usa C,M,Y,K (ej. 12,8,0,60)"
        )));
    }
    let mut channels = [0f32; 4];
    for (index, part) in parts.iter().enumerate() {
        channels[index] = part
            .parse()
            .map_err(|_| AppError::Test(format!("cobertura inválida '{part}'")))?;
    }
    Ok(Coverage::new(
        channels[0],
        channels[1],
        channels[2],
        channels[3],
    ))
}

fn tiers_command(
    action: Option<TierAction>,
    config_override: Option<PathBuf>,
) -> Result<(), AppError> {
    match action {
        None => {
            let (core, _) = load_core(config_override)?;
            if core.tiers().is_empty() {
                println!("no tiers (run `mpa tiers sync` when paired)");
            }
            for t in core.tiers() {
                println!(
                    "{}\t{}-{}\t{}",
                    t.id, t.min_coverage, t.max_coverage, t.active
                );
            }
            Ok(())
        }
        Some(TierAction::Sync) => {
            let pairing = PairingStore::load(None)?;
            let client = SyncClient::new(&pairing.server_url, Some(pairing.agent_key))?;
            let remote = client.get_tiers()?;
            let tiers: Vec<Tier> = remote.iter().map(|t| t.to_domain()).collect();
            // Persist into the state file (what the UI/runtime read)…
            let (mut core, state_path) = load_core(config_override.clone())?;
            core.set_tiers(tiers.clone());
            save_core(&core, &state_path)?;
            // …and mirror into config [[tiers]] as the offline fallback.
            if let Ok(cfg) = mpa_agent::config_path(config_override) {
                let _ = mpa_agent::save_tiers(&tiers, &cfg);
            }
            println!("synced {} tiers:", remote.len());
            for t in &remote {
                let price = t
                    .price
                    .map(|p| format!("${p:.2}"))
                    .unwrap_or_else(|| "—".into());
                println!(
                    "{}\t{}-{}\t{}\t{}",
                    t.id,
                    t.min_coverage,
                    t.max_coverage,
                    t.product_name.as_deref().unwrap_or("?"),
                    price
                );
            }
            Ok(())
        }
    }
}

fn outbox_command(config_override: Option<PathBuf>) -> Result<(), AppError> {
    let (core, _) = load_core(config_override)?;
    if core.outbox().is_empty() {
        println!("outbox empty");
        return Ok(());
    }
    for (i, e) in core.outbox().iter().enumerate() {
        let target = match &e.target {
            mpa_agent::TargetSelection::Ticket(id) => format!("ticket {id}"),
            mpa_agent::TargetSelection::NewDraft => "new draft".into(),
            mpa_agent::TargetSelection::Unassigned => "unassigned".into(),
        };
        println!(
            "[{i}]\t{}\t{}\t{} pages\t{target}\tsynced={}\treprint_pending={}\tremote={}",
            e.idempotency_key,
            e.printer,
            e.pages.len(),
            e.synced,
            e.reprint_pending,
            e.remote_id.as_deref().unwrap_or("—")
        );
    }
    Ok(())
}

/// Logon autostart for the UI, as a Task Scheduler entry with highest
/// privileges.
///
/// `mpa-ui.exe` requires administrator to read the held job's spool file, so a
/// plain `HKCU\...\Run` value would pop a UAC prompt at every logon (and skip
/// the agent entirely if the operator declines). The task runs elevated with
/// no prompt. Creating it needs an elevated shell — `mpa autostart on` says so
/// instead of failing silently.
#[cfg(windows)]
fn autostart_command(action: AutostartAction) -> Result<(), AppError> {
    use std::process::Command;
    const TASK: &str = "MyPaperPrintAgent";
    /// Value written by older builds; removed so it cannot double-start the UI.
    const LEGACY_RUN_VALUE: &str = "MyPaperPrintAgent";

    let mut ui = std::env::current_exe()?;
    ui.set_file_name("mpa-ui.exe");
    let ui = ui.to_string_lossy().into_owned();
    // schtasks parses its own command line: a path with spaces must be quoted
    // inside the /TR value.
    let quoted = format!("\"{ui}\"");

    match action {
        AutostartAction::Status => {
            let enabled = Command::new("schtasks")
                .args(["/Query", "/TN", TASK])
                .output()
                .map(|out| out.status.success())
                .unwrap_or(false);
            if enabled {
                println!("autostart: on (tarea {TASK} -> {ui})");
            } else {
                println!("autostart: off");
            }
        }
        AutostartAction::On => {
            remove_legacy_run_value(LEGACY_RUN_VALUE);
            let status = Command::new("schtasks")
                .args([
                    "/Create", "/TN", TASK, "/SC", "ONLOGON", "/RL", "HIGHEST", "/TR", &quoted,
                    "/F",
                ])
                .status()?;
            if !status.success() {
                return Err(AppError::Autostart(format!(
                    "no se pudo crear la tarea {TASK}: ejecuta `mpa autostart on` \
                     como administrador (mpa-ui necesita elevación para leer el spool)"
                )));
            }
            println!("autostart: on (tarea {TASK}, privilegios altos, -> {ui})");
        }
        AutostartAction::Off => {
            let _ = Command::new("schtasks")
                .args(["/Delete", "/TN", TASK, "/F"])
                .status();
            remove_legacy_run_value(LEGACY_RUN_VALUE);
            println!("autostart: off");
        }
    }
    Ok(())
}

/// Drop the pre-task `HKCU\...\Run` entry written by earlier builds.
#[cfg(windows)]
fn remove_legacy_run_value(name: &str) {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
    use winreg::RegKey;
    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    if let Ok(run) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN, KEY_WRITE) {
        let _ = run.delete_value(name);
    }
}

#[cfg(not(windows))]
fn autostart_command(action: AutostartAction) -> Result<(), AppError> {
    let _ = action;
    println!("autostart is only supported on Windows");
    Ok(())
}

fn parse_tiers(spec: &str) -> Result<Vec<Tier>, AppError> {
    let mut tiers = Vec::new();
    for part in spec.split(',') {
        let fields: Vec<&str> = part.split(':').collect();
        if fields.len() != 3 {
            return Err(AppError::Tiers(format!("bad tier: {part}")));
        }
        tiers.push(Tier {
            id: fields[0].to_string(),
            min_coverage: fields[1]
                .parse()
                .map_err(|_| AppError::Tiers(format!("bad min: {part}")))?,
            max_coverage: fields[2]
                .parse()
                .map_err(|_| AppError::Tiers(format!("bad max: {part}")))?,
            active: true,
        });
    }
    Ok(tiers)
}

fn analyze_command(
    file: PathBuf,
    tiers: Option<String>,
    thumbs_dir: Option<PathBuf>,
) -> Result<(), AppError> {
    // Images analyze without Ghostscript; PDFs need it.
    let gs = Ghostscript::discover().ok();
    let analysis = analyze_file(&file, gs.as_ref()).map_err(AppError::Analyze)?;
    let tiers = tiers
        .map(|s| parse_tiers(&s))
        .transpose()?
        .unwrap_or_default();

    if let Some(dir) = thumbs_dir {
        std::fs::create_dir_all(&dir)?;
        for page in &analysis.pages {
            if !page.thumbnail_png.is_empty() {
                std::fs::write(
                    dir.join(format!("thumb-{}.png", page.page_no)),
                    &page.thumbnail_png,
                )?;
            }
        }
        println!("thumbnails: {}", dir.display());
    }

    println!("format: {:?}", analysis.format);
    for page in &analysis.pages {
        let c = &page.coverage;
        let color = c.color_coverage();
        let tier = if tiers.is_empty() {
            "-".to_string()
        } else {
            classify_coverage(color, &tiers)
                .map(|t| t.id.clone())
                .unwrap_or_else(|| "NONE".into())
        };
        println!(
            "page {}\tC={:.2} M={:.2} Y={:.2} K={:.2}\tcolor={:.2}%\ttier={tier}",
            page.page_no, c.c, c.m, c.y, c.k, color
        );
    }
    Ok(())
}

/// One thread per watched printer: wait for arrivals, pause immediately,
/// track the domain state. Ctrl-C stops the process (Phase 2 adds a tray
/// service with graceful shutdown).
fn watch_loop(
    backend: Box<dyn SpoolBackend>,
    config_arg: Option<PathBuf>,
    quantum: Duration,
) -> Result<(), AppError> {
    let (policy, _) = load_policy(config_arg)?;
    let backend: Arc<dyn SpoolBackend> = backend.into();
    let printers: Vec<String> = backend
        .list_printers()?
        .into_iter()
        .map(|p| p.name)
        .filter(|name| policy.watches(name))
        .collect();

    if printers.is_empty() {
        println!("no watched printers (see `mpa printers`)");
        return Ok(());
    }
    println!("watching: {}", printers.join(", "));

    let mut handles = Vec::new();
    for printer in printers {
        let backend = Arc::clone(&backend);
        handles.push(std::thread::spawn(move || {
            watch_printer(backend, printer, quantum)
        }));
    }
    for h in handles {
        match h.join() {
            Ok(Err(e)) => eprintln!("watcher error: {e}"),
            Err(_) => eprintln!("watcher thread panicked"),
            Ok(Ok(())) => {}
        }
    }
    Ok(())
}

fn watch_printer(
    backend: Arc<dyn SpoolBackend>,
    printer: String,
    quantum: Duration,
) -> Result<(), SpoolError> {
    // Jobs already paused by this watcher: never re-pause, never re-report.
    let mut seen: HashSet<u32> = HashSet::new();
    let mut states: HashMap<u32, JobState> = HashMap::new();

    loop {
        match backend.wait_for_new_job(&printer, quantum)? {
            None => continue,
            Some(job_id) => {
                if !seen.insert(job_id) {
                    continue;
                }
                states.insert(job_id, JobState::Detected);
                match backend.pause_job(&printer, job_id) {
                    Ok(()) => {
                        let state = transition(JobState::Detected, JobEvent::PausedByAgent)
                            .expect("detected + pause is legal");
                        states.insert(job_id, state);
                        let doc = backend
                            .list_jobs(&printer)?
                            .into_iter()
                            .find(|j| j.id == job_id)
                            .map(|j| j.document)
                            .unwrap_or_default();
                        println!("paused\t{printer}\t{job_id}\t{doc}");
                    }
                    Err(SpoolError::JobNotFound(_, _)) => {
                        // Tiny job finished before the pause landed: nothing to hold.
                        println!("missed\t{printer}\t{job_id}\t(finished before pause)");
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error(transparent)]
    Spool(#[from] SpoolError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("analyze: {0}")]
    Analyze(#[from] mpa_analyzer::AnalyzeError),
    #[error("capture: {0}")]
    Capture(#[from] mpa_analyzer::CaptureError),
    #[error("tiers: {0}")]
    Tiers(String),
    #[error("prueba: {0}")]
    Test(String),
    #[error("sync: {0}")]
    Sync(#[from] mpa_sync::SyncError),
    #[error("pairing: {0}")]
    Pairing(#[from] mpa_sync::PairingError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Only built on Windows: `autostart` talks to the Task Scheduler.
    #[cfg(windows)]
    #[error("{0}")]
    Autostart(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_spec_parses_cmyk_and_rejects_junk() {
        let default = parse_coverage(None).expect("default coverage");
        assert_eq!(default.color_coverage(), 6.0);

        let parsed = parse_coverage(Some("12,8,0,60")).expect("explicit coverage");
        assert_eq!(
            (parsed.c, parsed.m, parsed.y, parsed.k),
            (12.0, 8.0, 0.0, 60.0)
        );
        // Billable coverage is the worst color channel, never an average.
        assert_eq!(parsed.color_coverage(), 12.0);

        let spaced = parse_coverage(Some(" 12 , 8 , 0 , 60 ")).expect("trimmed coverage");
        assert_eq!(spaced.c, 12.0);

        assert!(parse_coverage(Some("12,8,0")).is_err());
        assert!(parse_coverage(Some("12,ocho,0,60")).is_err());
    }
}
