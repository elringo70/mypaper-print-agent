//! DEVMODE plumbing: what a printer can do, and how to change a held job's
//! print settings in place.
//!
//! The agent holds a job by pausing it, so the spooler keeps its `DEVMODE`
//! until somebody resumes it. That is the only place where paper size, media
//! type (paper thickness), tray, duplex and color can be changed for a job the
//! Windows driver already rendered — Ghostscript's `mswinpr2` reprint path
//! cannot rewrite them (see `mpa_domain::PrintOptions`).
//!
//! Everything here is read-back verified: `apply` returns what the spooler
//! reports AFTER the change, not what we asked for.

use crate::{JobSettings, PrinterCaps, SpoolError};
use windows::core::{HRESULT, HSTRING, PWSTR};
use windows::Win32::Graphics::Gdi::{DEVMODEW, DM_COLOR, DM_DUPLEX, DM_MEDIATYPE, DM_PAPERSIZE};
use windows::Win32::Graphics::Printing::{GetJobW, SetJobW, JOB_INFO_2W, PRINTER_HANDLE};

use super::windows_backend::{open_printer, open_printer_admin};

/// `DeviceCapabilities` capabilities (wingdi.h). The `windows` crate wraps only
/// the `IPrintOemUI` COM method, so the flat winspool export is declared here.
const DC_PAPERS: u16 = 2;
const DC_DUPLEX: u16 = 7;
const DC_PAPERNAMES: u16 = 16;
const DC_COLORDEVICE: u16 = 32;
/// `DC_MEDIATYPES` returns the media-type CODES (DWORDs, the values that go into
/// `dmMediaType`); the NAMES come from `DC_MEDIATYPE`, singular. Reading the
/// plural one as a name list yields garbage — verified against a Konica
/// Minolta PCL driver, where the codes are 0x00010101, 0x00010133…
const DC_MEDIATYPES: u16 = 35;
const DC_MEDIATYPE: u16 = 34;

/// Name widths used by `DeviceCapabilities` (wingdi.h): paper and media type
/// names are 64 UTF-16 code units, form names 32.
const PAPER_NAME_LEN: usize = 64;
const MEDIA_NAME_LEN: usize = 64;

// MSVC import library name (the DLL is winspool.drv; its .lib is Winspool.lib).
#[link(name = "Winspool")]
extern "system" {
    fn DeviceCapabilitiesW(
        pdevice: *const u16,
        pport: *const u16,
        fwcapability: u16,
        poutput: *mut u16,
        pdevmode: *const DEVMODEW,
    ) -> i32;
}

/// The printer's port and its default DEVMODE. `DeviceCapabilities` asks the
/// DRIVER, and several drivers (Konica Minolta among them) answer garbage or
/// nothing for `DC_MEDIATYPES` unless they get the DEVMODE too.
fn printer_port(printer: &str) -> Result<(String, Vec<u8>), SpoolError> {
    let handle = open_printer(printer)?;
    let mut needed = 0u32;
    // SAFETY: probe call with no buffer; only the size out-param is used. The
    // spooler answers ERROR_INSUFFICIENT_BUFFER here, which is the SUCCESS of a
    // size probe, not a failure.
    let probe =
        unsafe { windows::Win32::Graphics::Printing::GetPrinterW(handle.0, 2, None, &mut needed) };
    if let Err(e) = probe {
        if e.code() != HRESULT::from_win32(122) {
            return Err(SpoolError::Backend(format!("get printer {printer}: {e}")));
        }
    }
    let mut buf = vec![0u8; needed as usize];
    // SAFETY: `buf` is sized from the probe and lives through the call.
    unsafe {
        windows::Win32::Graphics::Printing::GetPrinterW(
            handle.0,
            2,
            Some(buf.as_mut_slice()),
            &mut needed,
        )
    }
    .map_err(|e| SpoolError::Backend(format!("get printer {printer}: {e}")))?;
    // SAFETY: the buffer holds a PRINTER_INFO_2W written by the API.
    let info =
        unsafe { &*(buf.as_ptr() as *const windows::Win32::Graphics::Printing::PRINTER_INFO_2W) };
    // SAFETY: spooler-owned string, valid while the buffer lives.
    let port = unsafe { info.pPortName.to_string().unwrap_or_default() };
    // Keep a copy of the default DEVMODE: it lives inside `buf`, which is
    // dropped on return, and several drivers need it to answer at all.
    let devmode = if info.pDevMode.is_null() {
        Vec::new()
    } else {
        // SAFETY: pDevMode points inside `buf`; dmSize is the public part and
        // dmDriverExtra the private tail, both written by the driver.
        let dm = unsafe { &*info.pDevMode };
        let size = dm.dmSize as usize + dm.dmDriverExtra as usize;
        // SAFETY: the DEVMODE is `size` bytes at that address.
        unsafe { std::slice::from_raw_parts(info.pDevMode as *const u8, size) }.to_vec()
    };
    Ok((port, devmode))
}

