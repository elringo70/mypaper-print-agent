use crate::core::{analyze_spool, detect, AgentCore, PauseOutcome, TargetSelection, WatchedJob};
use mpa_domain::{PrintMode, PrinterPolicy};
use mpa_spool::SpoolBackend;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::Duration;

/// Background supervisor: one watcher thread per watched printer.
/// Threads only touch the core behind its mutex — never GPUI state.
/// The UI polls `AgentCore::version` and mirrors snapshots.
pub struct AgentRuntime {
    core: Arc<Mutex<AgentCore>>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

/// Queue label for captured jobs. Distinct from any spooler printer name on
/// purpose: it is the key half of (printer, job_id).
pub const CAPTURE_PRINTER: &str = "MyPaper";

impl std::fmt::Debug for AgentRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRuntime")
            .field("threads", &self.threads.len())
            .finish_non_exhaustive()
    }
}

impl AgentRuntime {
    pub fn start(core: Arc<Mutex<AgentCore>>, policy: PrinterPolicy, quantum: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let printers: Vec<String> = {
            let core = core.lock().expect("agent core");
            core.watched_printers(&policy)
        };
        let mut threads = Vec::new();
        for printer in printers {
            let core = Arc::clone(&core);
            let stop = Arc::clone(&stop);
            threads.push(std::thread::spawn(move || {
                watch_printer(core, printer, quantum, stop)
            }));
        }
        Self {
            core,
            stop,
            threads,
        }
    }

