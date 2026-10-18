use thiserror::Error;

/// Hard ceiling for one spool copy. Past this the job is pathological for
/// an intercept-and-hold agent (half a gigabyte held in RAM); fail loudly
/// instead of OOMing the cashier PC.
pub const MAX_SPOOL_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("spool dir not found: {0}")]
    NoSpoolDir(String),
    #[error("no spool file for job {0} (printer {1})")]
    NotFound(u32, String),
    #[error("spool file locked after retries: {0}")]
    Locked(String),
    #[error("spool file too large: {bytes} bytes (limit {MAX_SPOOL_BYTES})")]
    TooLarge { bytes: u64 },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Raw spool bytes for a paused job, plus the detected payload format.
#[derive(Debug)]
pub struct SpoolFile {
    pub job_id: u32,
    pub data: Vec<u8>,
    pub format: super::format::SpoolFormat,
}

/// How long to wait for a spool file to appear and stop growing.
///
/// The spooler reports a new job the moment it is queued, but the driver
/// writes `NNNNN.SPL` a moment later (measured ~1.4 s for a Word job queued to
/// a PCL driver), so the *lookup* has to be retried, not just the read:
/// resolving the path once and retrying only the read fails with
/// "no spool file" on every such race.
#[cfg(any(windows, test))]
const CAPTURE_ATTEMPTS: u32 = 40;
#[cfg(any(windows, test))]
const CAPTURE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
/// Consecutive sightings of the same non-zero size (i.e. ~`QP-1` intervals of
/// quiet) before the payload is considered complete. Drivers write the spool
/// in bursts; reading the first non-empty read can splice a half-written job.
#[cfg(any(windows, test))]
const CAPTURE_QUIET_SIGHTINGS: u32 = 4;

/// Copy `%SystemRoot%\System32\spool\PRINTERS\<jobid>.SPL`.
/// Spool filenames are the zero-padded job id (`00042.SPL`); a directory
/// scan covers drivers that deviate.
#[cfg(windows)]
pub fn capture_spool_file(job_id: u32, printer: &str) -> Result<SpoolFile, CaptureError> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let dir = std::path::PathBuf::from(format!(r"{root}\System32\spool\PRINTERS"));
    if !dir.is_dir() {
        return Err(CaptureError::NoSpoolDir(dir.display().to_string()));
    }
    capture_from_dir(&dir, job_id, printer)
}

/// The platform-independent half of the Windows capture: wait for the job's
/// spool file inside `dir` to appear and settle, then return its bytes.
/// `cfg(test)` builds it everywhere so the race is testable without Windows.
#[cfg(any(windows, test))]
fn capture_from_dir(
    dir: &std::path::Path,
    job_id: u32,
    printer: &str,
) -> Result<SpoolFile, CaptureError> {
    capture_from_dir_with(dir, job_id, printer, CAPTURE_ATTEMPTS, CAPTURE_INTERVAL)
}

#[cfg(any(windows, test))]
fn capture_from_dir_with(
    dir: &std::path::Path,
    job_id: u32,
    printer: &str,
    attempts: u32,
    interval: std::time::Duration,
) -> Result<SpoolFile, CaptureError> {
    let exact = dir.join(format!("{job_id:05}.SPL"));
    let mut last_err = None;
    let mut last_len: Option<u64> = None;
    let mut quiet = 0u32;

    for _ in 0..attempts {
        let Some(path) = resolve_spool_path(dir, &exact, job_id)? else {
            last_len = None;
            quiet = 0;
            std::thread::sleep(interval);
            continue;
        };
        // Refuse absurd sizes before paying for the copy (metadata is cheap).
        let len = match std::fs::metadata(&path) {
            Ok(meta) => meta.len(),
            Err(e) => {
                last_err = Some(e);
                last_len = None;
                quiet = 0;
                std::thread::sleep(interval);
                continue;
            }
        };
        if len > MAX_SPOOL_BYTES {
            return Err(CaptureError::TooLarge { bytes: len });
        }
        if last_len == Some(len) {
            quiet += 1;
        } else {
            last_len = Some(len);
            quiet = 1;
        }
        if len == 0 || quiet < CAPTURE_QUIET_SIGHTINGS {
            std::thread::sleep(interval);
            continue;
        }
        match std::fs::read(&path) {
            Ok(data) if data.len() as u64 == len && !data.is_empty() => {
                let format = super::format::detect_format(&data);
                return Ok(SpoolFile {
                    job_id,
                    data,
                    format,
                });
            }
            // Grew between the metadata check and the read: settle again.
            Ok(_) => {
                last_len = None;
                quiet = 0;
            }
            Err(e) => {
                last_err = Some(e);
                last_len = None;
                quiet = 0;
            }
        }
        std::thread::sleep(interval);
    }

    Err(match last_err {
        Some(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            CaptureError::Locked(exact.display().to_string())
        }
        _ => CaptureError::NotFound(job_id, printer.into()),
    })
}