/// `DeviceCapabilities` with the printer's DEVMODE, which some drivers need to
/// answer at all.
fn device_capabilities(
    device: &HSTRING,
    port: &HSTRING,
    capability: u16,
    output: *mut u16,
    devmode: &[u8],
) -> i32 {
    let dm = if devmode.is_empty() {
        std::ptr::null()
    } else {
        devmode.as_ptr() as *const DEVMODEW
    };
    // SAFETY: `device`/`port` outlive the call; `output` is sized by the caller
    // (or null for the size probe); `dm` points at a valid DEVMODE.
    unsafe { DeviceCapabilitiesW(device.as_ptr(), port.as_ptr(), capability, output, dm) }
}

/// Names from a `DeviceCapabilities` capability that returns a name list.
/// `name_len` is the fixed width in UTF-16 units of each entry.
fn name_list(
    device: &HSTRING,
    port: &HSTRING,
    capability: u16,
    name_len: usize,
    devmode: &[u8],
) -> Result<Vec<String>, SpoolError> {
    let count = device_capabilities(device, port, capability, std::ptr::null_mut(), devmode);
    if count <= 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u16; count as usize * name_len];
    let got = device_capabilities(device, port, capability, buf.as_mut_ptr(), devmode);
    if got < 0 {
        return Err(SpoolError::Backend(format!(
            "device capabilities {capability} on {device:?}"
        )));
    }
    Ok((0..got as usize)
        .map(|i| {
            let start = i * name_len;
            let slice = &buf[start..start + name_len];
            let end = slice.iter().position(|&c| c == 0).unwrap_or(name_len);
            String::from_utf16_lossy(&slice[..end])
        })
        .collect())
}

/// Word list (paper size codes, bin numbers) for a capability.
fn word_list(
    device: &HSTRING,
    port: &HSTRING,
    capability: u16,
    devmode: &[u8],
) -> Result<Vec<u16>, SpoolError> {
    let count = device_capabilities(device, port, capability, std::ptr::null_mut(), devmode);
    if count <= 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u16; count as usize];
    let got = device_capabilities(device, port, capability, buf.as_mut_ptr(), devmode);
    if got < 0 {
        return Err(SpoolError::Backend(format!(
            "device capabilities {capability} on {device:?}"
        )));
    }
    Ok(buf[..got as usize].to_vec())
}

fn capability_count(device: &HSTRING, port: &HSTRING, capability: u16, devmode: &[u8]) -> i32 {
    device_capabilities(device, port, capability, std::ptr::null_mut(), devmode)
}

/// Paper size codes paired with their names, in the driver's order.
fn paper_sizes(
    device: &HSTRING,
    port: &HSTRING,
    devmode: &[u8],
) -> Result<Vec<(u16, String)>, SpoolError> {
    let codes = word_list(device, port, DC_PAPERS, devmode)?;
    let names = name_list(device, port, DC_PAPERNAMES, PAPER_NAME_LEN, devmode)?;
    Ok(codes
        .into_iter()
        .enumerate()
        .map(|(i, code)| (code, names.get(i).cloned().unwrap_or_default()))
        .collect())
}

/// DWORD list (media-type codes) for a capability.
fn dword_list(
    device: &HSTRING,
    port: &HSTRING,
    capability: u16,
    devmode: &[u8],
) -> Result<Vec<u32>, SpoolError> {
    let count = device_capabilities(device, port, capability, std::ptr::null_mut(), devmode);
    if count <= 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u32; count as usize];
    let got = device_capabilities(
        device,
        port,
        capability,
        buf.as_mut_ptr() as *mut u16,
        devmode,
    );
    if got < 0 {
        return Err(SpoolError::Backend(format!(
            "device capabilities {capability} on {device:?}"
        )));
    }
    Ok(buf[..got as usize].to_vec())
}

