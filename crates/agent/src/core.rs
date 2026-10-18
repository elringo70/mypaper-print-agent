use crate::reprint::{cache_spool_bytes, cleanup_spool_file, ReprintError};
use crate::tickets::{Ticket, TicketError, TicketSource};
use mpa_analyzer::{
    analyze_bytes, capture_spool_file, AnalyzeError, CaptureError, Ghostscript, SpoolFormat,
};
use mpa_domain::{classify_coverage, transition, Coverage, JobEvent, JobState, Tier};
use mpa_spool::{SpoolBackend, SpoolError};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Spool(#[from] SpoolError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Analyze(#[from] AnalyzeError),
    #[error(transparent)]
    Tickets(#[from] TicketError),
    #[error(transparent)]
    Reprint(#[from] ReprintError),
    #[error("job not found: {0} on {1}")]
    JobNotFound(u32, String),
    #[error("nothing to complete: no included pages")]
    NothingIncluded,
    #[error("job has no analysis yet")]
    NoAnalysis,
    #[error("partial completion needs a PDF/PS job, got {0:?}")]
    SubsetUnsupported(SpoolFormat),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum AgentMode {
    /// Intercept and pause new jobs.
    #[default]
    Active,
    /// Let everything print untouched.
    Suspended,
}

/// Where the draft goes on completion.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum TargetSelection {
    #[default]
    Unassigned,
    Ticket(String),
    NewDraft,
}

#[derive(Debug, Clone)]
pub struct WatchedPage {
    pub page_no: u32,
    pub coverage: Coverage,
    pub tier_id: Option<String>,
    pub included: bool,
    pub thumbnail_png: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct WatchedJob {
    pub printer: String,
    pub job_id: u32,
    pub document: String,
    pub state: JobState,
    pub pages: Vec<WatchedPage>,
    pub analysis_error: Option<String>,
    /// Non-fatal interception notes (e.g. pages printed before the pause
    /// landed). Surfaced amber in the UI; never blocks the flow.
    pub warnings: Vec<String>,
    pub target: TargetSelection,
    pub format: Option<SpoolFormat>,
    pub spool_path: Option<PathBuf>,
}

/// Outcome of intercepting a job (see `detect_and_pause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseOutcome {
    Paused,
    Missed,
    Escaped,
}

/// Spooler-side result of one `detect` pass, before any core state exists.
/// Produced OUTSIDE the core lock (the pause retries and the verifying
/// listing are spooler IO) and applied with `AgentCore::record_detected`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detected {
    /// The job is held. `escaped` marks a pause that landed too late: the
    /// first pages may already have printed.
    Held { document: String, escaped: bool },
    /// The job vanished before the hold; nothing to record.
    Missed,
}

/// One analyzed page, still without its tier: classification needs the
/// core's synced tabulador, which only the locked half can read.
#[derive(Debug, Clone)]
pub struct AnalyzedPage {
    pub page_no: u32,
    pub coverage: Coverage,
    pub thumbnail_png: Vec<u8>,
}

/// Result of the heavy half of `analyze_job`: spool capture, Ghostscript
/// rendering and the cached spool bytes for a possible subset reprint.
/// Built OUTSIDE the core lock (seconds of work) and applied with
/// `AgentCore::record_analysis`.
#[derive(Debug, Clone)]
pub struct AnalyzedSpool {
    pub format: SpoolFormat,
    pub pages: Vec<AnalyzedPage>,
    pub spool_path: Option<PathBuf>,
}

/// A failed analysis pass: the typed error for the caller plus the
/// operator-facing message the UI shows. The two differ for server-rendered
/// network jobs, which have no local spool file at all.
#[derive(Debug)]
pub struct AnalysisFailure {
    pub message: String,
    pub error: AgentError,
}

/// Pause `job_id` on `printer` and verify the hold, touching only the
/// spooler. Blocking (retry sleeps + a fresh listing): call it OUTSIDE the
/// core lock — the UI thread polls that lock every 500 ms and a guard held
/// across spooler/plugin IO is what flags the window "Not Responding".
///
/// Unlocked half of `detect_and_pause`; pair it with
/// `AgentCore::record_detected`.
pub fn detect(
    backend: &dyn SpoolBackend,
    printer: &str,
    job_id: u32,
) -> Result<Detected, AgentError> {
    let mut attempts = 0;
    loop {
        match backend.pause_job(printer, job_id) {
            Ok(()) => break,
            Err(SpoolError::JobNotFound(_, _)) => return Ok(Detected::Missed),
            Err(e @ (SpoolError::PrinterNotFound(_) | SpoolError::AccessDenied(_))) => {
                return Err(e.into())
            }
            Err(e) => {
                attempts += 1;
                if attempts >= 3 {
                    return Err(e.into());
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
    }
    let listed = backend.list_jobs(printer)?;
    let Some(info) = listed.into_iter().find(|j| j.id == job_id) else {
        crate::audit::record(
            "missed",
            printer,
            Some(job_id),
            "vanished before the hold".into(),
        );
        return Ok(Detected::Missed);
    };
    let escaped = info.status_text != "PAUSED";
    let document = info.document;
    let outcome = if escaped {
        PauseOutcome::Escaped
    } else {
        PauseOutcome::Paused
    };
    crate::audit::record(
        "paused",
        printer,
        Some(job_id),
        format!("{document} ({outcome:?})"),
    );
    Ok(Detected::Held { document, escaped })
}

/// Capture the spool file, render it with Ghostscript and cache the bytes.
/// Blocking (seconds): call it OUTSIDE the core lock. Unlocked half of
/// `analyze_job`; pair it with `AgentCore::record_analysis`.
pub fn analyze_spool(
    backend: &dyn SpoolBackend,
    printer: &str,
    job_id: u32,
    version: u64,
) -> Result<AnalyzedSpool, AnalysisFailure> {
    let spool = match capture_spool_file(job_id, printer) {
        Ok(spool) => spool,
        Err(e) => {
            // Server-rendered network jobs leave no local spool file.
            // Say so explicitly instead of a bare "not found": the fix
            // lives in the printer properties, not in a retry.
            let message = match &e {
                CaptureError::NotFound(_, _)
                    if backend.is_network_printer(printer).unwrap_or(false) =>
                {
                    format!(
                        "sin spool local: '{printer}' renderiza en el servidor; \
                         activa el renderizado en el cliente o imprime a PDF"
                    )
                }
                // %SystemRoot%\System32\spool\PRINTERS only opens for
                // Administrators and the spooler: a plain user gets os error 5
                // here and every analysis fails without saying why.
                CaptureError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied => {
                    format!(
                        "sin acceso a la carpeta de spool de '{printer}': \
                         ejecuta MyPaper Print Agent como administrador"
                    )
                }
                _ => e.to_string(),
            };
            return Err(AnalysisFailure {
                message,
                error: e.into(),
            });
        }
    };
    let gs = Ghostscript::discover().ok();
    let analysis = match analyze_bytes(&spool.data, gs.as_ref()) {
        Ok(analysis) => analysis,
        Err(e) => {
            return Err(AnalysisFailure {
                message: e.to_string(),
                error: e.into(),
            })
        }
    };
    // Keep our own copy: the spooler deletes its file once the held job is
    // cancelled, but a partial completion still needs to reprint.
    let spool_path = cache_spool_bytes(printer, job_id, version, &spool.data).ok();
    Ok(AnalyzedSpool {
        format: analysis.format,
        pages: analysis
            .pages
            .into_iter()
            .map(|p| AnalyzedPage {
                page_no: p.page_no,
                coverage: p.coverage,
                thumbnail_png: p.thumbnail_png,
            })
            .collect(),
        spool_path,
    })
}

impl WatchedJob {
    pub fn included_pages(&self) -> usize {
        self.pages.iter().filter(|p| p.included).count()
    }

    pub fn all_included(&self) -> bool {
        !self.pages.is_empty() && self.pages.iter().all(|p| p.included)
    }

    /// Included-page count per tier, for the summary line.
    pub fn tier_counts(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for page in self.pages.iter().filter(|p| p.included) {
            let key = page.tier_id.clone().unwrap_or_else(|| "NONE".into());
            if !counts.contains_key(&key) {
                order.push(key.clone());
            }
            *counts.entry(key).or_default() += 1;
        }
        order.into_iter().map(|k| (k.clone(), counts[&k])).collect()
    }
}

/// One analyzed page as the server expects it (`include=false` = unchecked,
/// priced zero server-side).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutboxPage {
    pub page_no: u32,
    pub coverage: Coverage,
    pub tier_id: Option<String>,
    pub included: bool,
}

/// Draft for the MyPaper sync. The server prices authoritatively and is
/// idempotent on `idempotency_key`, so offline retries are safe.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutboxEntry {
    pub idempotency_key: String,
    pub printer: String,
    pub job_id: u32,
    pub target: TargetSelection,
    pub pages: Vec<OutboxPage>,
    pub format: Option<SpoolFormat>,
    pub spool_path: Option<PathBuf>,
    /// Partial completion cancels the held job; the subset must be reprinted
    /// from the cached spool bytes.
    pub reprint_pending: bool,
    pub synced: bool,
    pub remote_id: Option<String>,
    /// Lines injected into the ticket via `materialize`. Defaults false so
    /// pre-existing state files keep working.
    #[serde(default)]
    pub materialized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// Every page included: original job released to the printer.
    ResumedAll,
    /// Partial selection: original cancelled, draft kept for reprint.
    CancelledPartial,
}

pub struct AgentCore {
    // Manual `Debug` below: trait objects and handles have no useful dump.
    backend: Arc<dyn SpoolBackend>,
    tickets: Arc<dyn TicketSource>,
    /// Mirror of `tickets.list_open_tickets()` for the UI. `TicketSource`
    /// callers can block for the whole HTTP timeout, so the UI never calls
    /// them: a background refresher fills this under a brief lock and the
    /// UI reads it in-memory (see `cached_tickets`).
    tickets_cache: Vec<Ticket>,
    mode: AgentMode,
    tiers: Vec<Tier>,
    jobs: HashMap<(String, u32), WatchedJob>,
    outbox: Vec<OutboxEntry>,
    pending_notice: Option<String>,
    quit_requested: bool,
    pending_update: Option<mpa_domain::ReleaseManifest>,
    version: u64,
}

impl std::fmt::Debug for AgentCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCore")
            .field("mode", &self.mode)
            .field("tiers", &self.tiers.len())
            .field("jobs", &self.jobs.len())
            .field("outbox", &self.outbox.len())
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl AgentCore {
    pub fn new(
        backend: Arc<dyn SpoolBackend>,
        tickets: Arc<dyn TicketSource>,
        tiers: Vec<Tier>,
    ) -> Self {
        Self {
            backend,
            tickets,
            tickets_cache: Vec::new(),
            mode: AgentMode::Active,
            tiers,
            jobs: HashMap::new(),
            outbox: Vec::new(),
            pending_notice: None,
            quit_requested: false,
            pending_update: None,
            version: 0,
        }
    }

    /// Message for the UI, set by background threads (sync/drain).
    /// The UI moves it into its notice bar on the next refresh.
    pub fn push_notice(&mut self, notice: String) {
        self.pending_notice = Some(notice);
        self.bump();
    }

    pub fn take_notice(&mut self) -> Option<String> {
        self.pending_notice.take()
    }

    /// Asked by the tray menu. The UI thread observes it on its poll tick
    /// and quits the event loop from there (GPUI contexts are main-thread).
    pub fn request_quit(&mut self) {
        self.quit_requested = true;
        self.bump();
    }

    pub fn take_quit_request(&mut self) -> bool {
        std::mem::replace(&mut self.quit_requested, false)
    }

    /// Release waiting for the operator to apply it (set by update checks).
    pub fn set_pending_update(&mut self, manifest: Option<mpa_domain::ReleaseManifest>) {
        self.pending_update = manifest;
        self.bump();
    }

    pub fn pending_update(&self) -> Option<mpa_domain::ReleaseManifest> {
        self.pending_update.clone()
    }

    /// Monotonic counter: the UI polls it and re-reads the snapshot on change.
    pub fn version(&self) -> u64 {
        self.version
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub fn mode(&self) -> AgentMode {
        self.mode
    }

    /// Shared spooler handle, so background workers can call blocking spooler
    /// operations WITHOUT holding the core lock (the UI thread needs that
    /// lock every 500 ms — holding it across a 2 s spooler wait freezes the
    /// window long enough for Windows to flag "Not Responding").
    pub fn backend(&self) -> Arc<dyn SpoolBackend> {
        Arc::clone(&self.backend)
    }

    /// Clone the oldest entry awaiting a subset reprint (brief lock only).
    /// The caller runs Ghostscript UNLOCKED, then reports back with
    /// `mark_reprinted`.
    pub fn claim_reprint_entry(&self) -> Option<OutboxEntry> {
        self.outbox.iter().find(|e| e.reprint_pending).cloned()
    }

    /// Clear the reprint flag after a successful out-of-lock reprint.
    /// Returns false if the entry vanished meanwhile (drain won the race).
    pub fn mark_reprinted(&mut self, idempotency_key: &str) -> bool {
        if let Some(entry) = self
            .outbox
            .iter_mut()
            .find(|e| e.idempotency_key == idempotency_key)
        {
            entry.reprint_pending = false;
            self.bump();
            true
        } else {
            false
        }
    }

    pub fn is_active(&self) -> bool {
        self.mode == AgentMode::Active
    }

    /// Printers the runtime should watch under `policy`.
    pub fn watched_printers(&self, policy: &mpa_domain::PrinterPolicy) -> Vec<String> {
        self.backend
            .list_printers()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.name)
            .filter(|name| policy.watches(name))
            .collect()
    }

    /// One wait quantum on `printer`. `None` = timeout, no arrival.
    pub fn wait_for_arrival(
        &self,
        printer: &str,
        quantum: std::time::Duration,
    ) -> Option<Result<u32, AgentError>> {
        match self.backend.wait_for_new_job(printer, quantum) {
            Ok(None) => None,
            Ok(Some(job_id)) => Some(Ok(job_id)),
            Err(e) => Some(Err(e.into())),
        }
    }

    pub fn set_mode(&mut self, mode: AgentMode) {
        self.mode = mode;
        self.bump();
    }

    pub fn set_tiers(&mut self, tiers: Vec<Tier>) {
        self.tiers = tiers;
        self.bump();
    }

    pub fn set_ticket_source(&mut self, tickets: Arc<dyn TicketSource>) {
        self.tickets = tickets;
        self.bump();
    }

    /// Record a job the agent already holds, without touching the spooler.
    ///
    /// Recovery path for jobs that outlive the process: a held job stays paused
    /// in the spooler after a crash/kill, so the operator must be able to see
    /// it again (and release or complete it) once the agent restarts.
    pub fn adopt_job(&mut self, job: WatchedJob) {
        self.jobs.insert((job.printer.clone(), job.job_id), job);
        self.bump();
    }

    pub fn snapshot(&self) -> Vec<WatchedJob> {
        let mut jobs: Vec<WatchedJob> = self.jobs.values().cloned().collect();
        jobs.sort_by(|a, b| a.printer.cmp(&b.printer).then(a.job_id.cmp(&b.job_id)));
        jobs
    }

    pub fn outbox(&self) -> &[OutboxEntry] {
        &self.outbox
    }

    pub fn outbox_len(&self) -> usize {
        self.outbox.len()
    }

    pub fn pending_outbox(&self) -> usize {
        self.outbox
            .iter()
            .filter(|e| !e.synced || e.reprint_pending)
            .count()
    }

    pub fn tiers(&self) -> &[Tier] {
        &self.tiers
    }

    pub fn set_outbox(&mut self, outbox: Vec<OutboxEntry>) {
        self.outbox = outbox;
        self.bump();
    }

    pub fn outbox_entry(&self, index: usize) -> Option<&OutboxEntry> {
        self.outbox.get(index)
    }

    /// Remove an entry, deleting its cached spool bytes.
    pub fn remove_outbox(&mut self, index: usize) -> Option<OutboxEntry> {
        if index >= self.outbox.len() {
            return None;
        }
        let entry = self.outbox.remove(index);
        cleanup_spool_file(entry.spool_path.as_deref());
        self.bump();
        Some(entry)
    }

    pub fn mark_synced(&mut self, index: usize, remote_id: String) {
        if let Some(entry) = self.outbox.get_mut(index) {
            entry.synced = true;
            entry.remote_id = Some(remote_id);
            self.bump();
        }
    }

    pub fn mark_materialized(&mut self, index: usize) {
        if let Some(entry) = self.outbox.get_mut(index) {
            entry.materialized = true;
            self.bump();
        }
    }

    /// Open tickets straight from the configured source. Blocking for the
    /// HTTP source (up to the 15 s client timeout): never call it from the UI
    /// thread or while holding the core lock on a path the UI polls. The UI
    /// reads `cached_tickets` instead.
    pub fn list_open_tickets(&self) -> Result<Vec<Ticket>, AgentError> {
        Ok(self.tickets.list_open_tickets()?)
    }

    /// Shared ticket source, so a background refresher can run the blocking
    /// listing WITHOUT holding the core lock.
    pub fn ticket_source(&self) -> Arc<dyn TicketSource> {
        Arc::clone(&self.tickets)
    }

    /// Tickets mirrored for the UI: pure in-memory read, never does IO.
    pub fn cached_tickets(&self) -> &[Ticket] {
        &self.tickets_cache
    }

    /// Replace the mirrored tickets (called by the background refresher under
    /// a brief lock). Bumps the version only on change, so an idle poll does
    /// not force a re-render.
    pub fn set_cached_tickets(&mut self, tickets: Vec<Ticket>) {
        let changed = tickets.len() != self.tickets_cache.len()
            || tickets.iter().zip(&self.tickets_cache).any(|(new, old)| {
                new.id != old.id
                    || new.ticket_number != old.ticket_number
                    || new.cashier != old.cashier
                    || new.total != old.total
            });
        self.tickets_cache = tickets;
        if changed {
            self.bump();
        }
    }

    fn job_mut(&mut self, printer: &str, job_id: u32) -> Result<&mut WatchedJob, AgentError> {
        self.jobs
            .get_mut(&(printer.to_string(), job_id))
            .ok_or_else(|| AgentError::JobNotFound(job_id, printer.into()))
    }

    /// Record a detection performed by `detect`. Brief lock only: the
    /// spooler IO already happened outside it.
    pub fn record_detected(
        &mut self,
        printer: &str,
        job_id: u32,
        detected: Detected,
    ) -> PauseOutcome {
        match detected {
            Detected::Missed => PauseOutcome::Missed,
            Detected::Held { document, escaped } => {
                self.jobs.insert(
                    (printer.to_string(), job_id),
                    WatchedJob {
                        printer: printer.into(),
                        job_id,
                        document,
                        state: JobState::Paused,
                        pages: Vec::new(),
                        analysis_error: None,
                        warnings: if escaped {
                            vec!["El trabajo siguió imprimiendo tras la pausa: \
                                 las primeras páginas pudieron haberse impreso sin cobro"
                                .into()]
                        } else {
                            Vec::new()
                        },
                        target: TargetSelection::Unassigned,
                        format: None,
                        spool_path: None,
                    },
                );
                self.bump();
                if escaped {
                    PauseOutcome::Escaped
                } else {
                    PauseOutcome::Paused
                }
            }
        }
    }

    /// Record a detected job and pause it in the spooler, verifying the hold.
    ///
    /// Fast drivers spool the first pages before we arrive: the pause is
    /// retried on transient failures, then verified against a fresh listing.
    /// `Missed` (job gone) records nothing; `Escaped` (still printing
    /// despite the pause) records a warning the UI surfaces — Windows cannot
    /// un-print escaped pages. Permanent failures (unknown job/printer, no
    /// rights) return immediately.
    ///
    /// Convenience for direct callers (CLI, tests) that hold the lock for the
    /// whole call. The UI runtime must NOT use it: it wraps `detect` +
    /// `record_detected` so the spooler IO stays off the locked path.
    pub fn detect_and_pause(
        &mut self,
        printer: &str,
        job_id: u32,
    ) -> Result<PauseOutcome, AgentError> {
        let detected = detect(self.backend.as_ref(), printer, job_id)?;
        Ok(self.record_detected(printer, job_id, detected))
    }

    /// Apply an analysis pass performed by `analyze_spool`. Brief lock only:
    /// Ghostscript already ran outside it.
    pub fn record_analysis(
        &mut self,
        printer: &str,
        job_id: u32,
        result: Result<AnalyzedSpool, AnalysisFailure>,
    ) -> Result<(), AgentError> {
        let analyzed = match result {
            Ok(analyzed) => analyzed,
            Err(failure) => {
                self.job_mut(printer, job_id)?.analysis_error = Some(failure.message.clone());
                self.bump();
                crate::audit::record("analyze_failed", printer, Some(job_id), failure.message);
                return Err(failure.error);
            }
        };
        let tiers = self.tiers.clone();
        let job = self.job_mut(printer, job_id)?;
        job.analysis_error = None;
        job.format = Some(analyzed.format);
        job.spool_path = analyzed.spool_path;
        job.pages = analyzed
            .pages
            .into_iter()
            .map(|p| {
                let color = p.coverage.color_coverage();
                WatchedPage {
                    page_no: p.page_no,
                    coverage: p.coverage,
                    tier_id: classify_coverage(color, &tiers).map(|t| t.id.clone()),
                    included: true,
                    thumbnail_png: p.thumbnail_png,
                }
            })
            .collect();
        let pages = job.pages.len();
        let format = job.format;
        self.bump();
        crate::audit::record(
            "analyzed",
            printer,
            Some(job_id),
            format!("{pages} pages, {format:?}"),
        );
        Ok(())
    }

    /// Capture the spool file and attach the per-page analysis.
    /// The job stays paused on analysis failure, with the error surfaced.
    ///
    /// Convenience for direct callers (CLI, tests) that hold the lock for the
    /// whole call; the UI runtime uses `analyze_spool` + `record_analysis`.
    pub fn analyze_job(&mut self, printer: &str, job_id: u32) -> Result<(), AgentError> {
        let result = analyze_spool(self.backend.as_ref(), printer, job_id, self.version);
        self.record_analysis(printer, job_id, result)
    }

    pub fn toggle_page(
        &mut self,
        printer: &str,
        job_id: u32,
        page_no: u32,
    ) -> Result<(), AgentError> {
        let job = self.job_mut(printer, job_id)?;
        if let Some(page) = job.pages.iter_mut().find(|p| p.page_no == page_no) {
            page.included = !page.included;
        }
        self.bump();
        Ok(())
    }

    pub fn select_target(
        &mut self,
        printer: &str,
        job_id: u32,
        target: TargetSelection,
    ) -> Result<(), AgentError> {
        self.job_mut(printer, job_id)?.target = target;
        self.bump();
        Ok(())
    }

    /// Operator decision: release or cancel the held job and record the draft.
    /// Full selection resumes the original. Partial selection cancels it and
    /// immediately reprints the subset; if the reprint is impossible right
    /// now (no Ghostscript, unsupported format), the draft stays pending and
    /// `retry_reprint` can run later. The spool job is only cancelled once a
    /// reprint path exists.
    pub fn complete_job(&mut self, printer: &str, job_id: u32) -> Result<Completion, AgentError> {
        let (all_pages, target, format, spool_path) = {
            let job = self.job_mut(printer, job_id)?;
            if job.state != JobState::Paused {
                return Err(AgentError::NoAnalysis);
            }
            if job.pages.is_empty() {
                return Err(AgentError::NoAnalysis);
            }
            (
                job.pages.clone(),
                job.target.clone(),
                job.format,
                job.spool_path.clone(),
            )
        };
        if !all_pages.iter().any(|p| p.included) {
            return Err(AgentError::NothingIncluded);
        }
        let partial = all_pages.iter().any(|p| !p.included);
        // Pre-flight: only formats with a normalization path may cancel the
        // original. Anything else fails here, before touching the spooler —
        // conversion problems at runtime (missing GhostPCL, no GDI) keep the
        // entry pending instead, via `attempt_reprint`.
        if partial
            && !matches!(
                format,
                Some(
                    SpoolFormat::Pdf
                        | SpoolFormat::PostScript
                        | SpoolFormat::Xps
                        | SpoolFormat::Emf
                        | SpoolFormat::Pcl
                )
            )
        {
            return Err(AgentError::SubsetUnsupported(
                format.unwrap_or(SpoolFormat::Unknown),
            ));
        }

        let completion = if partial {
            self.release_spooled_job(printer, job_id, true)?;
            Completion::CancelledPartial
        } else {
            self.release_spooled_job(printer, job_id, false)?;
            Completion::ResumedAll
        };
        let job = self.job_mut(printer, job_id)?;
        job.state = transition(job.state, JobEvent::Resumed).expect("paused completes legally");
        let entry = OutboxEntry {
            idempotency_key: format!("{}-{}-{}", printer, job_id, self.version),
            printer: printer.into(),
            job_id,
            target,
            pages: all_pages
                .into_iter()
                .map(|p| OutboxPage {
                    page_no: p.page_no,
                    coverage: p.coverage,
                    tier_id: p.tier_id,
                    included: p.included,
                })
                .collect(),
            format,
            spool_path,
            reprint_pending: completion == Completion::CancelledPartial,
            synced: false,
            materialized: false,
            remote_id: None,
        };
        // The subset reprint runs OUTSIDE the core lock (see the runtime
        // sweeper): Ghostscript + printer IO take seconds, and the UI thread
        // must keep getting the lock every 500 ms.
        self.outbox.push(entry);
        self.bump();
        crate::audit::record(
            "completed",
            printer,
            Some(job_id),
            format!("{completion:?}"),
        );
        Ok(completion)
    }

    /// Cancel or resume the held job in the spooler, tolerating a job the
    /// spooler already dropped.
    ///
    /// The operator's analysis is the billable record; it must not be lost
    /// because the queue forgot the job (printer restarted, job purged, virtual
    /// printer consumed it). A real permission error still aborts.
    fn release_spooled_job(
        &self,
        printer: &str,
        job_id: u32,
        cancel: bool,
    ) -> Result<(), AgentError> {
        let result = if cancel {
            self.backend.cancel_job(printer, job_id)
        } else {
            self.backend.resume_job(printer, job_id)
        };
        match result {
            Ok(()) => Ok(()),
            Err(SpoolError::JobNotFound(_, _)) => {
                crate::log::warn(format!(
                    "release {printer} job {job_id}: ya no está en el spooler; se registra el análisis igual"
                ));
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    #[cfg(test)]
    pub fn test_set_pages(&mut self, printer: &str, job_id: u32, pages: Vec<WatchedPage>) {
        if let Ok(job) = self.job_mut(printer, job_id) {
            job.pages = pages;
        }
    }

    #[cfg(test)]
    pub fn test_set_format(&mut self, printer: &str, job_id: u32, format: SpoolFormat) {
        if let Ok(job) = self.job_mut(printer, job_id) {
            job.format = Some(format);
        }
    }

    pub fn cancel_job(&mut self, printer: &str, job_id: u32) -> Result<(), AgentError> {
        self.backend.cancel_job(printer, job_id)?;
        let job = self.job_mut(printer, job_id)?;
        job.state = transition(job.state, JobEvent::Cancelled).expect("paused cancels legally");
        cleanup_spool_file(job.spool_path.as_deref());
        job.spool_path = None;
        self.bump();
        crate::audit::record("cancelled", printer, Some(job_id), String::new());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tickets::MockTickets;
    use mpa_spool::MockSpool;

    fn tier(id: &str, min: f32, max: f32) -> Tier {
        Tier {
            id: id.into(),
            min_coverage: min,
            max_coverage: max,
            active: true,
        }
    }

    fn core() -> AgentCore {
        let backend: Arc<dyn SpoolBackend> = Arc::new(MockSpool::with_demo_jobs());
        AgentCore::new(
            backend,
            Arc::new(MockTickets::demo()),
            vec![tier("t6", 0.0, 50.0), tier("t10", 50.01, 90.0)],
        )
    }

    /// Backend with scripted pause/list behavior for race testing.
    struct FlakyBackend {
        pause_calls: std::sync::Mutex<u32>,
        fail_pauses: u32,
        vanish: bool,
        never_holds: bool,
    }

    impl FlakyBackend {
        fn printing_job() -> mpa_spool::JobInfo {
            mpa_spool::JobInfo {
                id: 7,
                document: "race.pdf".into(),
                status_text: "PRINTING".into(),
                total_pages: 3,
            }
        }
    }

    impl mpa_spool::SpoolBackend for FlakyBackend {
        fn list_printers(&self) -> Result<Vec<mpa_spool::PrinterInfo>, mpa_spool::SpoolError> {
            Ok(vec![])
        }
        fn list_jobs(
            &self,
            _printer: &str,
        ) -> Result<Vec<mpa_spool::JobInfo>, mpa_spool::SpoolError> {
            if self.vanish {
                return Ok(vec![]);
            }
            let mut job = Self::printing_job();
            if !self.never_holds {
                job.status_text = "PAUSED".into();
            }
            Ok(vec![job])
        }
        fn pause_job(&self, printer: &str, job_id: u32) -> Result<(), mpa_spool::SpoolError> {
            let mut calls = self.pause_calls.lock().unwrap();
            *calls += 1;
            if *calls <= self.fail_pauses {
                return Err(mpa_spool::SpoolError::Backend("spooler busy".into()));
            }
            if self.vanish {
                return Err(mpa_spool::SpoolError::JobNotFound(job_id, printer.into()));
            }
            Ok(())
        }
        fn resume_job(&self, _printer: &str, _job_id: u32) -> Result<(), mpa_spool::SpoolError> {
            Ok(())
        }
        fn cancel_job(&self, _printer: &str, _job_id: u32) -> Result<(), mpa_spool::SpoolError> {
            Ok(())
        }
        fn wait_for_new_job(
            &self,
            _printer: &str,
            _timeout: std::time::Duration,
        ) -> Result<Option<u32>, mpa_spool::SpoolError> {
            Ok(None)
        }
    }

    fn flaky_core(backend: FlakyBackend) -> (AgentCore, Arc<FlakyBackend>) {
        let backend = Arc::new(backend);
        let core = AgentCore::new(
            backend.clone(),
            Arc::new(MockTickets::demo()),
            vec![tier("t6", 0.0, 50.0)],
        );
        (core, backend)
    }

    #[test]
    fn pause_retries_transient_failures_then_holds() {
        let (mut core, backend) = flaky_core(FlakyBackend {
            pause_calls: std::sync::Mutex::new(0),
            fail_pauses: 2,
            vanish: false,
            never_holds: false,
        });
        assert_eq!(core.detect_and_pause("P", 7).unwrap(), PauseOutcome::Paused);
        assert_eq!(*backend.pause_calls.lock().unwrap(), 3);
        assert!(core.snapshot()[0].warnings.is_empty());
    }

    #[test]
    fn pause_gives_up_after_three_attempts() {
        let (mut core, _) = flaky_core(FlakyBackend {
            pause_calls: std::sync::Mutex::new(0),
            fail_pauses: 99,
            vanish: false,
            never_holds: false,
        });
        assert!(core.detect_and_pause("P", 7).is_err());
        assert!(core.snapshot().is_empty());
    }

    #[test]
    fn vanished_job_is_missed_without_record() {
        let (mut core, _) = flaky_core(FlakyBackend {
            pause_calls: std::sync::Mutex::new(0),
            fail_pauses: 0,
            vanish: true,
            never_holds: false,
        });
        assert_eq!(core.detect_and_pause("P", 7).unwrap(), PauseOutcome::Missed);
        assert!(core.snapshot().is_empty());
    }

    #[test]
    fn unpausable_job_is_held_with_warning() {
        let (mut core, _) = flaky_core(FlakyBackend {
            pause_calls: std::sync::Mutex::new(0),
            fail_pauses: 0,
            vanish: false,
            never_holds: true,
        });
        assert_eq!(
            core.detect_and_pause("P", 7).unwrap(),
            PauseOutcome::Escaped
        );
        let jobs = core.snapshot();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].warnings.len(), 1);
    }

    #[test]
    fn detect_pause_analyze_full_flow() {
        let mut core = core();
        core.detect_and_pause("Mock-LaserJet", 41).unwrap();
        // Mock capture returns the white+1px fixture → analyzed, tier t6.
        core.analyze_job("Mock-LaserJet", 41).unwrap();
        let jobs = core.snapshot();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].state, JobState::Paused);
        assert_eq!(jobs[0].pages.len(), 1);
        assert_eq!(jobs[0].pages[0].tier_id.as_deref(), Some("t6"));
        assert!(jobs[0].analysis_error.is_none());
    }

    #[test]
    fn toggle_select_complete_resumes_and_records_outbox() {
        let mut core = core();
        core.detect_and_pause("Mock-LaserJet", 41).unwrap();
        core.analyze_job("Mock-LaserJet", 41).unwrap();
        core.select_target(
            "Mock-LaserJet",
            41,
            TargetSelection::Ticket("order-101".into()),
        )
        .unwrap();
        let completion = core.complete_job("Mock-LaserJet", 41).unwrap();
        assert_eq!(completion, Completion::ResumedAll);
        assert_eq!(core.snapshot()[0].state, JobState::Released);
        let entry = &core.outbox()[0];
        assert_eq!(entry.target, TargetSelection::Ticket("order-101".into()));
        assert!(!entry.reprint_pending);
        assert!(!entry.synced);
        assert_eq!(entry.pages.len(), 1);
        assert!(entry.pages[0].included);
    }

    /// A job the spooler already dropped must still record its draft: the
    /// analysis is the billable record and the queue forgets jobs (printer
    /// restarted, Purge, virtual printer that consumed it).
    #[test]
    fn completion_survives_a_job_the_spooler_already_dropped() {
        let backend = Arc::new(MockSpool::with_demo_jobs());
        let mut core = AgentCore::new(
            Arc::clone(&backend) as Arc<dyn SpoolBackend>,
            Arc::new(MockTickets::demo()),
            vec![tier("t6", 0.0, 50.0)],
        );
        core.detect_and_pause("Mock-LaserJet", 41).unwrap();
        core.analyze_job("Mock-LaserJet", 41).unwrap();
        core.select_target(
            "Mock-LaserJet",
            41,
            TargetSelection::Ticket("order-101".into()),
        )
        .unwrap();
        // The job disappears from the queue before the operator completes it.
        backend.cancel_job("Mock-LaserJet", 41).unwrap();

        let completion = core.complete_job("Mock-LaserJet", 41).unwrap();
        assert_eq!(completion, Completion::ResumedAll);
        assert_eq!(core.snapshot()[0].state, JobState::Released);
        assert_eq!(core.outbox().len(), 1);
        assert_eq!(
            core.outbox()[0].target,
            TargetSelection::Ticket("order-101".into())
        );
    }

    #[test]
    fn excluded_pages_make_completion_partial() {
        let mut core = core();
        core.detect_and_pause("Mock-LaserJet", 41).unwrap();
        core.analyze_job("Mock-LaserJet", 41).unwrap();
        // Single-page job: exclude it → nothing to complete.
        core.toggle_page("Mock-LaserJet", 41, 1).unwrap();
        assert!(matches!(
            core.complete_job("Mock-LaserJet", 41),
            Err(AgentError::NothingIncluded)
        ));
    }

    #[test]
    fn partial_selection_cancels_and_flags_reprint() {
        use mpa_domain::Coverage;
        let mut core = core();
        core.detect_and_pause("Mock-LaserJet", 41).unwrap();
        core.test_set_format("Mock-LaserJet", 41, SpoolFormat::Pdf);
        core.test_set_pages(
            "Mock-LaserJet",
            41,
            vec![
                WatchedPage {
                    page_no: 1,
                    coverage: Coverage::new(80.0, 80.0, 0.0, 0.0),
                    tier_id: Some("t10".into()),
                    included: true,
                    thumbnail_png: Vec::new(),
                },
                WatchedPage {
                    page_no: 2,
                    coverage: Coverage::new(0.0, 0.0, 0.0, 3.0),
                    tier_id: Some("t6".into()),
                    included: false,
                    thumbnail_png: Vec::new(),
                },
            ],
        );
        let completion = core.complete_job("Mock-LaserJet", 41).unwrap();
        assert_eq!(completion, Completion::CancelledPartial);
        let entry = &core.outbox()[0];
        // No cached spool bytes in this path (pages were injected), so the
        // auto-reprint cannot run and the entry stays pending.
        assert!(entry.reprint_pending);
        assert_eq!(entry.pages.len(), 2);
        assert_eq!(entry.pages.iter().filter(|p| p.included).count(), 1);
        assert!(!entry.synced);
    }

    #[test]
    fn partial_completion_refuses_unsupported_format_before_cancelling() {
        use mpa_domain::Coverage;
        let mut core = core();
        core.detect_and_pause("Mock-LaserJet", 41).unwrap();
        core.test_set_format("Mock-LaserJet", 41, SpoolFormat::Unknown);
        core.test_set_pages(
            "Mock-LaserJet",
            41,
            vec![
                WatchedPage {
                    page_no: 1,
                    coverage: Coverage::new(80.0, 80.0, 0.0, 0.0),
                    tier_id: Some("t10".into()),
                    included: true,
                    thumbnail_png: Vec::new(),
                },
                WatchedPage {
                    page_no: 2,
                    coverage: Coverage::new(0.0, 0.0, 0.0, 3.0),
                    tier_id: Some("t6".into()),
                    included: false,
                    thumbnail_png: Vec::new(),
                },
            ],
        );
        assert!(matches!(
            core.complete_job("Mock-LaserJet", 41),
            Err(AgentError::SubsetUnsupported(SpoolFormat::Unknown))
        ));
        // The spool job is untouched: still held, nothing recorded.
        assert_eq!(core.snapshot()[0].state, JobState::Paused);
        assert!(core.outbox().is_empty());
    }

    #[test]
    fn mode_toggle_bumps_version() {
        let mut core = core();
        let v = core.version();
        core.set_mode(AgentMode::Suspended);
        assert_eq!(core.mode(), AgentMode::Suspended);
        assert!(core.version() > v);
    }

    #[test]
    fn tickets_come_from_source() {
        let core = core();
        let tickets = core.list_open_tickets().unwrap();
        assert_eq!(tickets.len(), 2);
        assert_eq!(tickets[0].ticket_number, 101);
    }
}
