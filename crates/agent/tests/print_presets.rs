//! Print presets (Fase 4): the operator picks a preset at print time, and the
//! agent applies it by cancelling the held job and reprinting, because the
//! Windows driver's own settings cannot be rewritten in place.
//!
//! The command line the preset turns into is asserted in `mpa-domain`
//! (`options.rs`); here we prove the core routes the job and snapshots the
//! options into the outbox entry.

use mpa_agent::{AgentCore, Completion, MockTickets, OutboxEntry, WatchedJob, WatchedPage};
use mpa_analyzer::SpoolFormat;
use mpa_domain::{Coverage, JobState, PrintMode, PrintOptions, PrintPreset, PrinterRoles, Tier};
use mpa_spool::{JobInfo, JobSettings, PrinterInfo, SpoolBackend, SpoolError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Backend {
    cancelled: Mutex<Vec<u32>>,
    resumed: Mutex<Vec<u32>>,
}

impl SpoolBackend for Backend {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError> {
        Ok(vec![PrinterInfo {
            name: "Mono-Laser".into(),
            status: "READY".into(),
        }])
    }

    fn list_jobs(&self, _printer: &str) -> Result<Vec<JobInfo>, SpoolError> {
        Ok(Vec::new())
    }

    fn pause_job(&self, _printer: &str, _job_id: u32) -> Result<(), SpoolError> {
        Ok(())
    }

    fn resume_job(&self, _printer: &str, job_id: u32) -> Result<(), SpoolError> {
        self.resumed.lock().unwrap().push(job_id);
        Ok(())
    }

    fn cancel_job(&self, _printer: &str, job_id: u32) -> Result<(), SpoolError> {
        self.cancelled.lock().unwrap().push(job_id);
        Ok(())
    }

    fn wait_for_new_job(
        &self,
        _printer: &str,
        _timeout: Duration,
    ) -> Result<Option<u32>, SpoolError> {
        Ok(None)
    }
}

fn tiers() -> Vec<Tier> {
    vec![Tier {
        id: "bw100".into(),
        mode: PrintMode::Bw,
        min_coverage: 0.0,
        max_coverage: 100.0,
        active: true,
    }]
}

fn core_with_paused_job(format: Option<SpoolFormat>) -> (Arc<Mutex<AgentCore>>, Arc<Backend>) {
    let backend = Arc::new(Backend::default());
    let mut core = AgentCore::new(
        Arc::clone(&backend) as Arc<dyn SpoolBackend>,
        Arc::new(MockTickets::demo()),
        tiers(),
    );
    core.adopt_job(WatchedJob {
        printer: "Mono-Laser".into(),
        job_id: 7,
        document: "tesis.docx".into(),
        state: JobState::Paused,
        pages: vec![
            WatchedPage {
                area: None,
                page_no: 1,
                coverage: Coverage::new(0.0, 0.0, 0.0, 12.0),
                tier_id: Some("bw100".into()),
                included: true,
                thumbnail_png: Vec::new(),
            },
            WatchedPage {
                area: None,
                page_no: 2,
                coverage: Coverage::new(0.0, 0.0, 0.0, 20.0),
                tier_id: Some("bw100".into()),
                included: true,
                thumbnail_png: Vec::new(),
            },
        ],
        analysis_error: None,
        warnings: Vec::new(),
        target: mpa_agent::TargetSelection::Unassigned,
        format,
        spool_path: None,
        print_mode: PrintMode::Bw,
        preset: None,
    });
    (Arc::new(Mutex::new(core)), backend)
}

fn a4_duplex() -> PrintPreset {
    PrintPreset {
        name: "A4 doble cara".into(),
        options: PrintOptions {
            paper: Some("a4".into()),
            duplex: true,
            ..Default::default()
        },
    }
}

fn outbox_entry(core: &Arc<Mutex<AgentCore>>) -> OutboxEntry {
    core.lock()
        .unwrap()
        .outbox()
        .last()
        .cloned()
        .expect("completing a job queues an outbox entry")
}

#[test]
fn a_preset_cancels_and_reprints_even_with_every_page_included() {
    let (core, backend) = core_with_paused_job(Some(SpoolFormat::Pdf));
    {
        let mut c = core.lock().unwrap();
        c.set_roles(PrinterRoles {
            bw: Some("Mono-Laser".into()),
            ..Default::default()
        });
        c.set_presets(vec![a4_duplex()]);
        c.set_preset("Mono-Laser", 7, Some("A4 doble cara".into()))
            .expect("job is held");
    }

    let completion = core
        .lock()
        .unwrap()
        .complete_job("Mono-Laser", 7)
        .expect("a preset can be applied");

    // The driver's settings cannot change in place, so the job is reprinted.
    assert_eq!(completion, Completion::CancelledPartial);
    assert_eq!(backend.cancelled.lock().unwrap().as_slice(), &[7]);
    assert!(backend.resumed.lock().unwrap().is_empty());
    let entry = outbox_entry(&core);
    assert!(entry.reprint_pending);
    assert_eq!(
        entry.options.as_ref().and_then(|o| o.paper.as_deref()),
        Some("a4")
    );
    assert!(entry.options.as_ref().is_some_and(|o| o.duplex));
    assert_eq!(entry.pages.len(), 2, "a preset is not a page subset");
}

#[test]
fn without_a_preset_the_job_resumes_untouched() {
    let (core, backend) = core_with_paused_job(Some(SpoolFormat::Pdf));
    {
        let mut c = core.lock().unwrap();
        c.set_roles(PrinterRoles {
            bw: Some("Mono-Laser".into()),
            ..Default::default()
        });
        // Presets exist but the operator kept the driver's own settings.
        c.set_presets(vec![a4_duplex()]);
    }

    let completion = core
        .lock()
        .unwrap()
        .complete_job("Mono-Laser", 7)
        .expect("resume path");

    assert_eq!(completion, Completion::ResumedAll);
    assert_eq!(backend.resumed.lock().unwrap().as_slice(), &[7]);
    assert!(backend.cancelled.lock().unwrap().is_empty());
    let entry = outbox_entry(&core);
    assert!(entry.options.is_none());
    assert!(!entry.reprint_pending);
}

/// A backend whose spooler accepts DEVMODE changes, like the real one.
#[derive(Default)]
struct DevmodeBackend {
    cancelled: Mutex<Vec<u32>>,
    resumed: Mutex<Vec<u32>>,
    applied: Mutex<Vec<JobSettings>>,
}

impl SpoolBackend for DevmodeBackend {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError> {
        Ok(vec![PrinterInfo {
            name: "Mono-Laser".into(),
            status: "READY".into(),
        }])
    }

    fn list_jobs(&self, _printer: &str) -> Result<Vec<JobInfo>, SpoolError> {
        Ok(Vec::new())
    }

    fn pause_job(&self, _printer: &str, _job_id: u32) -> Result<(), SpoolError> {
        Ok(())
    }

    fn resume_job(&self, _printer: &str, job_id: u32) -> Result<(), SpoolError> {
        self.resumed.lock().unwrap().push(job_id);
        Ok(())
    }

    fn cancel_job(&self, _printer: &str, job_id: u32) -> Result<(), SpoolError> {
        self.cancelled.lock().unwrap().push(job_id);
        Ok(())
    }

    fn wait_for_new_job(
        &self,
        _printer: &str,
        _timeout: Duration,
    ) -> Result<Option<u32>, SpoolError> {
        Ok(None)
    }

    fn apply_job_settings(
        &self,
        _printer: &str,
        _job_id: u32,
        settings: &JobSettings,
    ) -> Result<JobSettings, SpoolError> {
        self.applied.lock().unwrap().push(settings.clone());
        // A real spooler reports the state after the change.
        Ok(settings.clone())
    }
}

fn core_with_devmode_job(
    format: Option<SpoolFormat>,
) -> (Arc<Mutex<AgentCore>>, Arc<DevmodeBackend>) {
    let backend = Arc::new(DevmodeBackend::default());
    let mut core = AgentCore::new(
        Arc::clone(&backend) as Arc<dyn SpoolBackend>,
        Arc::new(MockTickets::demo()),
        tiers(),
    );
    core.adopt_job(WatchedJob {
        printer: "Mono-Laser".into(),
        job_id: 7,
        document: "tesis.docx".into(),
        state: JobState::Paused,
        pages: vec![WatchedPage {
            area: None,
            page_no: 1,
            coverage: Coverage::new(0.0, 0.0, 0.0, 12.0),
            tier_id: Some("bw100".into()),
            included: true,
            thumbnail_png: Vec::new(),
        }],
        analysis_error: None,
        warnings: Vec::new(),
        target: mpa_agent::TargetSelection::Unassigned,
        format,
        spool_path: None,
        print_mode: PrintMode::Bw,
        preset: None,
    });
    (Arc::new(Mutex::new(core)), backend)
}

#[test]
fn a_preset_the_spooler_accepts_is_applied_in_place() {
    // No cancel, no reprint: the job keeps its pages and prints with the new
    // settings when it resumes. This is the path that makes paper thickness
    // reachable at all, since mswinpr2 cannot set it.
    let (core, backend) = core_with_devmode_job(Some(SpoolFormat::Pdf));
    {
        let mut c = core.lock().unwrap();
        c.set_presets(vec![PrintPreset {
            name: "Papel grueso".into(),
            options: PrintOptions {
                paper: Some("a4".into()),
                media_type: Some("Thick".into()),
                duplex: true,
                ..Default::default()
            },
        }]);
        c.set_preset("Mono-Laser", 7, Some("Papel grueso".into()))
            .expect("job is held");
    }

    let completion = core
        .lock()
        .unwrap()
        .complete_job("Mono-Laser", 7)
        .expect("applied in place");

    assert_eq!(completion, Completion::ResumedAll);
    assert!(
        backend.cancelled.lock().unwrap().is_empty(),
        "an in-place preset must not cancel the held job"
    );
    assert_eq!(backend.resumed.lock().unwrap().as_slice(), &[7]);
    let applied = backend.applied.lock().unwrap();
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].media_type.as_deref(), Some("Thick"));
    assert_eq!(applied[0].paper.as_deref(), Some("a4"));
    assert_eq!(applied[0].duplex, Some(true));
    let entry = outbox_entry(&core);
    assert!(!entry.reprint_pending);
    // The read-back is what gets recorded, not the request.
    assert_eq!(
        entry
            .applied_settings
            .as_ref()
            .and_then(|s| s.media_type.as_deref()),
        Some("Thick")
    );
}