/// Media types as (code, name) pairs, in the driver's order. The codes are what
/// `dmMediaType` stores; the names are what the operator picks.
fn media_types(
    device: &HSTRING,
    port: &HSTRING,
    devmode: &[u8],
) -> Result<Vec<(u32, String)>, SpoolError> {
    let codes = dword_list(device, port, DC_MEDIATYPES, devmode)?;
    let names = name_list(device, port, DC_MEDIATYPE, MEDIA_NAME_LEN, devmode)?;
    Ok(codes
        .into_iter()
        .enumerate()
        .map(|(i, code)| (code, names.get(i).cloned().unwrap_or_default()))
        .collect())
}

/// Everything the driver reports about a printer.
pub fn caps(printer: &str) -> Result<PrinterCaps, SpoolError> {
    let (port_name, devmode) = printer_port(printer)?;
    let device = HSTRING::from(printer);
    let port = HSTRING::from(port_name.as_str());
    let papers: Vec<String> = paper_sizes(&device, &port, &devmode)?
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let media_types: Vec<String> = media_types(&device, &port, &devmode)?
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let duplex_modes = capability_count(&device, &port, DC_DUPLEX, &devmode).max(0) as u32;
    let color = capability_count(&device, &port, DC_COLORDEVICE, &devmode) > 0;
    Ok(PrinterCaps {
        papers,
        media_types,
        bins: Vec::new(),
        duplex_modes,
        color,
    })
}

/// Read a job's `JOB_INFO_2W` plus the buffer it points into. The DEVMODE lives
/// INSIDE that buffer (`pDevMode` points at the tail), so both must be kept.
fn job_info(
    handle: PRINTER_HANDLE,
    job_id: u32,
    printer: &str,
) -> Result<(Vec<u8>, JOB_INFO_2W), SpoolError> {
    let mut needed = 0u32;
    // SAFETY: probe call; only the size out-param is used. The spooler answers
    // FALSE with ERROR_INSUFFICIENT_BUFFER here, which is the SUCCESS of a size
    // probe — treating it as "not found" is what made every job look missing.
    let _ = unsafe { GetJobW(handle, job_id, 2, None, &mut needed) };
    if needed == 0 {
        return Err(SpoolError::JobNotFound(job_id, printer.into()));
    }
    let mut buf = vec![0u8; needed as usize];
    // SAFETY: `buf` is sized from the probe and lives through the call.
    let ok = unsafe { GetJobW(handle, job_id, 2, Some(buf.as_mut_slice()), &mut needed) };
    if !ok.as_bool() {
        return Err(SpoolError::JobNotFound(job_id, printer.into()));
    }
    // SAFETY: the buffer holds a JOB_INFO_2W written by the API.
    let info = unsafe { *(buf.as_ptr() as *const JOB_INFO_2W) };
    Ok((buf, info))
}

/// Settings of a held job, as the spooler reports them.
pub fn read(printer: &str, job_id: u32) -> Result<JobSettings, SpoolError> {
    let (port_name, devmode_default) = printer_port(printer)?;
    let device = HSTRING::from(printer);
    let port = HSTRING::from(port_name.as_str());
    let handle = open_printer(printer)?;
    let (buf, info) = job_info(handle.0, job_id, printer)?;
    if info.pDevMode.is_null() {
        // SAFETY: `buf` keeps the job info alive for this call.
        let _ = &buf;
        return Err(SpoolError::Backend(format!(
            "job {job_id} on {printer} carries no DEVMODE"
        )));
    }
    // SAFETY: pDevMode points inside `buf`, which is alive for this scope.
    let devmode = unsafe { &*info.pDevMode };
    let sizes = paper_sizes(&device, &port, &devmode_default)?;
    let types = media_types(&device, &port, &devmode_default)?;
    // SAFETY: the anonymous union holds dmPaperSize for printers.
    let paper_code = unsafe { devmode.Anonymous1.Anonymous1.dmPaperSize };
    let paper = sizes
        .iter()
        .find(|(code, _)| *code == paper_code as u16)
        .map(|(_, name)| name.clone());
    // dmMediaType stores the driver's CODE, not a list index.
    let media_type = types
        .iter()
        .find(|(code, _)| *code == devmode.dmMediaType)
        .map(|(_, name)| name.clone())
        .filter(|_| devmode.dmMediaType > 0);
    let _ = &buf;
    Ok(JobSettings {
        paper,
        media_type,
        duplex: match devmode.dmDuplex.0 {
            2 | 3 => Some(true),
            1 => Some(false),
            _ => None,
        },
        color: match devmode.dmColor.0 {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        },
    })
}