    /// Feed the agent from our own IPP queue.
    ///
    /// This is the capture path that replaced watching a physical printer: the
    /// document arrives as PDF, already in the format the analyzer measures, and
    /// enters the same job map the window reads. Windows considers the job
    /// printed; the operator's authorisation is what puts it on paper.
    pub fn start_capture<F>(
        &mut self,
        listener: std::net::TcpListener,
        inbox: std::path::PathBuf,
        port: u16,
        quote: F,
    ) -> Result<(), std::io::Error>
    where
        F: Fn(&[crate::pilot::PilotPage]) -> Option<(f64, f64)> + Send + 'static,
    {
        let core = Arc::clone(&self.core);
        // Job ids are scoped by printer name, so the queue label keeps them
        // from ever colliding with spooler ids.
        let next: u32 = 1;
        std::thread::Builder::new()
            .name("mpa-ipp".into())
            .spawn(move || {
                let mut watched = next;
                let result = mpa_ipp::serve_with(listener, inbox.clone(), port, |job, path| {
                    // Measure inside the lock; everything slow (HTTP price, file
                    // write) happens after it, so the window never waits.
                    let pages = {
                        let mut core = core.lock().expect("agent core");
                        if let Err(e) = core.ingest_captured(
                            CAPTURE_PRINTER,
                            watched,
                            job.name.clone(),
                            &job.document,
                            Some(path.to_path_buf()),
                        ) {
                            eprintln!("mpa: captura {watched}: {e}");
                        }
                        core.snapshot()
                            .into_iter()
                            .find(|j| j.printer == CAPTURE_PRINTER && j.job_id == watched)
                            .map(|j| {
                                j.pages
                                    .iter()
                                    .map(|p| crate::pilot::PilotPage {
                                        page_no: p.page_no,
                                        class: p
                                            .area
                                            .and_then(|a| a.class)
                                            .map(|c| format!("{c:?}").to_lowercase()),
                                        area_pct: p.area.map(|a| a.area_pct),
                                        c: p.coverage.c,
                                        m: p.coverage.m,
                                        y: p.coverage.y,
                                        k: p.coverage.k,
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    };
                    watched = watched.saturating_add(1);

                    // Shadow journal: the pilot measures and prints, nothing is
                    // charged, and every sheet is left on record to compare.
                    let (bw_total, color_total) = quote(&pages)
                        .map(|(bw, color)| (Some(bw), Some(color)))
                        .unwrap_or((None, None));
                    let entry = crate::pilot::PilotEntry {
                        at: chrono::Utc::now().to_rfc3339(),
                        document: job.name.clone(),
                        pages,
                        bw_total,
                        color_total,
                    };
                    if let Err(e) = crate::pilot::append(&inbox, &entry) {
                        eprintln!("mpa: no se pudo escribir el diario del piloto: {e}");
                    }
                });
                if let Err(e) = result {
                    eprintln!("mpa: endpoint IPP detenido: {e}");
                }
            })?;
        Ok(())
    }

    pub fn core(&self) -> Arc<Mutex<AgentCore>> {
        Arc::clone(&self.core)
    }

    pub fn shutdown(self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

fn watch_printer(
    core: Arc<Mutex<AgentCore>>,
    printer: String,
    quantum: Duration,
    stop: Arc<AtomicBool>,
) {
    // Spooler handle OUTSIDE the core lock: `wait_for_new_job` blocks up to
    // `quantum`, and the UI thread must keep getting the lock every 500 ms.
    // Holding the guard across that wait freezes the window long enough for
    // Windows to flag the app as "Not Responding".
    let backend = core
        .lock()
        .map(|c| c.backend())
        .unwrap_or_else(|_| mpa_spool::default_backend().into());
    let mut seen = std::collections::HashSet::new();
    // Re-adopt what this agent (or a previous run of it) already holds, before
    // the arrival loop: `wait_for_new_job` only fires on ADD_JOB/SET_JOB, so a
    // job paused by an earlier process is never reported again.
    recover_held_jobs(&core, backend.as_ref(), &printer, &mut seen);
    while !stop.load(Ordering::Relaxed) {
        let active = core.lock().map(|c| c.is_active()).unwrap_or(false);
        if !active {
            std::thread::sleep(quantum);
            continue;
        }
        // Settle one pending subset reprint per cycle (unlocked Ghostscript).
        match crate::reprint::sweep_one_reprint(&core) {
            crate::reprint::SweepOutcome::Reprinted { key } => {
                crate::log::info(format!("reprint {key}: subset sent"));
            }
            crate::reprint::SweepOutcome::Failed { key, error } => {
                crate::log::warn(format!("reprint {key}: still pending ({error})"));
            }
            crate::reprint::SweepOutcome::Idle => {}
        }
        let arrival = match backend.wait_for_new_job(&printer, quantum) {
            Ok(arrival) => arrival,
            Err(e) => {
                crate::log::warn(format!("watch {printer}: {e}"));
                std::thread::sleep(quantum);
                continue;
            }
        };
        let Some(job_id) = arrival else { continue };
        if !seen.insert(job_id) {
            continue;
        }
        hold_and_analyze(&core, backend.as_ref(), &printer, job_id);
    }
}

/// Re-adopt the jobs this agent left on hold.
///
/// A job the agent paused stays paused in the spooler across restarts, but the
/// spooler only reports ADD_JOB/SET_JOB notifications, so nothing surfaces it
/// again: the window opens with an empty queue while the printer stays blocked
/// and the operator has no way to release the job from the app.
fn recover_held_jobs(
    core: &Arc<Mutex<AgentCore>>,
    backend: &dyn SpoolBackend,
    printer: &str,
    seen: &mut std::collections::HashSet<u32>,
) {
    let jobs = match backend.list_jobs(printer) {
        Ok(jobs) => jobs,
        Err(e) => {
            crate::log::warn(format!("recover {printer}: {e}"));
            return;
        }
    };
    for job in jobs.iter().filter(|j| j.status_text == "PAUSED") {
        if !seen.insert(job.id) {
            continue;
        }
        crate::log::info(format!(
            "recover {printer} job {}: held before this run",
            job.id
        ));
        // Record it first so the window shows the held job immediately; the
        // analysis below fills in pages a few seconds later.
        if let Ok(mut c) = core.lock() {
            c.adopt_job(WatchedJob {
                printer: printer.into(),
                job_id: job.id,
                document: job.document.clone(),
                state: mpa_domain::JobState::Paused,
                pages: Vec::new(),
                analysis_error: None,
                warnings: vec!["recuperado de una ejecución anterior".into()],
                target: TargetSelection::Unassigned,
                format: None,
                spool_path: None,
                print_mode: PrintMode::Bw,
                preset: None,
            });
        }
        hold_and_analyze(core, backend, printer, job.id);
    }
}

/// Pause (idempotent for an already-held job) and analyze one job.
///
/// Both halves run WITHOUT the core lock: they touch the spooler and/or
/// Ghostscript for seconds and MUST stay off it, because the UI thread polls
/// that lock every 500 ms and a guard held across this work is what froze the
/// window ("Not Responding"). Same discipline as `reprint::sweep_one_reprint`
/// and `sync::drain_outbox`.
fn hold_and_analyze(
    core: &Arc<Mutex<AgentCore>>,
    backend: &dyn SpoolBackend,
    printer: &str,
    job_id: u32,
) {
    // A blocked device must not pause anything. Holding a job it cannot price
    // would leave a customer waiting for a print they already paid for, so the
    // job is allowed through unmanaged and only the log records why.
    if let Some(reason) = crate::gate::reason() {
        crate::log::warn(format!(
            "bloqueado ({reason}): {printer} job {job_id} pasa sin gestionar"
        ));
        return;
    }
    let pause = match detect(backend, printer, job_id) {
        Ok(detected) => core
            .lock()
            .map(|mut c| c.record_detected(printer, job_id, detected))
            .unwrap_or(PauseOutcome::Missed),
        Err(e) => {
            crate::log::warn(format!("pause {printer} job {job_id}: {e}"));
            return;
        }
    };
    match pause {
        PauseOutcome::Missed => return,
        PauseOutcome::Escaped => {
            crate::log::warn(format!(
                "hold {printer} job {job_id}: paused late, pages may have escaped"
            ));
        }
        PauseOutcome::Paused => {}
    }
    // Analysis runs here, unlocked (background thread, never the UI thread);
    // the guard is taken back only to record the outcome.
    let version = core.lock().map(|c| c.version()).unwrap_or_default();
    let analysis = analyze_spool(backend, printer, job_id, version);
    match core.lock() {
        Ok(mut c) => {
            if let Err(e) = c.record_analysis(printer, job_id, analysis) {
                crate::log::warn(format!("analyze {printer} job {job_id}: {e}"));
            }
        }
        Err(_) => {
            crate::log::error(format!(
                "analyze {printer} job {job_id}: core lock poisoned"
            ));
        }
    }
}
