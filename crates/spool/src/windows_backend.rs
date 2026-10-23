use super::{JobInfo, JobSettings, PrinterCaps, PrinterInfo, SpoolBackend, SpoolError};
use std::time::Duration;
use windows::core::{Error as WindowsError, HRESULT, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{E_ACCESSDENIED, WAIT_OBJECT_0};
use windows::Win32::Graphics::Printing::*;
use windows::Win32::System::Threading::WaitForSingleObject;

/// Real WinSpool backend (windows 0.61 API: `PRINTER_HANDLE` + `Result<()>`).
/// Every handle is RAII-guarded; every failure maps into `SpoolError`.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsSpool;

pub(crate) struct PrinterHandle(pub(crate) PRINTER_HANDLE);

impl Drop for PrinterHandle {
    fn drop(&mut self) {
        // SAFETY: handle came from a successful OpenPrinterW.
        unsafe {
            let _ = ClosePrinter(self.0);
        }
    }
}

struct ChangeHandle(windows::Win32::Foundation::HANDLE);

impl Drop for ChangeHandle {
    fn drop(&mut self) {
        // SAFETY: handle came from a successful FindFirstPrinterChangeNotification.
        unsafe {
            let _ = FindClosePrinterChangeNotification(self.0);
        }
    }
}

fn map_error(e: WindowsError, context: &str) -> SpoolError {
    if e.code() == E_ACCESSDENIED {
        SpoolError::AccessDenied(context.into())
    } else {
        SpoolError::Backend(format!("{context}: {e}"))
    }
}

/// Open the printer asking for `PRINTER_ACCESS_ADMINISTER`, which is what
/// `SetJob` level 2 needs to replace a job's information (its DEVMODE). Without
/// it the spooler answers Win32 error 5 (access denied).
pub(crate) fn open_printer_admin(name: &str) -> Result<PrinterHandle, SpoolError> {
    let hname = HSTRING::from(name);
    let mut handle = PRINTER_HANDLE::default();
    let defaults = PRINTER_DEFAULTSW {
        pDatatype: PWSTR::null(),
        pDevMode: std::ptr::null_mut(),
        DesiredAccess: PRINTER_ACCESS_ADMINISTER,
    };
    // SAFETY: `hname` outlives the call; `handle` is a valid out pointer and
    // `defaults` lives for the duration of the call.
    unsafe { OpenPrinterW(&hname, &mut handle, Some(&defaults)) }
        .map_err(|e| map_error(e, &format!("open printer {name} for administration")))?;
    if handle.Value.is_null() {
        return Err(SpoolError::PrinterNotFound(name.into()));
    }
    Ok(PrinterHandle(handle))
}

pub(crate) fn open_printer(name: &str) -> Result<PrinterHandle, SpoolError> {
    let hname = HSTRING::from(name);
    let mut handle = PRINTER_HANDLE::default();
    // SAFETY: `hname` outlives the call; `handle` is a valid out pointer.
    unsafe { OpenPrinterW(&hname, &mut handle, None) }
        .map_err(|e| map_error(e, &format!("open printer {name}")))?;
    if handle.Value.is_null() {
        return Err(SpoolError::PrinterNotFound(name.into()));
    }
    Ok(PrinterHandle(handle))
}

fn pwstr_to_string(p: PWSTR) -> String {
    // SAFETY: spooler-owned strings are valid null-terminated UTF-16.
    unsafe { p.to_string().unwrap_or_default() }
}

fn printer_status_text(status: u32) -> String {
    if status & PRINTER_STATUS_PAUSED != 0 {
        "PAUSED".into()
    } else if status & (PRINTER_STATUS_ERROR | PRINTER_STATUS_PAPER_OUT | PRINTER_STATUS_OFFLINE)
        != 0
    {
        "ERROR".into()
    } else if status & PRINTER_STATUS_PRINTING != 0 {
        "PRINTING".into()
    } else {
        "READY".into()
    }
}

fn job_status_text(status: u32) -> String {
    if status & JOB_STATUS_PAUSED != 0 {
        "PAUSED".into()
    } else if status & (JOB_STATUS_ERROR | JOB_STATUS_PAPEROUT | JOB_STATUS_OFFLINE) != 0 {
        "ERROR".into()
    } else if status & JOB_STATUS_PRINTING != 0 {
        "PRINTING".into()
    } else {
        "SPOOLING".into()
    }
}

/// Probe-then-fill helper for the `Enum*W` APIs: a first call with no buffer
/// reports the required size via `ERROR_INSUFFICIENT_BUFFER`.
fn probe_size(probe: windows::core::Result<()>, context: &str) -> Result<(), SpoolError> {
    match probe {
        Ok(()) => Ok(()),
        Err(e) if e.code() == HRESULT::from_win32(122) => Ok(()),
        Err(e) => Err(map_error(e, context)),
    }
}

fn enum_jobs(handle: PRINTER_HANDLE) -> Result<Vec<JobInfo>, SpoolError> {
    let mut needed = 0u32;
    let mut returned = 0u32;
    // SAFETY: no buffer; only the size out-params are used.
    let probe = unsafe { EnumJobsW(handle, 0, 255, 1, None, &mut needed, &mut returned) };
    probe_size(probe, "enum jobs size")?;
    if needed == 0 {
        return Ok(vec![]);
    }
    let mut buf = vec![0u8; needed as usize];
    // SAFETY: `buf` is sized from the probe and lives through the call.
    unsafe {
        EnumJobsW(
            handle,
            0,
            255,
            1,
            Some(buf.as_mut_slice()),
            &mut needed,
            &mut returned,
        )
    }
    .map_err(|e| map_error(e, "enum jobs"))?;
    // SAFETY: the buffer holds `returned` JOB_INFO_1W structs written by the API.
    let jobs = unsafe {
        std::slice::from_raw_parts(buf.as_ptr() as *const JOB_INFO_1W, returned as usize)
    };
    Ok(jobs
        .iter()
        .map(|j| JobInfo {
            id: j.JobId,
            document: pwstr_to_string(j.pDocument),
            status_text: job_status_text(j.Status),
            total_pages: j.TotalPages,
        })
        .collect())
}

fn control_job(printer: &str, job_id: u32, command: u32, verb: &str) -> Result<(), SpoolError> {
    let handle = open_printer(printer)?;
    // SAFETY: level 0 + null job struct means "control only".
    let ok = unsafe { SetJobW(handle.0, job_id, 0, None, command) };
    if !ok.as_bool() {
        // SetJob answers ERROR_INVALID_PARAMETER for a job that is gone, the same
        // error a bad id gets, so ask the queue whether the job still exists: the
        // caller (completion) must be able to tell "the spooler forgot it" —
        // JobNotFound is tolerated there — from a real backend failure.
        let gone = enum_jobs(handle.0)
            .map(|jobs| !jobs.iter().any(|j| j.id == job_id))
            .unwrap_or(false);
        if gone {
            return Err(SpoolError::JobNotFound(job_id, printer.into()));
        }
        return Err(SpoolError::Backend(format!(
            "{verb} job {job_id} on {printer}"
        )));
    }
    Ok(())
}

impl SpoolBackend for WindowsSpool {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError> {
        let mut needed = 0u32;
        let mut returned = 0u32;
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        // SAFETY: no buffer; only the size out-params are used.
        let probe =
            unsafe { EnumPrintersW(flags, PCWSTR::null(), 2, None, &mut needed, &mut returned) };
        probe_size(probe, "enum printers size")?;
        if needed == 0 {
            return Ok(vec![]);
        }
        let mut buf = vec![0u8; needed as usize];
        // SAFETY: `buf` is sized from the probe and lives through the call.
        unsafe {
            EnumPrintersW(
                flags,
                PCWSTR::null(),
                2,
                Some(buf.as_mut_slice()),
                &mut needed,
                &mut returned,
            )
        }
        .map_err(|e| map_error(e, "enum printers"))?;
        // SAFETY: the buffer holds `returned` PRINTER_INFO_2W structs.
        let printers = unsafe {
            std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_2W, returned as usize)
        };
        Ok(printers
            .iter()
            .map(|p| PrinterInfo {
                name: pwstr_to_string(p.pPrinterName),
                status: printer_status_text(p.Status),
            })
            .collect())
    }

    fn list_jobs(&self, printer: &str) -> Result<Vec<JobInfo>, SpoolError> {
        let handle = open_printer(printer)?;
        enum_jobs(handle.0)
    }

    fn printer_caps(&self, printer: &str) -> Result<PrinterCaps, SpoolError> {
        super::devmode::caps(printer)
    }

    fn job_settings(&self, printer: &str, job_id: u32) -> Result<JobSettings, SpoolError> {
        super::devmode::read(printer, job_id)
    }

    fn apply_job_settings(
        &self,
        printer: &str,
        job_id: u32,
        settings: &JobSettings,
    ) -> Result<JobSettings, SpoolError> {
        super::devmode::apply(printer, job_id, settings)
    }

    fn is_network_printer(&self, printer: &str) -> Result<bool, SpoolError> {
        let handle = open_printer(printer)?;
        let mut needed = 0u32;
        // SAFETY: no buffer; only the size out-param is used.
        let probe = unsafe { GetPrinterW(handle.0, 2, None, &mut needed) };
        probe_size(probe, "printer attributes size")?;
        if needed == 0 {
            return Ok(false);
        }
        let mut buf = vec![0u8; needed as usize];
        // SAFETY: `buf` is sized from the probe and lives through the call.
        unsafe { GetPrinterW(handle.0, 2, Some(buf.as_mut_slice()), &mut needed) }
            .map_err(|e| map_error(e, "printer attributes"))?;
        // SAFETY: the buffer holds one PRINTER_INFO_2W written by the API.
        let info = unsafe { &*(buf.as_ptr() as *const PRINTER_INFO_2W) };
        Ok(info.Attributes & PRINTER_ATTRIBUTE_NETWORK != 0)
    }

    fn pause_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError> {
        control_job(printer, job_id, JOB_CONTROL_PAUSE, "pause")
    }

    fn resume_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError> {
        control_job(printer, job_id, JOB_CONTROL_RESUME, "resume")
    }

    fn cancel_job(&self, printer: &str, job_id: u32) -> Result<(), SpoolError> {
        control_job(printer, job_id, JOB_CONTROL_CANCEL, "cancel")
    }

    fn wait_for_new_job(
        &self,
        printer: &str,
        timeout: Duration,
    ) -> Result<Option<u32>, SpoolError> {
        let handle = open_printer(printer)?;
        let filter = PRINTER_CHANGE_ADD_JOB | PRINTER_CHANGE_SET_JOB;
        // SAFETY: null notify options + options 0 means filter-only watching.
        let change = unsafe { FindFirstPrinterChangeNotification(handle.0, filter, 0, None) };
        if change.is_invalid() {
            return Err(SpoolError::Backend(format!("watch {printer}")));
        }
        let _change = ChangeHandle(change);

        // SAFETY: waiting on a valid notification handle with a bounded timeout.
        let waited = unsafe {
            WaitForSingleObject(change, timeout.as_millis().min(u32::MAX as u128) as u32)
        };
        if waited != WAIT_OBJECT_0 {
            return Ok(None);
        }
        let mut flags = 0u32;
        // SAFETY: null options/info pointers only report which filter bits fired.
        let ok = unsafe {
            FindNextPrinterChangeNotification(change, Some(&mut flags as *mut u32), None, None)
        };
        if !ok.as_bool() {
            return Err(SpoolError::Backend(format!("watch next on {printer}")));
        }
        if flags & (PRINTER_CHANGE_ADD_JOB | PRINTER_CHANGE_SET_JOB) == 0 {
            return Ok(None);
        }
        // Newest job carries the highest id; the caller pauses it immediately.
        let mut jobs = enum_jobs(handle.0)?;
        jobs.sort_by_key(|j| j.id);
        Ok(jobs.pop().map(|j| j.id))
    }
}
