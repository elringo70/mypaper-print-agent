use std::time::Duration;
use thiserror::Error;

#[cfg(windows)]
mod windows_backend;
#[cfg(windows)]
pub use windows_backend::WindowsSpool;

/// In-memory spooler. Unconditional (not `#[cfg(not(windows))]`): it is pure
/// bookkeeping with no OS calls, the crate's own unit tests and the agent's
/// rely on it, and the Windows test target needs it to compile at all.
/// `default_backend` still hands out the real WinSpool on Windows.
mod mock_backend;
pub use mock_backend::MockSpool;

#[derive(Debug, Error)]
pub enum SpoolError {
    #[error("printer not found: {0}")]
    PrinterNotFound(String),
    #[error("job {0} not found on {1}")]
    JobNotFound(u32, String),
    #[error("spooler access denied (run elevated): {0}")]
    AccessDenied(String),
    #[error("backend error: {0}")]
    Backend(String),
}

#[derive(Debug, Clone)]
pub struct PrinterInfo {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct JobInfo {
    pub id: u32,
    pub document: String,
    pub status_text: String,
    pub total_pages: u32,
}

/// Platform spooler access. Phase 1 covers detection + pause/resume/cancel;
/// spool-file capture and reprint land in Phase 2.
pub trait SpoolBackend: Send + Sync {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError>;
    fn list_jobs(&self, printer: &str) -> Result<Vec<JobInfo>, SpoolError>;
    fn pause_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError>;
    fn resume_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError>;
    fn cancel_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError>;
    /// Blocks up to `timeout` waiting for a new job on `printer`.
    /// Returns the job id when one arrives, `None` on timeout.
    fn wait_for_new_job(&self, printer: &str, timeout: Duration)
        -> Result<Option<u32>, SpoolError>;
    /// Whether the printer is a network connection (spool may live on the
    /// print server instead of this PC). Defaults to `false`; the Windows
    /// backend reads the real `PRINTER_ATTRIBUTE_NETWORK` flag.
    fn is_network_printer(&self, _printer: &str) -> Result<bool, SpoolError> {
        Ok(false)
    }
}

/// Platform backend: real WinSpool on Windows, in-memory mock elsewhere
/// (Linux dev and CI).
pub fn default_backend() -> Box<dyn SpoolBackend> {
    #[cfg(windows)]
    return Box::new(WindowsSpool);
    #[cfg(not(windows))]
    return Box::new(MockSpool::with_demo_jobs());
}
