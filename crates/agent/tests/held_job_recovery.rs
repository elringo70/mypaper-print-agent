//! A held job must survive an agent restart.
//!
//! The agent pauses a job, so the spooler keeps it `PAUSED` until somebody
//! resumes or cancels it. `wait_for_new_job` only fires on ADD_JOB/SET_JOB, so
//! a job paused by an earlier process is never reported again: without a
//! recovery scan the window opens empty while the printer stays blocked and
//! the operator has nothing to release it with.

use mpa_agent::{AgentCore, AgentRuntime, MockTickets};
use mpa_domain::{JobState, PolicyMode, PrinterPolicy, Tier};
use mpa_spool::{JobInfo, PrinterInfo, SpoolBackend, SpoolError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Spooler holding one PAUSED job that never shows up as a new arrival.
#[derive(Default)]
struct HeldJobBackend {
    resumed: Mutex<Vec<u32>>,
}

impl SpoolBackend for HeldJobBackend {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError> {
        Ok(vec![PrinterInfo {
            name: "Held-Print".into(),
            status: "READY".into(),
        }])
    }

    fn list_jobs(&self, _printer: &str) -> Result<Vec<JobInfo>, SpoolError> {
        Ok(vec![JobInfo {
            id: 7,
            document: "factura-cliente.pdf".into(),
            status_text: "PAUSED".into(),
            total_pages: 3,
        }])
    }

    fn pause_job(&self, _printer: &str, _job_id: u32) -> Result<(), SpoolError> {
        Ok(())
    }

    fn resume_job(&self, _printer: &str, job_id: u32) -> Result<(), SpoolError> {
        self.resumed.lock().unwrap().push(job_id);
        Ok(())
    }

    fn cancel_job(&self, _printer: &str, _job_id: u32) -> Result<(), SpoolError> {
        Ok(())
    }

    fn wait_for_new_job(
        &self,
        _printer: &str,
        timeout: Duration,
    ) -> Result<Option<u32>, SpoolError> {
        // Never reports the held job: only the recovery scan can find it.
        std::thread::sleep(timeout.min(Duration::from_millis(5)));
        Ok(None)
    }
}

fn tiers() -> Vec<Tier> {
    vec![Tier {
        id: "t6".into(),
        min_coverage: 0.0,
        max_coverage: 100.0,
        active: true,
    }]
}

#[test]
fn a_job_paused_by_a_previous_run_is_recovered_at_startup() {
    let backend = Arc::new(HeldJobBackend::default());
    let core = Arc::new(Mutex::new(AgentCore::new(
        Arc::clone(&backend) as Arc<dyn SpoolBackend>,
        Arc::new(MockTickets::demo()),
        tiers(),
    )));

    let policy = PrinterPolicy {
        mode: PolicyMode::Denylist,
        watched: vec![],
        ignored: vec![],
    };
    let runtime = AgentRuntime::start(Arc::clone(&core), policy, Duration::from_millis(20));

    let deadline = Instant::now() + Duration::from_secs(5);
    while !core
        .lock()
        .unwrap()
        .snapshot()
        .iter()
        .any(|j| j.job_id == 7)
    {
        assert!(
            Instant::now() < deadline,
            "the job held by a previous run never came back to the core"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let snapshot = core.lock().unwrap().snapshot();
    let job = snapshot.iter().find(|j| j.job_id == 7).expect("held job");
    assert_eq!(job.printer, "Held-Print");
    assert_eq!(job.document, "factura-cliente.pdf");
    assert_eq!(job.state, JobState::Paused);
    // The job is handed back to the operator, not silently resumed on the
    // printer.
    assert!(backend.resumed.lock().unwrap().is_empty());

    runtime.shutdown();
}
