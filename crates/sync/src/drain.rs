use crate::client::{SyncClient, SyncError};
use mpa_agent::{AgentCore, TargetSelection};
use std::sync::{Arc, Mutex};

/// Result of one outbox drain, rendered by the CLI/UI notices.
#[derive(Debug, Default)]
pub struct DrainReport {
    pub pushed: Vec<String>,
    pub push_failed: Vec<(String, String)>,
    pub materialized: Vec<String>,
    pub materialize_failed: Vec<(String, String)>,
    pub reprinted: Vec<String>,
    pub reprint_failed: Vec<(String, String)>,
    pub removed: usize,
}

impl DrainReport {
    pub fn summary(&self) -> String {
        format!(
            "sync: {} enviados, {} fallidos · ticket: {} con líneas, {} pendientes · reimpresión: {} ok, {} pendientes · {} borradores listos",
            self.pushed.len(),
            self.push_failed.len(),
            self.materialized.len(),
            self.materialize_failed.len(),
            self.reprinted.len(),
            self.reprint_failed.len(),
            self.removed
        )
    }

    pub fn has_work_left(&self) -> bool {
        !self.push_failed.is_empty()
            || !self.materialize_failed.is_empty()
            || !self.reprint_failed.is_empty()
    }
}

/// Push every unsynced entry, inject ticket-targeted drafts as order lines,
/// then attempt every pending reprint. Entries that are synced, materialized
/// (or ticketless) and printed are removed (their cached spool bytes go with
/// them). Indices run backwards so removals are safe.
pub fn drain_outbox(core: &Arc<Mutex<AgentCore>>, client: &SyncClient) -> DrainReport {
    // Locking discipline (hardened): the core guard is NEVER held across
    // network or Ghostscript calls. Each step clones what it needs under a
    // brief lock, works unlocked, then re-locks to record. The UI thread
    // polls this same lock every 500 ms; holding it across a 15 s HTTP
    // timeout is what froze the window ("Not Responding").
    let mut report = DrainReport::default();
    let indices: Vec<usize> = core
        .lock()
        .map(|c| (0..c.outbox_len()).rev().collect())
        .unwrap_or_default();

    for i in indices {
        let Some(entry) = core.lock().ok().and_then(|c| c.outbox_entry(i).cloned()) else {
            continue;
        };
        let key = entry.idempotency_key.clone();

        if !entry.synced {
            match client.post_job(&entry) {
                Ok(posted) => {
                    // Confirmado sólo si el servidor cotizó las mismas páginas
                    // que medimos: menos significa que el documento no llegó
                    // completo y no se puede cobrar así.
                    let expected = entry.pages.len() as i32;
                    if let Some(priced) = posted.page_count {
                        if priced != expected {
                            report.push_failed.push((
                                key.clone(),
                                format!("el servidor cotizó {priced} de {expected} páginas"),
                            ));
                            continue;
                        }
                    }
                    if let Ok(mut c) = core.lock() {
                        c.mark_synced(i, posted.id.clone());
                    }
                    report.pushed.push(format!("{key} -> {}", posted.id));
                }
                Err(e) => {
                    report.push_failed.push((key.clone(), friendly(&e)));
                    continue;
                }
            }
        }

        let needs_materialize = core
            .lock()
            .ok()
            .and_then(|c| c.outbox_entry(i).cloned())
            .map(|e| matches!(e.target, TargetSelection::Ticket(_)) && !e.materialized)
            .unwrap_or(false);
        if needs_materialize {
            let remote_id = core
                .lock()
                .ok()
                .and_then(|c| c.outbox_entry(i).cloned())
                .and_then(|e| e.remote_id)
                .unwrap_or_default();
            match client.materialize_job(&remote_id) {
                Ok(ack) => {
                    if let Ok(mut c) = core.lock() {
                        c.mark_materialized(i);
                    }
                    report.materialized.push(format!(
                        "{key} -> ticket #{} (+${:.2})",
                        ack.ticket_number, ack.added_total
                    ));
                }
                Err(e) => {
                    report.materialize_failed.push((key.clone(), friendly(&e)));
                }
            }
        }

        // Reprint WITHOUT holding the guard: clone, print unlocked, mark.
        let reprint_entry = core
            .lock()
            .ok()
            .and_then(|c| c.outbox_entry(i).cloned())
            .filter(|e| e.reprint_pending);
        if let Some(entry) = reprint_entry {
            match mpa_agent::attempt_reprint(&entry) {
                Ok(()) => {
                    if let Ok(mut c) = core.lock() {
                        c.mark_reprinted(&entry.idempotency_key);
                    }
                    report.reprinted.push(key.clone());
                }
                Err(e) => report.reprint_failed.push((key.clone(), e.to_string())),
            }
        }

        let done = core
            .lock()
            .ok()
            .and_then(|c| c.outbox_entry(i).cloned())
            .map(|e| {
                e.synced
                    && !e.reprint_pending
                    && (e.materialized || !matches!(e.target, TargetSelection::Ticket(_)))
            })
            .unwrap_or(false);
        if done {
            if let Ok(mut c) = core.lock() {
                c.remove_outbox(i);
            }
            report.removed += 1;
        }
    }
    mpa_agent::audit::record("sync", "", None, report.summary());
    report
}

fn friendly(e: &SyncError) -> String {
    match e {
        SyncError::Status(401, _) => "no autorizado (re-enlaza con `mpa pair`)".into(),
        SyncError::Status(403, _) => "requiere plan premium".into(),
        SyncError::Status(code, _) => format!("servidor {code}"),
        _ => e.to_string(),
    }
}
