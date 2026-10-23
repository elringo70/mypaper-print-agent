use std::time::Duration;
use thiserror::Error;

#[cfg(windows)]
mod devmode;
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
    #[error("not supported by this backend: {0}")]
    Unsupported(String),
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

/// What the driver reports it can do. Read-only discovery: presets are built
/// with the driver's own names instead of guessing what a printer accepts.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrinterCaps {
    /// Paper size names as the driver spells them (`A4`, `Letter`…).
    pub papers: Vec<String>,
    /// Media type names — the driver's own list, which is where paper
    /// thickness lives (`Plain`, `Thick`, `Cardstock`…).
    pub media_types: Vec<String>,
    /// Input bins / trays.
    pub bins: Vec<String>,
    /// Duplex modes the driver supports; 0 means single-sided only.
    pub duplex_modes: u32,
    /// Whether the driver can print in color.
    pub color: bool,
}

/// The print settings a held job carries in its DEVMODE. Names (not driver
/// codes) so they can be shown, stored in `config.toml` and mapped back.
///
/// Fields are `Option` because "not requested" and "requested as false" are
/// different things.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paper: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplex: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<bool>,
}

impl From<&mpa_domain::PrintOptions> for JobSettings {
    /// What a print preset means to the spooler. `width_points`/`height_points`
    /// have no DEVMODE equivalent here (they are a Ghostscript reprint concern),
    /// so only the named paper size and the media type travel.
    fn from(options: &mpa_domain::PrintOptions) -> Self {
        Self {
            paper: options.paper.clone(),
            media_type: options.media_type.clone(),
            duplex: if options.duplex { Some(true) } else { None },
            color: None,
        }
    }
}

impl JobSettings {
    /// Whether the spooler's answer matches what was requested. Only the fields
    /// the caller asked for are compared: a request that leaves `color` alone
    /// must not be judged by the job's current color setting.
    ///
    /// This is the guard that makes "apply in place" safe: a driver can accept
    /// `SetJob` and still keep the old values (verified against a Konica
    /// Minolta PCL queue), and trusting our own request would print on the
    /// wrong paper without a word.
    pub fn satisfies(&self, requested: &JobSettings) -> bool {
        let matches = |want: &Option<f32>, got: &Option<String>, same: fn(&str, f32) -> bool| match (
            want, got,
        ) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(value), Some(text)) => same(text, *value),
        };
        let _ = matches;
        if let Some(want) = &requested.paper {
            match &self.paper {
                Some(got) if got.eq_ignore_ascii_case(want) => {}
                _ => return false,
            }
        }
        if let Some(want) = &requested.media_type {
            match &self.media_type {
                Some(got) if got.eq_ignore_ascii_case(want) => {}
                _ => return false,
            }
        }
        if let Some(want) = requested.duplex {
            if self.duplex != Some(want) {
                return false;
            }
        }
        if let Some(want) = requested.color {
            if self.color != Some(want) {
                return false;
            }
        }
        true
    }
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

    /// Paper sizes, media types, trays and duplex/color support the driver
    /// reports. Used to build presets with real names.
    fn printer_caps(&self, printer: &str) -> Result<PrinterCaps, SpoolError> {
        Err(SpoolError::Unsupported(format!(
            "printer caps on {printer}"
        )))
    }

    /// The settings a held job carries today (its DEVMODE).
    fn job_settings(&self, printer: &str, job_id: u32) -> Result<JobSettings, SpoolError> {
        Err(SpoolError::Unsupported(format!(
            "job settings on {printer} job {job_id}"
        )))
    }

    /// Change a held job's settings in place. The spooler keeps them in the
    /// job's DEVMODE, so a paused job prints with the new values when it is
    /// resumed — no cancel, no reprint.
    fn apply_job_settings(
        &self,
        printer: &str,
        job_id: u32,
        _settings: &JobSettings,
    ) -> Result<JobSettings, SpoolError> {
        Err(SpoolError::Unsupported(format!(
            "job settings on {printer} job {job_id}"
        )))
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

#[cfg(test)]
mod settings_tests {
    use super::JobSettings;

    fn settings(
        paper: Option<&str>,
        media: Option<&str>,
        duplex: Option<bool>,
        color: Option<bool>,
    ) -> JobSettings {
        JobSettings {
            paper: paper.map(str::to_string),
            media_type: media.map(str::to_string),
            duplex,
            color,
        }
    }

    #[test]
    fn only_the_requested_fields_are_checked() {
        // The job reports color even though nobody asked about it: that must not
        // invalidate a paper change.
        let read_back = settings(Some("A4"), Some("Papel normal"), Some(false), Some(true));
        assert!(read_back.satisfies(&settings(Some("A4"), None, None, None)));
        assert!(read_back.satisfies(&settings(None, None, Some(false), None)));
        assert!(read_back.satisfies(&JobSettings::default()));
        // Case differences are the driver's spelling, not a failure.
        assert!(read_back.satisfies(&settings(Some("a4"), None, None, None)));
    }

    #[test]
    fn a_spooler_that_ignores_the_change_does_not_satisfy_the_request() {
        // This is exactly what the Konica queue did: SetJob answered success and
        // kept the original DEVMODE.
        let read_back = settings(
            Some("8 1/2x11"),
            Some("Papel normal"),
            Some(false),
            Some(true),
        );
        assert!(!read_back.satisfies(&settings(Some("A4"), None, None, None)));
        assert!(!read_back.satisfies(&settings(None, Some("Grosor 2"), None, None)));
        assert!(!read_back.satisfies(&settings(None, None, Some(true), None)));
        // A field the job does not report cannot be confirmed either.
        let unknown = settings(None, None, None, None);
        assert!(!unknown.satisfies(&settings(Some("A4"), None, None, None)));
    }
}