#[test]
fn a_stale_preset_name_does_not_change_the_print() {
    // The operator picked a preset that was deleted from config.toml in the
    // meantime: it resolves to no options, so the job must NOT be rerouted or
    // cancelled for nothing.
    let (core, backend) = core_with_paused_job(Some(SpoolFormat::Pdf));
    {
        let mut c = core.lock().unwrap();
        c.set_presets(Vec::new());
        c.set_preset("Mono-Laser", 7, Some("preset borrado".into()))
            .expect("job is held");
    }

    let completion = core
        .lock()
        .unwrap()
        .complete_job("Mono-Laser", 7)
        .expect("resume path");

    assert_eq!(completion, Completion::ResumedAll);
    assert_eq!(backend.resumed.lock().unwrap().as_slice(), &[7]);
    assert!(outbox_entry(&core).options.is_none());
}

#[test]
fn a_preset_without_readable_spool_bytes_fails_before_touching_the_spooler() {
    let (core, backend) = core_with_paused_job(None);
    {
        let mut c = core.lock().unwrap();
        c.set_presets(vec![a4_duplex()]);
        c.set_preset("Mono-Laser", 7, Some("A4 doble cara".into()))
            .expect("job is held");
    }

    let err = core.lock().unwrap().complete_job("Mono-Laser", 7);
    assert!(
        matches!(err, Err(mpa_agent::AgentError::SubsetUnsupported(_))),
        "expected SubsetUnsupported, got {err:?}"
    );
    assert!(backend.cancelled.lock().unwrap().is_empty());
    assert!(core.lock().unwrap().outbox().is_empty());
}
