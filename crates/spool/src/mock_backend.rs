use super::{JobInfo, PrinterInfo, SpoolBackend, SpoolError};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Default)]
struct MockState {
    jobs: HashMap<String, Vec<MockJob>>,
    /// Scripted arrivals consumed by `wait_for_new_job` (demo + tests).
    incoming: VecDeque<(String, MockJob)>,
}

#[derive(Debug, Clone)]
struct MockJob {
    id: u32,
    document: String,
    paused: bool,
    cancelled: bool,
}

/// In-memory spooler for Linux development and tests. Mirrors the
/// `SpoolBackend` contract so CLI flows run without Windows.
#[derive(Debug, Clone, Default)]
pub struct MockSpool {
    state: Arc<Mutex<MockState>>,
}

impl MockSpool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Demo printers plus one scripted arrival, so `mpa watch` shows the
    /// detect → pause flow on any OS.
    pub fn with_demo_jobs() -> Self {
        let spool = Self::new();
        {
            let mut state = spool.state.lock().expect("mock state");
            state.jobs.insert(
                "Mock-LaserJet".into(),
                vec![MockJob {
                    id: 41,
                    document: "lista-precios.xlsx".into(),
                    paused: false,
                    cancelled: false,
                }],
            );
            state.jobs.insert("Mock-Ticket".into(), vec![]);
            state.incoming.push_back((
                "Mock-LaserJet".into(),
                MockJob {
                    id: 42,
                    document: "tarea-historia.pdf".into(),
                    paused: false,
                    cancelled: false,
                },
            ));
        }
        spool
    }

    fn find_job_mut<'a>(
        state: &'a mut MockState,
        printer: &str,
        job_id: u32,
    ) -> Result<&'a mut MockJob, SpoolError> {
        state
            .jobs
            .get_mut(printer)
            .ok_or_else(|| SpoolError::PrinterNotFound(printer.into()))?
            .iter_mut()
            .find(|j| j.id == job_id && !j.cancelled)
            .ok_or_else(|| SpoolError::JobNotFound(job_id, printer.into()))
    }
}

impl SpoolBackend for MockSpool {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError> {
        let state = self.state.lock().expect("mock state");
        let mut printers: Vec<PrinterInfo> = state
            .jobs
            .keys()
            .map(|name| PrinterInfo {
                name: name.clone(),
                status: "READY".into(),
            })
            .collect();
        printers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(printers)
    }

    fn list_jobs(&self, printer: &str) -> Result<Vec<JobInfo>, SpoolError> {
        let state = self.state.lock().expect("mock state");
        let jobs = state
            .jobs
            .get(printer)
            .ok_or_else(|| SpoolError::PrinterNotFound(printer.into()))?;
        Ok(jobs
            .iter()
            .filter(|j| !j.cancelled)
            .map(|j| JobInfo {
                id: j.id,
                document: j.document.clone(),
                status_text: if j.paused {
                    "PAUSED".into()
                } else {
                    "PRINTING".into()
                },
                total_pages: 0,
            })
            .collect())
    }

    fn pause_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError> {
        let mut state = self.state.lock().expect("mock state");
        Self::find_job_mut(&mut state, printer, job_id)?.paused = true;
        Ok(())
    }

    fn resume_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError> {
        let mut state = self.state.lock().expect("mock state");
        Self::find_job_mut(&mut state, printer, job_id)?.paused = false;
        Ok(())
    }

    fn cancel_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError> {
        let mut state = self.state.lock().expect("mock state");
        Self::find_job_mut(&mut state, printer, job_id)?.cancelled = true;
        Ok(())
    }

    fn wait_for_new_job(
        &self,
        printer: &str,
        timeout: Duration,
    ) -> Result<Option<u32>, SpoolError> {
        if !self
            .state
            .lock()
            .expect("mock state")
            .jobs
            .contains_key(printer)
        {
            return Err(SpoolError::PrinterNotFound(printer.into()));
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            {
                let mut state = self.state.lock().expect("mock state");
                if let Some(pos) = state.incoming.iter().position(|(p, _)| p == printer) {
                    let (_, job) = state.incoming.remove(pos).expect("incoming position");
                    let id = job.id;
                    state.jobs.entry(printer.into()).or_default().push(job);
                    return Ok(Some(id));
                }
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_resume_cancel_flow() {
        let spool = MockSpool::with_demo_jobs();
        spool.pause_job("Mock-LaserJet", 41).unwrap();
        let jobs = spool.list_jobs("Mock-LaserJet").unwrap();
        assert!(jobs.iter().any(|j| j.id == 41 && j.status_text == "PAUSED"));
        spool.resume_job("Mock-LaserJet", 41).unwrap();
        spool.cancel_job("Mock-LaserJet", 41).unwrap();
        let jobs = spool.list_jobs("Mock-LaserJet").unwrap();
        assert!(!jobs.iter().any(|j| j.id == 41));
    }

    #[test]
    fn scripted_arrival_and_unknown_printer() {
        let spool = MockSpool::with_demo_jobs();
        let id = spool
            .wait_for_new_job("Mock-LaserJet", Duration::from_secs(5))
            .unwrap();
        assert_eq!(id, Some(42));
        let timeout = spool
            .wait_for_new_job("Mock-Ticket", Duration::from_millis(100))
            .unwrap();
        assert_eq!(timeout, None);
        assert!(matches!(
            spool.wait_for_new_job("Nope", Duration::from_millis(10)),
            Err(SpoolError::PrinterNotFound(_))
        ));
    }
}
