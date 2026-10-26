//! Printer roles (Fase 3): the operator's B/N | Color choice decides which
//! printer the pages come out of. When the role points at a different printer
//! than the one that received the job, the held job cannot be resumed — it must
//! be cancelled and reprinted there, even when every page is included.

use mpa_agent::{AgentCore, MockTickets, OutboxEntry, WatchedJob, WatchedPage};
use mpa_analyzer::SpoolFormat;
use mpa_domain::{Coverage, JobState, PrintMode, PrinterRoles, Tier};
use mpa_spool::{JobInfo, PrinterInfo, SpoolBackend, SpoolError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Two installed printers: a mono laser and a color jet.
#[derive(Default)]
struct TwoPrinterBackend {
    cancelled: Mutex<Vec<u32>>,
    resumed: Mutex<Vec<u32>>,
}

impl SpoolBackend for TwoPrinterBackend {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError> {
        Ok(vec![
            PrinterInfo {
                name: "Mono-Laser".into(),
                status: "READY".into(),
            },
            PrinterInfo {
                name: "Color-Jet".into(),
                status: "READY".into(),
            },
        ])
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
    vec![
        Tier {
            id: "bw100".into(),
            mode: PrintMode::Bw,
            min_coverage: 0.0,
            max_coverage: 100.0,
            active: true,
        },
        Tier {
            id: "color100".into(),
            mode: PrintMode::Color,
            min_coverage: 0.0,
            max_coverage: 100.0,
            active: true,
        },
    ]
}

/// Core holding one PAUSED, fully analyzed job on `printer`.
fn core_with_paused_job(
    printer: &str,
    format: Option<SpoolFormat>,
    include: &[bool],
) -> (Arc<Mutex<AgentCore>>, Arc<TwoPrinterBackend>) {
    let backend = Arc::new(TwoPrinterBackend::default());
    let mut core = AgentCore::new(
        Arc::clone(&backend) as Arc<dyn SpoolBackend>,
        Arc::new(MockTickets::demo()),
        tiers(),
    );
    let pages = include
        .iter()
        .enumerate()
        .map(|(i, included)| WatchedPage {
            area: None,
            page_no: i as u32 + 1,
            coverage: Coverage::new(10.0, 0.0, 0.0, 20.0),
            tier_id: Some("color100".into()),
            included: *included,
            thumbnail_png: Vec::new(),
        })
        .collect();
    core.adopt_job(WatchedJob {
        printer: printer.into(),
        job_id: 42,
        document: "apunte.docx".into(),
        state: JobState::Paused,
        pages,
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

fn outbox_entry(core: &Arc<Mutex<AgentCore>>) -> OutboxEntry {
    core.lock()
        .unwrap()
        .outbox()
        .last()
        .cloned()
        .expect("completing a job queues an outbox entry")
}

#[test]
fn the_role_printer_receives_the_job_even_with_every_page_included() {
    let (core, backend) = core_with_paused_job("Mono-Laser", Some(SpoolFormat::Pdf), &[true, true]);
    {
        let mut c = core.lock().unwrap();
        c.set_roles(PrinterRoles {
            bw: Some("Mono-Laser".into()),
            color: Some("Color-Jet".into()),
        });
        c.set_print_mode("Mono-Laser", 42, PrintMode::Color)
            .expect("job is held");
    }

    let completion = core
        .lock()
        .unwrap()
        .complete_job("Mono-Laser", 42)
        .expect("the color role can be honored");

    // Cancelled on the original printer, reprinted on the role's.
    assert_eq!(completion, mpa_agent::Completion::CancelledPartial);
    assert_eq!(backend.cancelled.lock().unwrap().as_slice(), &[42]);
    assert!(
        backend.resumed.lock().unwrap().is_empty(),
        "a rerouted job must not be resumed on the original printer"
    );
    let entry = outbox_entry(&core);
    assert_eq!(entry.deliver_printer.as_deref(), Some("Color-Jet"));
    assert_eq!(entry.mode, PrintMode::Color);
    assert!(entry.reprint_pending);
    // Every page still travels: rerouting is not a subset.
    assert_eq!(entry.pages.len(), 2);
    assert!(entry.pages.iter().all(|p| p.included));
}

#[test]
fn the_default_mode_keeps_the_job_on_the_receiving_printer() {
    let (core, backend) = core_with_paused_job("Mono-Laser", Some(SpoolFormat::Pdf), &[true, true]);
    core.lock().unwrap().set_roles(PrinterRoles {
        bw: Some("Mono-Laser".into()),
        color: Some("Color-Jet".into()),
    });

    // No mode was chosen: black and white is the default, and the B/N role is
    // the printer that already holds the job.
    let completion = core
        .lock()
        .unwrap()
        .complete_job("Mono-Laser", 42)
        .expect("resume path");

    assert_eq!(completion, mpa_agent::Completion::ResumedAll);
    assert_eq!(backend.resumed.lock().unwrap().as_slice(), &[42]);
    assert!(backend.cancelled.lock().unwrap().is_empty());
    let entry = outbox_entry(&core);
    assert_eq!(entry.mode, PrintMode::Bw);
    assert_eq!(entry.deliver_printer.as_deref(), Some("Mono-Laser"));
    assert!(!entry.reprint_pending);
}

#[test]
fn without_an_assigned_role_the_original_printer_is_kept() {
    let (core, backend) = core_with_paused_job("Mono-Laser", Some(SpoolFormat::Pdf), &[true, true]);
    core.lock().unwrap().set_roles(PrinterRoles::default());

    let completion = core
        .lock()
        .unwrap()
        .complete_job("Mono-Laser", 42)
        .expect("resume path");

    assert_eq!(completion, mpa_agent::Completion::ResumedAll);
    assert_eq!(backend.resumed.lock().unwrap().as_slice(), &[42]);
    assert_eq!(outbox_entry(&core).deliver_printer, None);
}

#[test]
fn a_reroute_without_readable_spool_bytes_fails_before_touching_the_spooler() {
    // The subset path needs the original bytes; without them the agent cannot
    // deliver to another printer, so it refuses instead of losing the job.
    let (core, backend) = core_with_paused_job("Mono-Laser", None, &[true, true]);
    {
        let mut c = core.lock().unwrap();
        c.set_roles(PrinterRoles {
            bw: Some("Mono-Laser".into()),
            color: Some("Color-Jet".into()),
        });
        c.set_print_mode("Mono-Laser", 42, PrintMode::Color)
            .expect("job is held");
    }

    let err = core.lock().unwrap().complete_job("Mono-Laser", 42);
    assert!(
        matches!(err, Err(mpa_agent::AgentError::SubsetUnsupported(_))),
        "expected SubsetUnsupported, got {err:?}"
    );
    assert!(
        backend.cancelled.lock().unwrap().is_empty() && backend.resumed.lock().unwrap().is_empty(),
        "the spooler must not be touched when the delivery cannot be prepared"
    );
    assert!(
        core.lock().unwrap().outbox().is_empty(),
        "nothing may be queued for a delivery that failed"
    );
}

#[test]
fn a_subset_still_reroutes_to_the_role_printer() {
    let (core, backend) =
        core_with_paused_job("Mono-Laser", Some(SpoolFormat::Pdf), &[true, false]);
    {
        let mut c = core.lock().unwrap();
        c.set_roles(PrinterRoles {
            bw: Some("Mono-Laser".into()),
            color: Some("Color-Jet".into()),
        });
        c.set_print_mode("Mono-Laser", 42, PrintMode::Color)
            .expect("job is held");
    }

    let completion = core
        .lock()
        .unwrap()
        .complete_job("Mono-Laser", 42)
        .expect("partial reroute");

    assert_eq!(completion, mpa_agent::Completion::CancelledPartial);
    assert_eq!(backend.cancelled.lock().unwrap().as_slice(), &[42]);
    let entry = outbox_entry(&core);
    assert_eq!(entry.deliver_printer.as_deref(), Some("Color-Jet"));
    assert_eq!(entry.pages.iter().filter(|p| p.included).count(), 1);
}
