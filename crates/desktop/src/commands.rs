//! Commands the window can call.
//!
//! Every command takes the core lock briefly and never does IO inside it: the
//! spooler and the printer are slow, and the watcher threads need that lock.
//! Long work (reprint, sync) already lives on its own thread.
//!
//! Commands are declared `#[tauri::command(async)]` on purpose: a plain command
//! runs inline in the WebView's IPC handler, i.e. on the UI thread, so a 15 s
//! HTTP round trip inside one froze the window. Async commands run on the
//! runtime's thread pool and the window keeps painting while they work.

use crate::dto::{AppSnapshot, CheckDto, JobDto, PageDto, TicketDto};
use crate::AppState;
use base64::Engine;
use mpa_agent::AgentCore;
use std::sync::Arc;
use tauri::State;

fn lock(
    core: &Arc<std::sync::Mutex<AgentCore>>,
) -> Result<std::sync::MutexGuard<'_, AgentCore>, String> {
    core.lock()
        .map_err(|_| "estado interno bloqueado".to_string())
}

fn data_url(png: &[u8]) -> String {
    if png.is_empty() {
        String::new()
    } else {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        )
    }
}

/// Core change counter. The window polls this instead of the full snapshot:
/// one number per second, and the snapshot is only fetched when it moved.
#[tauri::command(async)]
pub fn version(state: State<'_, AppState>) -> Result<u64, String> {
    let core = lock(&state.core)?;
    Ok(core.version())
}

/// Full snapshot, one per core change. Page previews are NOT included: a
/// 100-page job would push megabytes of base64 through IPC on every read.
#[tauri::command(async)]
pub fn snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, String> {
    let core = lock(&state.core)?;
    let jobs = core
        .snapshot()
        .into_iter()
        .map(|job| JobDto {
            printer: job.printer.clone(),
            job_id: job.job_id,
            document: job.document.clone(),
            state: format!("{:?}", job.state),
            analysis_error: job.analysis_error.clone(),
            warnings: job.warnings.clone(),
            format: job.format.map(|f| format!("{f:?}")),
            target: match &job.target {
                mpa_agent::TargetSelection::Unassigned => "unassigned".into(),
                mpa_agent::TargetSelection::NewDraft => "newDraft".into(),
                mpa_agent::TargetSelection::Ticket(_) => "ticket".into(),
            },
            target_ticket_id: match &job.target {
                mpa_agent::TargetSelection::Ticket(id) => Some(id.clone()),
                _ => None,
            },
            preset: job.preset.clone(),
            print_mode: format!("{:?}", job.print_mode),
            price: None,
            pages: job
                .pages
                .iter()
                .map(|page| PageDto {
                    page_no: page.page_no,
                    c: page.coverage.c,
                    m: page.coverage.m,
                    y: page.coverage.y,
                    k: page.coverage.k,
                    area_pct: page.area.map(|a| a.area_pct),
                    class: page
                        .area
                        .and_then(|a| a.class)
                        .map(|c| c.as_str().to_string()),
                    tier_id: page.tier_id.clone(),
                    included: page.included,
                })
                .collect(),
        })
        .collect();
    let tickets = core
        .cached_tickets()
        .iter()
        .map(|t| TicketDto {
            id: t.id.clone(),
            number: t.ticket_number,
            cashier: t.cashier.clone(),
            customer: t.customer.clone(),
            total: t.total,
        })
        .collect();
    let presets = core.presets().iter().map(|p| p.name.clone()).collect();

    Ok(AppSnapshot {
        version: core.version(),
        mode: format!("{:?}", core.mode()),
        blocked: mpa_agent::gate::reason(),
        jobs,
        tickets,
        presets,
    })
}

/// One page preview, fetched only when the operator looks at it.
#[tauri::command(async)]
pub fn page_thumbnail(
    state: State<'_, AppState>,
    printer: String,
    job_id: u32,
    page_no: u32,
) -> Result<String, String> {
    let core = lock(&state.core)?;
    let png = core
        .page(&printer, job_id, page_no)
        .map(|p| p.thumbnail_png.clone())
        .unwrap_or_default();
    Ok(data_url(&png))
}

/// Include or exclude one page from the billed job.
#[tauri::command(async)]
pub fn toggle_page(
    state: State<'_, AppState>,
    printer: String,
    job_id: u32,
    page_no: u32,
    included: bool,
) -> Result<(), String> {
    let mut core = lock(&state.core)?;
    // The core flips the flag; only ask when the UI disagrees with it, so a
    // duplicated click cannot undo itself.
    let current = core.page(&printer, job_id, page_no).map(|p| p.included);
    if current == Some(included) {
        return Ok(());
    }
    core.toggle_page(&printer, job_id, page_no)
        .map_err(|e| e.to_string())
}

