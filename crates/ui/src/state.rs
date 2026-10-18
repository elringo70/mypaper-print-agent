use gpui::{App, Context, Entity};
use mpa_agent::{AgentCore, AgentError, AgentMode, Completion, TargetSelection, Ticket};
use mpa_domain::{Coverage, JobState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Render-friendly mirror of the core. Thumbnails ride in `Arc` so frames
/// only clone pointers; the mirror refreshes only when `core.version()`
/// moves (the runtime bumps it on every mutation).
#[derive(Debug)]
pub struct JobView {
    pub printer: String,
    pub job_id: u32,
    pub document: String,
    pub state: JobState,
    pub pages: Vec<PageView>,
    pub analysis_error: Option<String>,
    pub warnings: Vec<String>,
    pub target: TargetSelection,
}

#[derive(Debug)]
pub struct PageView {
    pub page_no: u32,
    pub coverage: Coverage,
    pub tier_id: Option<String>,
    pub included: bool,
    pub thumb: Arc<Vec<u8>>,
}

impl JobView {
    /// Included-page count per tier, in first-seen order.
    pub fn tier_counts(&self) -> Vec<(String, usize)> {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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

pub struct UiState {
    core: Arc<Mutex<AgentCore>>,
    state_path: std::path::PathBuf,
    tray: Option<crate::tray::Tray>,
    pub jobs: Vec<JobView>,
    pub tickets: Vec<Ticket>,
    pub selected: Option<(String, u32)>,
    /// Page shown in the single preview pane. Picking a row in the page list
    /// moves it; `sync_selected_page` keeps it pointing at a real page.
    pub selected_page: Option<u32>,
    pub version: u64,
    pub outbox_len: usize,
    pub outbox_pending: usize,
    pub mode: mpa_agent::AgentMode,
    pub update_available: Option<String>,
    pub notice: Option<String>,
}

impl std::fmt::Debug for UiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiState")
            .field("jobs", &self.jobs.len())
            .field("mode", &self.mode)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl UiState {
    pub fn new(
        core: Arc<Mutex<AgentCore>>,
        state_path: std::path::PathBuf,
        tray: Option<crate::tray::Tray>,
    ) -> Self {
        let mut state = Self {
            core,
            state_path,
            tray,
            jobs: Vec::new(),
            tickets: Vec::new(),
            selected: None,
            selected_page: None,
            version: u64::MAX,
            outbox_len: 0,
            outbox_pending: 0,
            mode: mpa_agent::AgentMode::Active,
            update_available: None,
            notice: None,
        };
        state.pull();
        state
    }

    fn pull(&mut self) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        self.version = core.version();
        self.mode = core.mode();
        self.jobs = core
            .snapshot()
            .into_iter()
            .map(|j| JobView {
                printer: j.printer,
                job_id: j.job_id,
                document: j.document,
                state: j.state,
                pages: j
                    .pages
                    .into_iter()
                    .map(|p| PageView {
                        page_no: p.page_no,
                        coverage: p.coverage,
                        tier_id: p.tier_id,
                        included: p.included,
                        thumb: Arc::new(p.thumbnail_png),
                    })
                    .collect(),
                analysis_error: j.analysis_error,
                warnings: j.warnings,
                target: j.target,
            })
            .collect();
        // In-memory read only: the source itself is a blocking HTTP call on
        // paired agents, refreshed off-thread by `spawn_ticket_refresh`.
        self.tickets = core.cached_tickets().to_vec();
        self.outbox_len = core.outbox_len();
        self.outbox_pending = core.pending_outbox();
        self.update_available = core.pending_update().map(|m| m.version);
        if let Some(notice) = core.take_notice() {
            self.notice = Some(notice);
        }
        if let Some(tray) = &self.tray {
            tray.reflect_mode(core.mode());
            for action in tray.poll_actions() {
                drop(core);
                self.run_tray_action(action);
                core = match self.core.lock() {
                    Ok(core) => core,
                    Err(_) => return,
                };
            }
        }
        drop(core);
        self.sync_selected_page();
    }

    /// Keep the preview pointing at a page of the selected job: the first one
    /// when nothing is picked yet, `None` when the job has no analysis.
    fn sync_selected_page(&mut self) {
        let pages =
            UiState::selected_job(&self.jobs, &self.selected).map(|job| job.pages.as_slice());
        match pages {
            Some(pages) if !pages.is_empty() => {
                let still_there = self
                    .selected_page
                    .is_some_and(|no| pages.iter().any(|p| p.page_no == no));
                if !still_there {
                    self.selected_page = Some(pages[0].page_no);
                }
            }
            _ => self.selected_page = None,
        }
    }

    fn run_tray_action(&mut self, action: crate::tray::TrayAction) {
        // UI-thread handlers without a Context: notify through version bump.
        match action {
            crate::tray::TrayAction::ToggleMode => {
                if let Ok(mut core) = self.core.lock() {
                    let next = match core.mode() {
                        AgentMode::Active => AgentMode::Suspended,
                        AgentMode::Suspended => AgentMode::Active,
                    };
                    core.set_mode(next);
                    core.push_notice(match next {
                        AgentMode::Active => "Intercepción activada".into(),
                        AgentMode::Suspended => "Intercepción suspendida".into(),
                    });
                }
            }
            crate::tray::TrayAction::SyncNow => self.sync_now_without_cx(),
            crate::tray::TrayAction::CheckUpdate => self.check_updates_without_cx(),
            crate::tray::TrayAction::Quit => {
                if let Ok(mut core) = self.core.lock() {
                    core.request_quit();
                }
            }
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let version = self
            .core
            .lock()
            .map(|c| c.version())
            .unwrap_or(self.version);
        if version != self.version {
            self.pull();
            // Drop the selection if its job left the store.
            if let Some((printer, job_id)) = &self.selected {
                if !self
                    .jobs
                    .iter()
                    .any(|j| &j.printer == printer && j.job_id == *job_id)
                {
                    self.selected = None;
                }
            }
            cx.notify();
        }
    }

    fn mutate(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut AgentCore) -> Result<String, AgentError>,
    ) {
        // Resolve the lock fully before `pull()` so no guard outlives this scope.
        let result: Result<Result<String, AgentError>, String> = match self.core.lock() {
            Ok(mut core) => Ok(f(&mut core)),
            Err(_) => Err("core ocupado, intenta de nuevo".into()),
        };
        match result {
            Ok(Ok(note)) => {
                self.notice = if note.is_empty() { None } else { Some(note) };
            }
            Ok(Err(e)) => self.notice = Some(e.to_string()),
            Err(e) => self.notice = Some(e),
        }
        self.pull();
        self.persist();
        cx.notify();
    }

    fn persist(&self) {
        if let Ok(core) = self.core.lock() {
            let _ = mpa_agent::save_to(&core, &self.state_path);
        }
    }

    /// Push the outbox to MyPaper + retry pending reprints, off the UI thread.
    /// Completion lands in the notice bar through the next `refresh()`.
    pub fn sync_now(&mut self, cx: &mut Context<Self>) {
        Self::spawn_sync(Arc::clone(&self.core), self.state_path.clone());
        self.notice = Some("sincronizando…".into());
        cx.notify();
    }

    /// Context-free twin for tray-menu handling (no `Context` available there).
    pub fn sync_now_without_cx(&mut self) {
        Self::spawn_sync(Arc::clone(&self.core), self.state_path.clone());
        // The thread posts the outcome via `push_notice`; the next tick shows it.
    }

    /// Poll the ticket source on a worker and mirror the result into the core.
    ///
    /// `TicketSource::list_open_tickets` is a blocking HTTP call (15 s timeout)
    /// on paired agents. It used to run inside `pull()` — on the UI thread,
    /// holding the core lock — so the window showed "Not Responding" every
    /// time it opened or a job moved. Here the lock is only ever held to read
    /// the source handle and to store the mirror.
    pub fn spawn_ticket_refresh(core: Arc<Mutex<AgentCore>>) {
        std::thread::spawn(move || loop {
            let source = core.lock().ok().map(|c| c.ticket_source());
            if let Some(source) = source {
                if let Ok(tickets) = source.list_open_tickets() {
                    if let Ok(mut core) = core.lock() {
                        core.set_cached_tickets(tickets);
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(15));
        });
    }

    fn spawn_sync(core: Arc<Mutex<AgentCore>>, path: std::path::PathBuf) {
        std::thread::spawn(move || {
            let report = match mpa_sync::PairingStore::load(None) {
                Err(e) => format!("sin enlace: {e} (usa `mpa pair <código>`)"),
                Ok(pairing) => {
                    match mpa_sync::SyncClient::new(&pairing.server_url, Some(pairing.agent_key)) {
                        Err(e) => format!("sync: {e}"),
                        Ok(client) => {
                            let report = mpa_sync::drain_outbox(&core, &client);
                            if let Ok(core) = core.lock() {
                                let _ = mpa_agent::save_to(&core, &path);
                            }
                            report.summary()
                        }
                    }
                }
            };
            if let Ok(mut core) = core.lock() {
                core.push_notice(report);
            }
        });
    }

    /// Check the release channel, off the UI thread. A newer release is
    /// staged on the core; the footer offers to install it.
    pub fn check_updates(&mut self, cx: &mut Context<Self>) {
        Self::spawn_update_check(Arc::clone(&self.core));
        self.notice = Some("buscando actualizaciones…".into());
        cx.notify();
    }

    pub fn check_updates_without_cx(&mut self) {
        Self::spawn_update_check(Arc::clone(&self.core));
    }

    fn spawn_update_check(core: Arc<Mutex<AgentCore>>) {
        std::thread::spawn(move || {
            let outcome = match mpa_sync::PairingStore::load(None) {
                Err(e) => format!("sin enlace: {e} (usa `mpa pair <código>`)"),
                Ok(pairing) => {
                    let manifest_url = std::env::var("MPA_UPDATE_MANIFEST").ok();
                    match mpa_update::check_for_update(
                        Some(&pairing.server_url),
                        manifest_url.as_deref(),
                        Some(&pairing.agent_key),
                        env!("CARGO_PKG_VERSION"),
                    ) {
                        Err(e) => format!("actualización: {e}"),
                        Ok(mpa_update::UpdateStatus::UpToDate { version }) => {
                            if let Ok(mut core) = core.lock() {
                                core.set_pending_update(None);
                            }
                            format!("estás en la última versión ({version})")
                        }
                        Ok(mpa_update::UpdateStatus::NoChannel) => {
                            "el servidor no publica releases todavía".into()
                        }
                        Ok(mpa_update::UpdateStatus::Available { manifest }) => {
                            let version = manifest.version.clone();
                            if let Ok(mut core) = core.lock() {
                                core.set_pending_update(Some(manifest));
                            }
                            format!("nueva versión disponible: {version}")
                        }
                    }
                }
            };
            if let Ok(mut core) = core.lock() {
                core.push_notice(outcome);
            }
        });
    }

    /// Download, verify and hand off to the installer, then exit. Runs off
    /// the UI thread; the process ends here on success.
    pub fn apply_update(&mut self, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        std::thread::spawn(move || {
            let outcome: Result<String, String> = (|| {
                let pairing =
                    mpa_sync::PairingStore::load(None).map_err(|e| format!("sin enlace: {e}"))?;
                let manifest = core
                    .lock()
                    .map_err(|_| "core ocupado".to_string())?
                    .pending_update()
                    .ok_or_else(|| "no hay actualización pendiente".to_string())?;
                let client =
                    mpa_sync::SyncClient::new(&pairing.server_url, Some(pairing.agent_key))
                        .map_err(|e| e.to_string())?;
                let staged = mpa_update::fetch_verified(&client, &manifest)
                    .map_err(|e| format!("descarga/verificación: {e}"))?;
                let name = staged.to_string_lossy().to_lowercase();
                if name.ends_with(".msi") || name.ends_with(".exe") {
                    mpa_update::launch_installer_and_exit(&staged, &["/VERYSILENT".into()])
                        .map_err(|e| format!("instalador: {e}"))?;
                    Ok("instalador lanzado".into())
                } else {
                    mpa_update::replace_running_exe(
                        &std::fs::read(&staged).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| format!("reemplazo: {e}"))?;
                    Ok("binario reemplazado, reinicia la app".into())
                }
            })();
            let text = outcome.unwrap_or_else(|e| e);
            if let Ok(mut core) = core.lock() {
                core.push_notice(text);
            }
        });
        self.notice = Some("descargando actualización…".into());
        cx.notify();
    }

    pub fn toggle_mode(&mut self, cx: &mut Context<Self>) {
        self.mutate(cx, |core| {
            let next = match core.mode() {
                AgentMode::Active => AgentMode::Suspended,
                AgentMode::Suspended => AgentMode::Active,
            };
            core.set_mode(next);
            Ok(String::new())
        });
    }

    pub fn select(&mut self, cx: &mut Context<Self>, printer: String, job_id: u32) {
        self.selected = Some((printer, job_id));
        // Start the preview on the job's first page.
        self.selected_page = None;
        self.sync_selected_page();
        cx.notify();
    }

    /// Show `page_no` in the single preview pane.
    pub fn select_page(&mut self, cx: &mut Context<Self>, page_no: u32) {
        if self.selected_page != Some(page_no) {
            self.selected_page = Some(page_no);
            cx.notify();
        }
    }

    pub fn toggle_page(
        &mut self,
        cx: &mut Context<Self>,
        printer: String,
        job_id: u32,
        page_no: u32,
    ) {
        self.mutate(cx, |core| {
            core.toggle_page(&printer, job_id, page_no)?;
            Ok(String::new())
        });
    }

    pub fn select_target(
        &mut self,
        cx: &mut Context<Self>,
        printer: String,
        job_id: u32,
        target: TargetSelection,
    ) {
        self.mutate(cx, |core| {
            core.select_target(&printer, job_id, target)?;
            Ok(String::new())
        });
    }

    pub fn complete(&mut self, cx: &mut Context<Self>, printer: String, job_id: u32) {
        self.mutate(cx, |core| {
            let completion = core.complete_job(&printer, job_id)?;
            Ok(match completion {
                Completion::ResumedAll => "Impresión liberada y borrador registrado".into(),
                Completion::CancelledPartial => {
                    "Original cancelado; subset reimpreso o pendiente".into()
                }
            })
        });
        // The whole point of completing a job here is that MyPaper records it, so
        // push the draft now instead of waiting for a manual "Sincronizar". The
        // worker takes the lock only briefly (`spawn_sync`) and reports back
        // through the notice bar; the footer shows what is still queued.
        if self.outbox_pending > 0 {
            let queued = self.outbox_pending;
            let done = self
                .notice
                .take()
                .unwrap_or_else(|| "Impresión liberada".into());
            Self::spawn_sync(Arc::clone(&self.core), self.state_path.clone());
            self.notice = Some(format!(
                "{done} · enviando {queued} borrador(es) a MyPaper…"
            ));
            cx.notify();
        }
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>, printer: String, job_id: u32) {
        self.mutate(cx, |core| {
            core.cancel_job(&printer, job_id)?;
            Ok("Impresión cancelada".into())
        });
    }

    pub fn dismiss_notice(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        cx.notify();
    }

    /// Consume a tray/menu quit request (called from the UI poll tick).
    pub fn take_quit_request(&mut self) -> bool {
        self.core
            .lock()
            .map(|mut core| core.take_quit_request())
            .unwrap_or(false)
    }

    /// Mirrored job count for diagnostics (no lock held by the caller).
    pub fn job_count(&self) -> usize {
        self.jobs.len()
    }

    pub fn selected_job<'a>(
        jobs: &'a [JobView],
        selected: &Option<(String, u32)>,
    ) -> Option<&'a JobView> {
        selected.as_ref().and_then(|(printer, job_id)| {
            jobs.iter()
                .find(|j| &j.printer == printer && j.job_id == *job_id)
        })
    }

    /// Page currently shown in the preview pane, if it still exists.
    pub fn preview_page<'a>(&self, job: &'a JobView) -> Option<&'a PageView> {
        let page_no = self.selected_page?;
        job.pages.iter().find(|p| p.page_no == page_no)
    }
}

/// Thin helper so views update the entity without repeating the pattern.
pub fn update_state<R>(
    state: &Entity<UiState>,
    cx: &mut App,
    f: impl FnOnce(&mut UiState, &mut Context<UiState>) -> R,
) -> R {
    state.update(cx, f)
}
