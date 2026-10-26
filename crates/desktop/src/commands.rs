//! Commands the window can call.
//!
//! Every command takes the core lock briefly and never does IO inside it: the
//! spooler and the printer are slow, and the watcher threads need that lock.
//! Long work (reprint, sync) already lives on its own thread.

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

/// Full snapshot, cheap enough to poll every second.
#[tauri::command]
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
                    thumbnail: data_url(&page.thumbnail_png),
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

/// Include or exclude one page from the billed job.
#[tauri::command]
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
    let current = core
        .snapshot()
        .into_iter()
        .find(|j| j.printer == printer && j.job_id == job_id)
        .and_then(|j| j.pages.into_iter().find(|p| p.page_no == page_no))
        .map(|p| p.included);
    if current == Some(included) {
        return Ok(());
    }
    core.toggle_page(&printer, job_id, page_no)
        .map_err(|e| e.to_string())
}

/// Release the job and hand the selected pages to the outbox for MyPaper.
#[tauri::command]
pub fn complete_job(
    state: State<'_, AppState>,
    printer: String,
    job_id: u32,
) -> Result<(), String> {
    let mut core = lock(&state.core)?;
    core.complete_job(&printer, job_id)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Cancel the job in the spooler; nothing is billed.
#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>, printer: String, job_id: u32) -> Result<(), String> {
    let mut core = lock(&state.core)?;
    core.cancel_job(&printer, job_id).map_err(|e| e.to_string())
}

/// The same checks `mpa doctor` runs, for the window's status panel.
#[tauri::command]
pub fn doctor(state: State<'_, AppState>) -> Result<Vec<CheckDto>, String> {
    let core = lock(&state.core)?;
    let mut rows = Vec::new();

    match !core.snapshot().is_empty() {
        true => rows.push(CheckDto {
            name: "trabajos".into(),
            health: "ok".into(),
            detail: format!("{} en curso", core.snapshot().len()),
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
#[tauri::command]
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
#[tauri::command]
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
#[tauri::command]
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