/// Release the job and hand the selected pages to the outbox for MyPaper.
#[tauri::command(async)]
pub fn complete_job(
    state: State<'_, AppState>,
    printer: String,
    job_id: u32,
) -> Result<(), String> {
    let mut core = lock(&state.core)?;
    // A captured job has no held spooler job to release: the operator's
    // authorisation is what queues the print.
    if printer == mpa_agent::CAPTURE_PRINTER {
        let (mode, target) = core
            .job(&printer, job_id)
            .map(|j| (j.print_mode, j.target.clone()))
            .unwrap_or((
                mpa_domain::PrintMode::Bw,
                mpa_agent::TargetSelection::Unassigned,
            ));
        return core
            .authorize_captured(&printer, job_id, mode, target)
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    core.complete_job(&printer, job_id)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Cancel the job in the spooler; nothing is billed.
#[tauri::command(async)]
pub fn cancel_job(state: State<'_, AppState>, printer: String, job_id: u32) -> Result<(), String> {
    let mut core = lock(&state.core)?;
    core.cancel_job(&printer, job_id).map_err(|e| e.to_string())
}

/// What both rule sets would cost for this job's included pages.
///
/// The price always comes from the server: a local guess would drift from what
/// the ticket charges, and the operator reads this number to decide.
#[tauri::command(async)]
pub fn quote(
    state: State<'_, AppState>,
    printer: String,
    job_id: u32,
) -> Result<crate::dto::QuoteDto, String> {
    let pages = {
        let core = lock(&state.core)?;
        core.job(&printer, job_id)
            .map(|j| j.pages.clone())
            .ok_or_else(|| "trabajo no encontrado".to_string())?
    };

    let pairing = mpa_sync::PairingStore::load(None).map_err(|e| e.to_string())?;
    let client = mpa_sync::SyncClient::new(&pairing.server_url, Some(pairing.agent_key))
        .map_err(|e| e.to_string())?;
    let request: Vec<mpa_sync::QuotePage> = pages
        .iter()
        .map(|p| mpa_sync::QuotePage {
            page_no: p.page_no,
            c: p.coverage.c,
            m: p.coverage.m,
            y: p.coverage.y,
            k: p.coverage.k,
            include: p.included,
        })
        .collect();
    let quote = client.quote(&request).map_err(|e| e.to_string())?;
    Ok(crate::dto::QuoteDto {
        bw: mode_quote(quote.for_mode(mpa_domain::PrintMode::Bw)),
        color: mode_quote(quote.for_mode(mpa_domain::PrintMode::Color)),
    })
}

fn mode_quote(mode: &mpa_sync::QuoteMode) -> crate::dto::ModeQuoteDto {
    crate::dto::ModeQuoteDto {
        total: mode.total,
        pages: mode
            .lines
            .iter()
            .map(|line| crate::dto::PageQuoteDto {
                page_no: line.page_no,
                unit_price: line.unit_price,
            })
            .collect(),
    }
}

/// The same checks `mpa doctor` runs, for the window's status panel.
#[tauri::command(async)]
pub fn doctor(state: State<'_, AppState>) -> Result<Vec<CheckDto>, String> {
    let jobs = {
        let core = lock(&state.core)?;
        core.job_count()
    };
    let mut rows = Vec::new();

    match jobs > 0 {
        true => rows.push(CheckDto {
            name: "trabajos".into(),
            health: "ok".into(),
            detail: format!("{jobs} en curso"),
        }),
        false => rows.push(CheckDto {
            name: "trabajos".into(),
            health: "ok".into(),
            detail: "sin trabajos retenidos".into(),
        }),
    }

    match mpa_agent::gate::reason() {
        None => rows.push(CheckDto {
            name: "plan".into(),
            health: "ok".into(),
            detail: "equipo autorizado".into(),
        }),
        Some(reason) => rows.push(CheckDto {
            name: "plan".into(),
            health: "fail".into(),
            detail: reason,
        }),
    }

    rows.push(match mpa_sync::PairingStore::load(None) {
        Ok(pairing) => CheckDto {
            name: "enlace".into(),
            health: "ok".into(),
            detail: format!(
                "{} · workspace {}",
                pairing.server_url, pairing.workspace_id
            ),
        },
        Err(_) => CheckDto {
            name: "enlace".into(),
            health: "warn".into(),
            detail: "sin enlazar: ejecuta `mpa pair <código>`".into(),
        },
    });

    Ok(rows)
}

/// Where the draft goes: a new ticket line set, an open ticket, or nowhere yet.
#[tauri::command(async)]
pub fn select_target(
    state: State<'_, AppState>,
    printer: String,
    job_id: u32,
    kind: String,
    ticket_id: Option<String>,
) -> Result<(), String> {
    let target = match kind.as_str() {
        "newDraft" => mpa_agent::TargetSelection::NewDraft,
        "ticket" => match ticket_id {
            Some(id) if !id.is_empty() => mpa_agent::TargetSelection::Ticket(id),
            _ => return Err("falta el ticket".into()),
        },
        _ => mpa_agent::TargetSelection::Unassigned,
    };
    let mut core = lock(&state.core)?;
    core.select_target(&printer, job_id, target)
        .map_err(|e| e.to_string())
}

/// Print preset for this job (`None` keeps the driver's own settings).
#[tauri::command(async)]
pub fn set_preset(
    state: State<'_, AppState>,
    printer: String,
    job_id: u32,
    preset: Option<String>,
) -> Result<(), String> {
    let mut core = lock(&state.core)?;
    core.set_preset(&printer, job_id, preset)
        .map_err(|e| e.to_string())
}

/// B/N or color for this job: it decides which rule set prices the pages.
#[tauri::command(async)]
pub fn set_print_mode(
    state: State<'_, AppState>,
    printer: String,
    job_id: u32,
    mode: String,
) -> Result<(), String> {
    let parsed = match mode.as_str() {
        "Color" => mpa_domain::PrintMode::Color,
        _ => mpa_domain::PrintMode::Bw,
    };
    let mut core = lock(&state.core)?;
    core.set_print_mode(&printer, job_id, parsed)
        .map_err(|e| e.to_string())
}
