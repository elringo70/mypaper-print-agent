use clap::{Parser, Subcommand, ValueEnum};
use mpa_agent::{
    config_path, default_state_path, load_into, load_metrics, load_policy, load_presets,
    load_roles, load_tiers, save_metrics, save_policy, save_presets, save_roles, save_tiers,
    save_to, AgentCore, ConfigError, MockTickets, OutboxEntry, OutboxPage, TargetSelection,
};
use mpa_analyzer::{analyze_file, capture_spool_file};
use mpa_domain::{
    classify_coverage, has_color_ink, metric_coverage, transition, Coverage, JobEvent, JobState,
    PolicyMode, PrintMode, Tier,
};
use mpa_spool::{default_backend, SpoolBackend, SpoolError};
use mpa_sync::{drain_outbox, PairingStore, SyncClient};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
// `version` matters beyond courtesy: the update channel and the support
// questions ("which build are you running?") both start with `mpa --version`.
#[command(
    name = "mpa",
    version,
    about = "MyPaper print agent — Phase 1: spool watcher"
)]
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
    /// Control a single job: pause | resume | cancel | settings | set-options.
    Job {
        #[command(subcommand)]
        action: JobAction,
    },
    /// Show or edit the watched/ignored printer lists and the mono/color roles.
    Printers {
        #[command(subcommand)]
        action: Option<PrinterAction>,
        /// Printer to query: lists the paper sizes, media types (paper
        /// thickness), trays and duplex/color support its DRIVER reports.
        #[arg(long)]
        caps: Option<String>,
    },
    /// Print presets: what the agent may change on a job it re-renders
    /// (paper size, duplex). The media type/thickness stays in the Windows
    /// driver's own preferences.
    Presets {
        #[command(subcommand)]
        action: Option<PresetAction>,
    },
    /// Analyze a file (image/PDF/PS) into per-page CMYK coverage.
    Analyze {
        file: PathBuf,
        /// Tiers as id:min:max[:BW|COLOR],... (e.g. "t6:0:50:COLOR,t10:50.01:90:COLOR").
        /// Mode defaults to BW.
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
    /// Offline entitlement: refresh from the server, or inspect the cached one.
    Entitlement {
        /// Verify and print the cached token without contacting the server.
        #[arg(long)]
        status: bool,
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

// Which job a `mpa job` subcommand acts on. A flattened struct (not a plain
// doc comment) so the usage stays `mpa job pause <printer> <job_id>`: clap
// parses parent-level positionals BEFORE the subcommand name, which would have
// inverted them. Keep this as a `//` comment: a `///` one becomes the help text
// of whichever subcommand clap sees first.
#[derive(clap::Args)]
struct JobRef {
    /// Printer as it appears in the Printers control panel.
    printer: String,
    /// Job id from `mpa list-printers` / the spooler queue.
    job_id: u32,
}

#[derive(Subcommand)]
enum JobAction {
    /// Hold the job (the spooler stops it before it prints).
    Pause(JobRef),
    /// Release a held job so it prints.
    Resume(JobRef),
    /// Drop the job without printing it.
    Cancel(JobRef),
    /// Show the print settings the held job carries (its Windows DEVMODE).
    Settings(JobRef),
    /// Change them in place: the spooler applies them when the job resumes.
    SetOptions {
        #[command(flatten)]
        target: JobRef,
        /// Paper size name as the driver spells it (`mpa printers caps` lists them).
        #[arg(long)]
        paper: Option<String>,
        /// Media type / paper thickness name from the driver's own list.
        #[arg(long)]
        media_type: Option<String>,
        /// Two-sided printing.
        #[arg(long, conflicts_with = "simplex")]
        duplex: bool,
        /// Single-sided printing.
        #[arg(long)]
        simplex: bool,
        /// Color printing.
        #[arg(long, conflicts_with = "mono")]
        color: bool,
        /// Monochrome printing.
        #[arg(long)]
        mono: bool,
    },
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
    /// Assign the mono and/or color role to an installed printer.
    Set {
        /// Printer that prints black and white (the default role).
        #[arg(long)]
        bw: Option<String>,
        /// Printer that prints in color.
        #[arg(long)]
        color: Option<String>,
    },
    /// Drop a role assignment (--bw / --color; no flags clears both).
    Unset {
        #[arg(long)]
        bw: bool,
        #[arg(long)]
        color: bool,
    },
}

#[derive(Subcommand)]
enum PresetAction {
    /// Create or replace a preset by name.
    Add {
        #[arg(long)]
        name: String,
        /// Ghostscript paper size (a4, letter, legal, ledger…).
        #[arg(long)]
        paper: Option<String>,
        /// Explicit page width in points (needs --height-points too).
        #[arg(long)]
        width_points: Option<u32>,
        /// Explicit page height in points (needs --width-points too).
        #[arg(long)]
        height_points: Option<u32>,
        /// Two-sided printing (the driver must support it).
        #[arg(long)]
        duplex: bool,
        /// Bind the verso on the short edge instead of the long edge.
        #[arg(long)]
        tumble: bool,
        /// Media type / paper thickness as the driver spells it (`mpa printers
        /// --caps <printer>` lists the driver's own names). Applied through the
        /// job's DEVMODE, so it also works for jobs mswinpr2 cannot re-render.
        #[arg(long)]
        media_type: Option<String>,
    },
    /// Delete a preset by name.
    Remove {
        #[arg(long)]
        name: String,
    },
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
        Commands::Job { action } => {
            let (printer, job_id) = match &action {
                JobAction::Pause(job)
                | JobAction::Resume(job)
                | JobAction::Cancel(job)
                | JobAction::Settings(job) => (job.printer.clone(), job.job_id),
                JobAction::SetOptions { target, .. } => (target.printer.clone(), target.job_id),
            };
            match action {
                JobAction::Pause(_) => backend.pause_job(&printer, job_id)?,
                JobAction::Resume(_) => backend.resume_job(&printer, job_id)?,
                JobAction::Cancel(_) => backend.cancel_job(&printer, job_id)?,
                JobAction::Settings(_) => {
                    let settings = backend.job_settings(&printer, job_id)?;
                    print_job_settings(&printer, job_id, &settings);
                    return Ok(());
                }
                JobAction::SetOptions {
                    paper,
                    media_type,
                    duplex,
                    simplex,
                    color,
                    mono,
                    ..
                } => {
                    let requested = mpa_spool::JobSettings {
                        paper,
                        media_type,
                        duplex: if duplex {
                            Some(true)
                        } else if simplex {
                            Some(false)
                        } else {
                            None
                        },
                        color: if color {
                            Some(true)
                        } else if mono {
                            Some(false)
                        } else {
                            None
                        },
                    };
                    if requested == mpa_spool::JobSettings::default() {
                        eprintln!("nothing to change: pass --paper/--media-type/--duplex/--simplex/--color/--mono");
                        std::process::exit(2);
                    }
                    println!("requested:");
                    print_job_settings(&printer, job_id, &requested);
                    // The spooler's answer after the change is what matters, so
                    // print the read-back instead of trusting the request.
                    let applied = backend.apply_job_settings(&printer, job_id, &requested)?;
                    println!("after the change (read back from the spooler):");
                    print_job_settings(&printer, job_id, &applied);
                    if !applied.satisfies(&requested) {
                        // Accepted by the API but not in effect: the job keeps
                        // its original DEVMODE, so the agent must reprint.
                        eprintln!(
                            "aviso: el spooler aceptó el cambio pero conserva los ajustes \
                             originales; el agente aplicará el preset reimprimiendo"
                        );
                    }
                    return Ok(());
                }
            }
            let verb = match action {
                JobAction::Pause(_) => "paused",
                JobAction::Resume(_) => "resumed",
                JobAction::Cancel(_) => "cancelled",
                JobAction::Settings(_) | JobAction::SetOptions { .. } => unreachable!(),
            };
            println!("job {job_id} on {printer} {verb}");
        }
        Commands::Printers { action, caps } => {
            if let Some(printer) = caps {
                let caps = backend.printer_caps(&printer)?;
                println!("printer: {printer}");
                println!(
                    "duplex: {} ({})",
                    if caps.duplex_modes > 0 { "sí" } else { "no" },
                    caps.duplex_modes
                );
                println!("color: {}", if caps.color { "sí" } else { "no" });
                println!("papel ({}): {}", caps.papers.len(), caps.papers.join(", "));
                println!(
                    "tipo/grosor de papel ({}): {}",
                    caps.media_types.len(),
                    caps.media_types.join(", ")
                );
                return Ok(());
            }
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
                    print_roles(&load_roles(&path));
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
                Some(PrinterAction::Set { bw, color }) => {
                    let mut roles = load_roles(&path);
                    if let Some(name) = bw {
                        roles.bw = Some(name);
                    }
                    if let Some(name) = color {
                        roles.color = Some(name);
                    }
                    save_roles(&roles, &path)?;
                    print_roles(&roles);
                    println!("saved to {}", path.display());
                }
                Some(PrinterAction::Unset { bw, color }) => {
                    let mut roles = load_roles(&path);
                    // No flags at all = clear both roles.
                    let both = !bw && !color;
                    if bw || both {
                        roles.bw = None;
                    }
                    if color || both {
                        roles.color = None;
                    }
                    save_roles(&roles, &path)?;
                    print_roles(&roles);
                    println!("saved to {}", path.display());
                }
            }
        }
        Commands::Presets { action } => {
            let path = config_path(cli.config.clone())?;
            let mut presets = load_presets(&path);
            match action {
                None => {
                    if presets.is_empty() {
                        println!("no presets configured");
                    }
                    for preset in &presets {
                        println!("{}: {}", preset.name, preset.options.label());
                        for arg in preset.options.gs_args() {
                            println!("  {arg}");
                        }
                    }
                    println!("config: {}", path.display());
                }
                Some(PresetAction::Add {
                    name,
                    paper,
                    width_points,
                    height_points,
                    duplex,
                    tumble,
                    media_type,
                }) => {
                    // A width without a height cannot pin a page; keep the name
                    // as the source of truth instead of storing half a size.
                    let geometry = match (width_points, height_points) {
                        (Some(w), Some(h)) => (Some(w), Some(h)),
                        _ => (None, None),
                    };
                    if (width_points.is_some() || height_points.is_some()) && geometry.0.is_none() {
                        println!("--width-points and --height-points go together; ignoring them");
                    }
                    let options = mpa_domain::PrintOptions {
                        paper,
                        width_points: geometry.0,
                        height_points: geometry.1,
                        duplex,
                        tumble,
                        media_type,
                    };
                    if options.is_empty() {
                        eprintln!("a preset with no options would print nothing differently");
                        std::process::exit(2);
                    }
                    presets.retain(|p| p.name != name);
                    presets.push(mpa_domain::PrintPreset {
                        name: name.clone(),
                        options,
                    });
                    save_presets(&presets, &path)?;
                    println!("preset \"{name}\" saved to {}", path.display());
                }
                Some(PresetAction::Remove { name }) => {
                    let before = presets.len();
                    presets.retain(|p| p.name != name);
                    if presets.len() == before {
                        eprintln!("no preset named \"{name}\"");
                        std::process::exit(2);
                    }
                    save_presets(&presets, &path)?;
                    println!("preset \"{name}\" removed from {}", path.display());
                }
            }
        }
        Commands::Watch { quantum_ms } => {
            apply_entitlement();
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
        Commands::Entitlement { status } => {
            entitlement_command(status)?;
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

    // PDF engine: pdfium must be next to the binary or on the search path.
    if mpa_analyzer::pdf::available() {
        check(&mut rows, Health::Ok, "pdfium: motor PDF disponible".into());
    } else {
        check(
            &mut rows,
            Health::Fail,
            "pdfium ausente: sin análisis de PDF (reinstala o define MPA_PDFIUM)".into(),
        );
    }
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
            core.set_metrics(load_metrics(&cfg));
            // Roles and presets describe this PC's printers, not runtime state.
            core.set_roles(load_roles(&cfg));
            core.set_presets(load_presets(&cfg));
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
                area: None,
            })
            .collect(),
        format: None,
        spool_path: None,
        mode: mpa_domain::PrintMode::Bw,
        deliver_printer: None,
        options: None,
        applied_settings: None,
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

/// Print a job's print settings (its DEVMODE) in a stable, greppable shape.
fn print_job_settings(printer: &str, job_id: u32, settings: &mpa_spool::JobSettings) {
    let dash = "—".to_string();
    println!("job {job_id} on {printer}");
    println!(
        "  paper:      {}",
        settings.paper.as_deref().unwrap_or(&dash)
    );
    println!(
        "  media type: {}",
        settings.media_type.as_deref().unwrap_or(&dash)
    );
    println!(
        "  duplex:     {}",
        match settings.duplex {
            Some(true) => "doble cara",
            Some(false) => "una cara",
            None => "—",
        }
    );
    println!(
        "  color:      {}",
        match settings.color {
            Some(true) => "color",
            Some(false) => "monocromo",
            None => "—",
        }
    );
}

/// Installed printers on this PC, flagging the mono / color roles. The physical
/// printer must already be set up for that ink — the agent only routes jobs.
fn print_roles(roles: &mpa_domain::PrinterRoles) {
    let installed = mpa_spool::default_backend()
        .list_printers()
        .unwrap_or_default();
    let names: Vec<&str> = installed.iter().map(|p| p.name.as_str()).collect();
    println!(
        "installed ({}): {}",
        names.len(),
        if names.is_empty() {
            "—".to_string()
        } else {
            names.join(", ")
        }
    );
    for (label, value) in [("bw", &roles.bw), ("color", &roles.color)] {
        match value {
            Some(name) if names.contains(&name.as_str()) => println!("{label}: {name}"),
            Some(name) => println!("{label}: {name}  (no instalada en esta PC)"),
            None => println!("{label}: —"),
        }
    }
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
            let sync = client.get_tiers_sync()?;
            let tiers: Vec<Tier> = sync.tiers.clone();
            // Persist into the state file (what the UI/runtime read)…
            let (mut core, state_path) = load_core(config_override.clone())?;
            core.set_tiers(tiers.clone());
            core.set_metrics(sync.metrics);
            // Fase 6: the rules the server prices with, so the local labels agree.
            core.set_pricing(sync.pricing);
            // …and the prices themselves, so the local label can pick the
            // dearest rule set instead of always preferring color.
            core.set_tier_prices(sync.prices.clone());
            save_core(&core, &state_path)?;
            // …and mirror into config [[tiers]] + [metrics] as the offline fallback.
            if let Ok(cfg) = mpa_agent::config_path(config_override) {
                let _ = save_tiers(&tiers, &cfg);
                let _ = save_metrics(&sync.metrics, &cfg);
            }
            println!(
                "synced {} tiers (BW: {:?}, COLOR: {:?}):",
                remote.len(),
                sync.metrics.bw,
                sync.metrics.color
            );
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

/// Path of the cached entitlement token, next to `agent.json`.
fn entitlement_path() -> Result<PathBuf, AppError> {
    let config = mpa_agent::config_path(None).map_err(AppError::Config)?;
    Ok(config.with_file_name("entitlement.jwt"))
}

/// `mpa entitlement`: fetch a fresh token, or verify the cached one.
///
/// The token is the only thing that lets the agent work without a network. It
/// carries an absolute expiry issued by the server, and this command never
/// extends it — `--status` only reads.
fn entitlement_command(status_only: bool) -> Result<(), AppError> {
    let token_path = entitlement_path()?;
    let guard_path = token_path.with_file_name("entitlement-guard.json");

    if !status_only {
        let pairing = mpa_sync::PairingStore::load(None)?;
        let client =
            mpa_sync::SyncClient::new(&pairing.server_url, Some(pairing.agent_key.clone()))?;
        let fresh = client.get_entitlement()?;
        if let Some(parent) = token_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&token_path, fresh.token.as_bytes())?;
        println!(
            "token renovado (vence {})",
            fresh.expires_at.as_deref().unwrap_or("según el token")
        );
    }

    let token = std::fs::read_to_string(&token_path).map_err(|_| {
        AppError::Tiers(
            "sin token local: ejecuta `mpa entitlement` con el servidor disponible".into(),
        )
    })?;

    // High-water mark: a clock that went backwards invalidates everything.
    let mut guard = mpa_sync::ClockGuard::load(&guard_path);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    match guard.authorize(token.trim(), now) {
        Ok(payload) => {
            guard.save(&guard_path);
            println!(
                "ok: plan {} · vence {}",
                payload.plan.as_deref().unwrap_or("-"),
                payload.exp
            );
            println!("   features: {}", payload.features.join(", "));
            println!("   horas restantes: {}", (payload.exp - now).max(0) / 3600);
        }
        Err(e) => println!("bloqueado: {e}"),
    }
    Ok(())
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
        if fields.len() != 3 && fields.len() != 4 {
            return Err(AppError::Tiers(format!("bad tier: {part}")));
        }
        // `id:min:max[:BW|COLOR]`; BW (the mono rule set) is the default.
        let mode = match fields.get(3) {
            None => PrintMode::Bw,
            Some(raw) => match raw.to_ascii_uppercase().as_str() {
                "BW" => PrintMode::Bw,
                "COLOR" => PrintMode::Color,
                other => return Err(AppError::Tiers(format!("bad mode: {other}"))),
            },
        };
        tiers.push(Tier {
            id: fields[0].to_string(),
            mode,
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
    let analysis = analyze_file(&file).map_err(AppError::Analyze)?;
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
    // Metrics from config.toml: the same rule sets MyPaper prices with.
    let metrics = mpa_agent::config_path(None)
        .map(|cfg| load_metrics(&cfg))
        .unwrap_or_default();
    println!("metrics: bw={:?} color={:?}", metrics.bw, metrics.color);
    for page in &analysis.pages {
        let c = &page.coverage;
        let color = c.color_coverage();
        // Both rule sets, so the operator can see which one would bill the page.
        let tier = |mode| {
            if tiers.is_empty() {
                "-".to_string()
            } else if mode == PrintMode::Color && !has_color_ink(c) {
                "NONE (sin tinta de color)".to_string()
            } else {
                classify_coverage(metric_coverage(metrics.for_mode(mode), c), &tiers, mode)
                    .map(|t| t.id.clone())
                    .unwrap_or_else(|| "NONE".into())
            }
        };
        println!(
            "page {}\tC={:.2} M={:.2} Y={:.2} K={:.2}\tcolor={:.2}%\tbw={}\tcolor_tier={}",
            page.page_no,
            c.c,
            c.m,
            c.y,
            c.k,
            color,
            tier(PrintMode::Bw),
            tier(PrintMode::Color)
        );
    }
    Ok(())
}

/// Evaluate the offline entitlement once, before any job can be paused.
///
/// The decision itself lives in `mpa_sync::entitlement::apply_gate` so the UI
/// runtime reaches exactly the same conclusion.
fn apply_entitlement() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let dir = match mpa_agent::config_path(None) {
        Ok(path) => path.parent().map(|p| p.to_path_buf()).unwrap_or_default(),
        Err(e) => {
            mpa_agent::gate::set(Some(format!("config no disponible: {e}")));
            eprintln!("bloqueado: config no disponible ({e})");
            return;
        }
    };
    match mpa_sync::entitlement::apply_gate(&dir, now) {
        mpa_sync::GateOutcome::Allowed(payload) => println!(
            "plan {} · vence {} ({} h)",
            payload.plan.as_deref().unwrap_or("-"),
            payload.exp,
            (payload.exp - now).max(0) / 3600
        ),
        mpa_sync::GateOutcome::NoToken => eprintln!(
            "aviso: sin token de entitlement; se opera sin control de plan. \
             Ejecuta `mpa entitlement` con el servidor disponible."
        ),
        mpa_sync::GateOutcome::Blocked(reason) => eprintln!("bloqueado: {reason}"),
    }
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
                // Blocked: the job prints unmanaged. Pausing it would hold a
                // customer's print hostage to a subscription.
                if let Some(reason) = mpa_agent::gate::reason() {
                    eprintln!("bloqueado ({reason}): {printer} job {job_id} pasa sin gestionar");
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