/// Change a held job's settings and return what the spooler reports afterwards.
pub fn apply(
    printer: &str,
    job_id: u32,
    settings: &JobSettings,
) -> Result<JobSettings, SpoolError> {
    let (port_name, devmode_default) = printer_port(printer)?;
    let device = HSTRING::from(printer);
    let port = HSTRING::from(port_name.as_str());
    // Administration rights: SetJob level 2 replaces the job's information.
    let handle = open_printer_admin(printer)?;
    let (buf, info) = job_info(handle.0, job_id, printer)?;
    if info.pDevMode.is_null() {
        let _ = &buf;
        return Err(SpoolError::Backend(format!(
            "job {job_id} on {printer} carries no DEVMODE"
        )));
    }
    if let Some(paper) = &settings.paper {
        let code = paper_sizes(&device, &port, &devmode_default)?
            .into_iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(paper))
            .map(|(code, _)| code)
            .ok_or_else(|| {
                SpoolError::Backend(format!("{printer} does not offer paper size \"{paper}\""))
            })?;
        // SAFETY: pDevMode points inside `buf`; we only write DEVMODE fields.
        unsafe {
            (*info.pDevMode).Anonymous1.Anonymous1.dmPaperSize = code as i16;
            (*info.pDevMode).dmFields |= DM_PAPERSIZE;
        }
    }
    if let Some(media) = &settings.media_type {
        let types = media_types(&device, &port, &devmode_default)?;
        let code = types
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(media))
            .map(|(code, _)| *code)
            .ok_or_else(|| {
                SpoolError::Backend(format!("{printer} does not offer media type \"{media}\""))
            })?;
        // SAFETY: same as above. dmMediaType takes the driver's own code.
        unsafe {
            (*info.pDevMode).dmMediaType = code;
            (*info.pDevMode).dmFields |= DM_MEDIATYPE;
        }
    }
    if let Some(duplex) = settings.duplex {
        // SAFETY: same as above. 1 = single-sided, 2 = long edge, 3 = short edge.
        unsafe {
            (*info.pDevMode).dmDuplex =
                windows::Win32::Graphics::Gdi::DEVMODE_DUPLEX(if duplex { 2 } else { 1 });
            (*info.pDevMode).dmFields |= DM_DUPLEX;
        }
    }
    if let Some(color) = settings.color {
        // SAFETY: same as above. 1 = monochrome, 2 = color.
        unsafe {
            (*info.pDevMode).dmColor =
                windows::Win32::Graphics::Gdi::DEVMODE_COLOR(if color { 2 } else { 1 });
            (*info.pDevMode).dmFields |= DM_COLOR;
        }
    }
    // SAFETY: `buf` holds the JOB_INFO_2W plus its DEVMODE tail; level 2 with
    // command 0 replaces the job's stored information.
    let ok = unsafe { SetJobW(handle.0, job_id, 2, Some(buf.as_ptr()), 0) };
    if !ok.as_bool() {
        // The Win32 code is the whole diagnosis here: 5 = access denied (the
        // process is not elevated), 87 = the driver rejected the DEVMODE.
        // SAFETY: reads the calling thread's last-error value.
        let code = unsafe { windows::Win32::Foundation::GetLastError() };
        return Err(SpoolError::Backend(format!(
            "set job {job_id} settings on {printer}: Win32 error {}",
            code.0
        )));
    }
    drop(buf);
    // Read back: the caller gets the spooler's answer, not our request.
    read(printer, job_id)
}

/// Silence the unused-import warning when `PWSTR` is only used in signatures.
#[allow(dead_code)]
fn _pwstr_marker() -> PWSTR {
    PWSTR::null()
}