/// `NNNNN.SPL` when the driver names it that way, else a directory scan.
#[cfg(any(windows, test))]
fn resolve_spool_path(
    dir: &std::path::Path,
    exact: &std::path::Path,
    job_id: u32,
) -> Result<Option<std::path::PathBuf>, CaptureError> {
    if exact.is_file() {
        return Ok(Some(exact.to_path_buf()));
    }
    scan_for_job(dir, job_id)
}

#[cfg(any(windows, test))]
fn scan_for_job(
    dir: &std::path::Path,
    job_id: u32,
) -> Result<Option<std::path::PathBuf>, CaptureError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_uppercase();
        if !name.ends_with(".SPL") {
            continue;
        }
        let digits: String = name.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits
            .trim_start_matches('0')
            .parse::<u32>()
            .unwrap_or(u32::MAX)
            == job_id
        {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// Demo bytes on non-Windows: a scripted payload so `mpa capture` and the
/// analyze pipeline run end-to-end on Linux CI.
#[cfg(not(windows))]
pub fn capture_spool_file(job_id: u32, _printer: &str) -> Result<SpoolFile, CaptureError> {
    let data = demo_page_ppm();
    let format = super::format::detect_format(&data);
    Ok(SpoolFile {
        job_id,
        data,
        format,
    })
}

/// 4x4 PPM: white with one black pixel — deterministic coverage fixture
/// (15 white pixels, 1 black → K = 6.25%).
#[cfg(not(windows))]
fn demo_page_ppm() -> Vec<u8> {
    let mut ppm = b"P6\n4 4\n255\n".to_vec();
    for i in 0..16 {
        ppm.extend_from_slice(if i == 5 { &[0, 0, 0] } else { &[255, 255, 255] });
    }
    ppm
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mpa-capture-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The regression that produced "no spool file job 4": the spooler reports
    /// the job the moment it is queued, the driver writes the `.SPL` ~1.4 s
    /// later. The lookup must be retried, not just the read.
    #[test]
    fn waits_for_a_late_spool_file() {
        let dir = scratch("late");
        let path = dir.join("00042.SPL");
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            std::fs::write(&path, b"%PDF-1.4 late").unwrap();
        });
        let spool = capture_from_dir(&dir, 42, "Test-Print").expect("must wait for the file");
        writer.join().unwrap();
        assert_eq!(spool.data, b"%PDF-1.4 late");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A driver that spools in bursts must not be sampled mid-write.
    #[test]
    fn waits_until_the_spool_stops_growing() {
        let dir = scratch("growing");
        let path = dir.join("00007.SPL");
        let writer = std::thread::spawn(move || {
            for chunk in [&b"AAAA"[..], &b"BBBB"[..], &b"CCCC"[..]] {
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&path)
                    .unwrap();
                f.write_all(chunk).unwrap();
                drop(f);
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        });
        let spool = capture_from_dir(&dir, 7, "Test-Print").expect("must settle");
        writer.join().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&spool.data),
            "AAAABBBBCCCC",
            "captured a half-written spool"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The deviating-name path: the file is not `00009.SPL` but carries the id.
    #[test]
    fn finds_a_deviating_spool_name() {
        let dir = scratch("scan");
        std::fs::write(dir.join("00009.SPL"), b"x").unwrap();
        std::fs::write(dir.join("whatever_0009.spl"), b"%PDF-1.4 scanned").unwrap();
        // Both carry id 9; the exact name wins, so drop it to exercise the scan.
        std::fs::remove_file(dir.join("00009.SPL")).unwrap();
        let spool = capture_from_dir(&dir, 9, "Test-Print").unwrap();
        assert_eq!(spool.data, b"%PDF-1.4 scanned");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A job that never spools still fails, and fails as NotFound.
    #[test]
    fn a_missing_spool_file_gives_up_as_not_found() {
        let dir = scratch("missing");
        let err = capture_from_dir_with(
            &dir,
            9,
            "Test-Print",
            3,
            std::time::Duration::from_millis(5),
        )
        .unwrap_err();
        assert!(matches!(err, CaptureError::NotFound(9, _)), "got {err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
