//! Lock discipline of the watcher runtime.
//!
//! The UI thread polls `AgentCore` behind its mutex every 500 ms; a guard held
//! across spooler or Ghostscript IO freezes the window ("Not Responding").
//! These tests pin that invariant: while the runtime is intercepting a job,
//! another thread must stay able to take the lock.

use mpa_agent::{AgentCore, AgentRuntime, MockTickets};
use mpa_domain::{PolicyMode, PrinterPolicy, Tier};
use mpa_spool::{JobInfo, PrinterInfo, SpoolBackend, SpoolError};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Backend whose spooler calls are deliberately slow, so any guard held across
/// them is trivially observable from another thread.
struct SlowBackend {
    call_ms: u64,
    arrivals: Mutex<VecDeque<u32>>,
}

impl SlowBackend {
    fn new(call_ms: u64, arrivals: Vec<u32>) -> Self {
        Self {
            call_ms,
            arrivals: Mutex::new(arrivals.into()),
        }
    }

    fn slow(&self) {
        std::thread::sleep(Duration::from_millis(self.call_ms));
    }
}

impl SpoolBackend for SlowBackend {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError> {
        self.slow();
        Ok(vec![PrinterInfo {
            name: "Slow-Print".into(),
            status: "READY".into(),
        }])
    }

    fn list_jobs(&self, _printer: &str) -> Result<Vec<JobInfo>, SpoolError> {
        self.slow();
        Ok(vec![JobInfo {
            id: 7,
            document: "apunte-fisica.pdf".into(),
            status_text: "PAUSED".into(),
            total_pages: 2,
        }])
    }

    fn pause_job(&self, _printer: &str, _job_id: u32) -> Result<(), SpoolError> {
        self.slow();
        Ok(())
    }

    fn resume_job(&self, _printer: &str, _job_id: u32) -> Result<(), SpoolError> {
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
        // Scripted single arrival, otherwise a short empty quantum.
        if let Some(id) = self.arrivals.lock().unwrap().pop_front() {
            return Ok(Some(id));
        }
        std::thread::sleep(timeout.min(Duration::from_millis(5)));
        Ok(None)
    }
}

fn tiers() -> Vec<Tier> {
    vec![Tier {
        id: "t6".into(),
        mode: mpa_domain::PrintMode::Color,
        min_coverage: 0.0,
        max_coverage: 100.0,
        active: true,
    }]
}

/// Watches the core mutex for `window` while the runtime works, and returns
/// the longest a single acquisition had to wait.
fn worst_lock_wait(core: &Arc<Mutex<AgentCore>>, window: Duration) -> Duration {
    let mut worst = Duration::ZERO;
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        let started = Instant::now();
        let guard = core.lock().unwrap();
        worst = worst.max(started.elapsed());
        drop(guard);
        std::thread::sleep(Duration::from_millis(5));
    }
    worst
}

#[test]
fn runtime_never_holds_the_core_lock_across_spooler_io() {
    const CALL_MS: u64 = 300;
    let backend = Arc::new(SlowBackend::new(CALL_MS, vec![7]));
    let core = AgentCore::new(
        Arc::clone(&backend) as Arc<dyn SpoolBackend>,
        Arc::new(MockTickets::demo()),
        tiers(),
    );
    let core = Arc::new(Mutex::new(core));

    let policy = PrinterPolicy {
        mode: PolicyMode::Denylist,
        watched: vec![],
        ignored: vec![],
    };
    let runtime = AgentRuntime::start(Arc::clone(&core), policy, Duration::from_millis(20));

    let worst = worst_lock_wait(&core, Duration::from_millis(1500));
    runtime.shutdown();

    // The job was intercepted (proves the runtime actually exercised the slow
    // spooler path)...
    assert_eq!(core.lock().unwrap().snapshot().len(), 1);
    // ...and the lock never stayed taken anywhere near one spooler call.
    assert!(
        worst < Duration::from_millis(CALL_MS / 3),
        "core lock blocked for {worst:?} while the runtime was intercepting: \
         a {} ms spooler call must not be inside the guard",
        CALL_MS
    );
}

#[test]
fn holding_the_lock_across_detect_and_pause_is_observable() {
    // Counter-example kept honest: the direct-caller helper deliberately spans
    // the spooler IO, which is exactly why the UI runtime does not use it.
    const CALL_MS: u64 = 200;
    let backend = Arc::new(SlowBackend::new(CALL_MS, vec![]));
    let core = Arc::new(Mutex::new(AgentCore::new(
        backend,
        Arc::new(MockTickets::demo()),
        tiers(),
    )));

    let worker = {
        let core = Arc::clone(&core);
        std::thread::spawn(move || {
            core.lock()
                .unwrap()
                .detect_and_pause("Slow-Print", 7)
                .unwrap()
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    let _guard = core.lock().unwrap();
    let waited = started.elapsed();
    let _ = worker.join();
    assert!(
        waited >= Duration::from_millis(CALL_MS / 2),
        "expected to observe the guard spanning the pause (waited {waited:?})"
    );
}
