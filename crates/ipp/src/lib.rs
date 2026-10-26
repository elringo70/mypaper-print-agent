//! Minimal IPP Everywhere endpoint: the capture point for everything printed
//! from Windows, whatever application produced it.
//!
//! Why this instead of watching a physical printer's queue: a PCL driver hands
//! the spooler raw PCL, which needs a PCL interpreter (GhostPCL is AGPL) to
//! measure. An IPP endpoint is not a driver — Windows' inbox IPP Class Driver
//! renders the job itself and, when the endpoint advertises only
//! `application/pdf`, submits the document as PDF. Every application that prints
//! through Windows goes through that driver, so Word, PowerPoint, Photoshop and
//! Acrobat all arrive the same way.
//!
//! Only the subset Windows actually uses is implemented: Get-Printer-Attributes,
//! Validate-Job, Print-Job, Create-Job, Send-Document, Get-Job-Attributes and
//! Cancel-Job. Deliberately no PWG Raster: advertising it would make Windows
//! rasterise the document and throw away the resolution we want to measure.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

pub const DEFAULT_PORT: u16 = 8631;
pub const PRINT_PATH: &str = "/ipp/print";

// Operation ids (RFC 8011 §4.2).
const OP_PRINT_JOB: u16 = 0x0002;
const OP_VALIDATE_JOB: u16 = 0x0004;
const OP_CREATE_JOB: u16 = 0x0005;
const OP_SEND_DOCUMENT: u16 = 0x0006;
const OP_CANCEL_JOB: u16 = 0x0008;
const OP_GET_JOB_ATTRIBUTES: u16 = 0x0009;
const OP_GET_PRINTER_ATTRIBUTES: u16 = 0x000b;

// Status codes.
const STATUS_OK: u16 = 0x0000;
const STATUS_NOT_SUPPORTED: u16 = 0x0501;

// Delimiter tags.
const TAG_OPERATION_ATTRIBUTES: u8 = 0x01;
/// Group delimiters are everything below 0x10; the specific tags only matter
/// for readability in the parser and in tests.
const TAG_GROUP_FLOOR: u8 = 0x10;
const TAG_JOB_ATTRIBUTES: u8 = 0x02;
const TAG_END: u8 = 0x03;
const TAG_PRINTER_ATTRIBUTES: u8 = 0x04;

// Value tags.
const TAG_INTEGER: u8 = 0x21;
const TAG_BOOLEAN: u8 = 0x22;
const TAG_ENUM: u8 = 0x23;
const TAG_TEXT: u8 = 0x41;
const TAG_NAME: u8 = 0x42;
const TAG_KEYWORD: u8 = 0x44;
const TAG_URI: u8 = 0x45;
const TAG_CHARSET: u8 = 0x47;
const TAG_LANGUAGE: u8 = 0x48;
const TAG_MIME: u8 = 0x49;
const TAG_RANGE: u8 = 0x33;
const TAG_RESOLUTION: u8 = 0x32;

#[derive(Debug, thiserror::Error)]
pub enum IppError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Protocol(String),
}

/// One document received from the client.
#[derive(Debug, Clone)]
pub struct ReceivedJob {
    pub id: u32,
    pub name: String,
    pub format: String,
    pub document: Vec<u8>,
}

#[derive(Debug)]
struct Attribute {
    name: String,
    values: Vec<Vec<u8>>,
}

#[derive(Debug)]
struct Request {
    major: u8,
    minor: u8,
    operation: u16,
    request_id: u32,
    attributes: Vec<Attribute>,
    /// Everything after the end-of-attributes tag.
    document: Vec<u8>,
}

/// Parsed result of one HTTP POST: the IPP answer plus whatever document came
/// with it.
#[derive(Debug)]
pub struct Handled {
    pub response: Vec<u8>,
    pub job: Option<ReceivedJob>,
    pub operation: u16,
}

impl Request {
    fn parse(body: &[u8]) -> Result<Self, IppError> {
        if body.len() < 8 {
            return Err(IppError::Protocol("ipp: short header".into()));
        }
        let major = body[0];
        let minor = body[1];
        let operation = u16::from_be_bytes([body[2], body[3]]);
        let request_id = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);

        let mut attributes = Vec::new();
        let mut cursor = 8;
        let mut current: Option<Attribute> = None;

        loop {
            if cursor >= body.len() {
                return Err(IppError::Protocol("ipp: no end-of-attributes".into()));
            }
            let tag = body[cursor];
            cursor += 1;
            if tag == TAG_END {
                break;
            }
            if matches!(
                tag,
                TAG_OPERATION_ATTRIBUTES | TAG_JOB_ATTRIBUTES | TAG_PRINTER_ATTRIBUTES
            ) || tag < TAG_GROUP_FLOOR
            {
                // A new group delimiter (operation/job/printer/unsupported).
                // Flush whatever was being accumulated.
                if let Some(attr) = current.take() {
                    attributes.push(attr);
                }
                continue;
            }
            if cursor + 2 > body.len() {
                return Err(IppError::Protocol("ipp: truncated name length".into()));
            }
            let name_len = u16::from_be_bytes([body[cursor], body[cursor + 1]]) as usize;
            cursor += 2;
            if cursor + name_len + 2 > body.len() {
                return Err(IppError::Protocol("ipp: truncated name".into()));
            }
            let name = String::from_utf8_lossy(&body[cursor..cursor + name_len]).into();
            cursor += name_len;
            let value_len = u16::from_be_bytes([body[cursor], body[cursor + 1]]) as usize;
            cursor += 2;
            if cursor + value_len > body.len() {
                return Err(IppError::Protocol("ipp: truncated value".into()));
            }
            let value = body[cursor..cursor + value_len].to_vec();
            cursor += value_len;

            match current.as_mut() {
                // A zero-length name continues the previous attribute (1setOf).
                Some(attr) if name_len == 0 => attr.values.push(value),
                _ => {
                    if let Some(attr) = current.take() {
                        attributes.push(attr);
                    }
                    current = Some(Attribute {
                        name,
                        values: vec![value],
                    });
                }
            }
        }
        if let Some(attr) = current.take() {
            attributes.push(attr);
        }

        Ok(Self {
            major,
            minor,
            operation,
            request_id,
            attributes,
            document: body[cursor..].to_vec(),
        })
    }

    fn text(&self, name: &str) -> Option<String> {
        self.attributes
            .iter()
            .find(|a| a.name == name)
            .and_then(|a| a.values.first())
            .map(|v| String::from_utf8_lossy(v).into_owned())
    }
}

fn write_attribute(out: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
    out.push(tag);
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

/// Second and later values of a 1setOf attribute: same tag, empty name.
fn write_additional(out: &mut Vec<u8>, tag: u8, value: &[u8]) {
    out.push(tag);
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

fn response_header(major: u8, minor: u8, status: u16, request_id: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    out.push(major);
    out.push(minor);
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&request_id.to_be_bytes());
    out
}

/// What the endpoint tells Windows about itself. Everything here exists to make
/// the IPP Class Driver pick PDF: `document-format-supported` lists exactly one
/// format, so there is nothing to negotiate down to.
fn printer_attributes(port: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(TAG_PRINTER_ATTRIBUTES);

    let uri = format!("ipp://127.0.0.1:{port}{PRINT_PATH}");
    write_attribute(&mut out, TAG_CHARSET, "attributes-charset", b"utf-8");
    write_attribute(&mut out, TAG_LANGUAGE, "attributes-natural-language", b"en");
    write_attribute(&mut out, TAG_URI, "printer-uri-supported", uri.as_bytes());
    write_attribute(&mut out, TAG_KEYWORD, "uri-security-supported", b"none");
    write_attribute(
        &mut out,
        TAG_KEYWORD,
        "uri-authentication-supported",
        b"none",
    );
    write_attribute(&mut out, TAG_NAME, "printer-name", b"MyPaper");
    write_attribute(&mut out, TAG_TEXT, "printer-info", b"MyPaper Print Agent");
    write_attribute(
        &mut out,
        TAG_TEXT,
        "printer-make-and-model",
        b"MyPaper Print Agent",
    );
    write_attribute(
        &mut out,
        TAG_TEXT,
        "printer-device-id",
        b"MFG:MyPaper;MDL:Print Agent;CMD:PDF;",
    );
    write_attribute(&mut out, TAG_KEYWORD, "printer-state-reasons", b"none");
    write_attribute(&mut out, TAG_ENUM, "printer-state", &3u32.to_be_bytes()); // idle
    write_attribute(&mut out, TAG_BOOLEAN, "printer-is-accepting-jobs", &[1]);
    write_attribute(&mut out, TAG_BOOLEAN, "color-supported", &[1]);
    write_attribute(
        &mut out,
        TAG_BOOLEAN,
        "printer-multiple-document-jobs-supported",
        &[0],
    );

    // The whole point: PDF only, no PWG Raster, no JPEG.
    write_attribute(
        &mut out,
        TAG_MIME,
        "document-format-supported",
        b"application/pdf",
    );
    write_attribute(
        &mut out,
        TAG_MIME,
        "document-format-default",
        b"application/pdf",
    );
    // Windows asks for exactly these 21 attributes before it will create a
    // queue; a gap in the list reads as "dispositivo no encontrado". The
    // fixture crates/ipp/tests/fixtures/windows-get-printer-attributes.bin is
    // the real request it sends, and a test replays it.
    write_attribute(
        &mut out,
        TAG_MIME,
        "document-format-preferred",
        b"application/pdf",
    );
    write_attribute(&mut out, TAG_KEYWORD, "ipp-versions-supported", b"1.1");
    write_additional(&mut out, TAG_KEYWORD, b"2.0");
    write_attribute(&mut out, TAG_KEYWORD, "media-type-supported", b"stationery");
    // Mopria certification is what the Windows client looks for in an IPP
    // endpoint; both spellings appear in the request.
    write_attribute(&mut out, TAG_KEYWORD, "mopria-certified", b"1.0");
    write_attribute(&mut out, TAG_KEYWORD, "mopria_certified", b"1.0");
    write_attribute(&mut out, TAG_NAME, "printer-dns-sd-name", b"MyPaper");
    write_attribute(&mut out, TAG_TEXT, "printer-firmware-name", b"mpa");
    write_attribute(
        &mut out,
        TAG_TEXT,
        "printer-firmware-string-version",
        env!("CARGO_PKG_VERSION").as_bytes(),
    );
    write_attribute(&mut out, TAG_TEXT, "printer-location", b"");
    let more_info = format!("http://127.0.0.1:{port}/");
    write_attribute(&mut out, TAG_URI, "printer-more-info", more_info.as_bytes());
    // Stable per installation: Windows keys the device on it.
    write_attribute(
        &mut out,
        TAG_URI,
        "printer-uuid",
        b"urn:uuid:8f3a1c2b-4d5e-4f6a-9a1b-2c3d4e5f6071",
    );
    write_attribute(&mut out, TAG_CHARSET, "charset-supported", b"utf-8");
    write_attribute(&mut out, TAG_LANGUAGE, "natural-language-configured", b"en");

    write_attribute(
        &mut out,
        TAG_KEYWORD,
        "print-color-mode-supported",
        b"color",
    );
    write_additional(&mut out, TAG_KEYWORD, b"monochrome");
    write_attribute(&mut out, TAG_KEYWORD, "print-color-mode-default", b"color");

    write_attribute(
        &mut out,
        TAG_KEYWORD,
        "media-supported",
        b"iso_a4_210x297mm",
    );
    write_additional(&mut out, TAG_KEYWORD, b"na_letter_8.5x11in");
    write_attribute(&mut out, TAG_KEYWORD, "media-default", b"iso_a4_210x297mm");
    write_attribute(&mut out, TAG_KEYWORD, "sides-supported", b"one-sided");
    write_additional(&mut out, TAG_KEYWORD, b"two-sided-long-edge");
    write_attribute(&mut out, TAG_KEYWORD, "sides-default", b"one-sided");

    // 300 and 600 dpi; units = 3 (dots per inch).
    let mut dpi300 = Vec::new();
    dpi300.extend_from_slice(&300u32.to_be_bytes());
    dpi300.extend_from_slice(&300u32.to_be_bytes());
    dpi300.push(3);
    write_attribute(
        &mut out,
        TAG_RESOLUTION,
        "printer-resolution-supported",
        &dpi300,
    );
    write_additional(&mut out, TAG_RESOLUTION, &dpi300);
    write_attribute(
        &mut out,
        TAG_RESOLUTION,
        "printer-resolution-default",
        &dpi300,
    );

    let mut copies = Vec::new();
    copies.extend_from_slice(&1i32.to_be_bytes());
    copies.extend_from_slice(&99i32.to_be_bytes());
    write_attribute(&mut out, TAG_RANGE, "copies-supported", &copies);
    write_attribute(&mut out, TAG_INTEGER, "copies-default", &1i32.to_be_bytes());

    // What this endpoint can do, so the client picks one of them.
    for (i, op) in [
        OP_PRINT_JOB,
        OP_VALIDATE_JOB,
        OP_CREATE_JOB,
        OP_SEND_DOCUMENT,
        OP_GET_JOB_ATTRIBUTES,
        OP_CANCEL_JOB,
        OP_GET_PRINTER_ATTRIBUTES,
    ]
    .iter()
    .enumerate()
    {
        let bytes = (*op as u32).to_be_bytes();
        if i == 0 {
            write_attribute(&mut out, TAG_ENUM, "operations-supported", &bytes);
        } else {
            write_additional(&mut out, TAG_ENUM, &bytes);
        }
    }

    out.push(TAG_END);
    out
}

fn job_attributes(port: u16, id: u32, name: &str, state: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(TAG_JOB_ATTRIBUTES);
    let uri = format!("ipp://127.0.0.1:{port}/jobs/{id}");
    write_attribute(&mut out, TAG_URI, "job-uri", uri.as_bytes());
    write_attribute(&mut out, TAG_INTEGER, "job-id", &(id as i32).to_be_bytes());
    write_attribute(&mut out, TAG_ENUM, "job-state", &state.to_be_bytes());
    write_attribute(&mut out, TAG_TEXT, "job-name", name.as_bytes());
    out.push(TAG_END);
    out
}

/// Handle one IPP POST body. Pure on purpose: the socket loop stays trivial and
/// the protocol is testable without a network.
pub fn handle(body: &[u8], port: u16, job_id: u32) -> Result<Handled, IppError> {
    let mut request = Request::parse(body)?;
    let (major, minor) = (request.major, request.minor);
    let request_id = request.request_id;
    let operation = request.operation;

    let (response, job) = match operation {
        OP_GET_PRINTER_ATTRIBUTES => {
            let mut out = response_header(major, minor, STATUS_OK, request_id);
            out.extend_from_slice(&printer_attributes(port));
            (out, None)
        }
        OP_VALIDATE_JOB => (response_header(major, minor, STATUS_OK, request_id), None),
        OP_CANCEL_JOB | OP_GET_JOB_ATTRIBUTES => {
            // State 7 = canceled, 9 = completed; nothing to do on our side.
            let state = if operation == OP_CANCEL_JOB { 7 } else { 9 };
            let mut out = response_header(major, minor, STATUS_OK, request_id);
            out.extend_from_slice(&job_attributes(port, job_id, "job", state));
            (out, None)
        }
        OP_CREATE_JOB | OP_PRINT_JOB | OP_SEND_DOCUMENT => {
            let name = request
                .text("job-name")
                .unwrap_or_else(|| format!("job-{job_id}"));
            let format = request
                .text("document-format")
                .unwrap_or_else(|| "application/pdf".into());
            // `Create-Job` legitimately has no document; `Send-Document` and
            // `Print-Job` carry it.
            let document = std::mem::take(&mut request.document);
            let received = (!document.is_empty()).then(|| ReceivedJob {
                id: job_id,
                name: name.clone(),
                format,
                document,
            });
            let mut out = response_header(major, minor, STATUS_OK, request_id);
            out.extend_from_slice(&job_attributes(port, job_id, &name, 9));
            (out, received)
        }
        _ => (
            response_header(major, minor, STATUS_NOT_SUPPORTED, request_id),
            None,
        ),
    };

    Ok(Handled {
        response,
        job,
        operation,
    })
}

/// Base64-free, filesystem-safe file name for a received job.
pub fn job_file_name(job: &ReceivedJob) -> String {
    let safe: String = job
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    format!("{:08}-{safe}.pdf", job.id)
}

/// Write a received job to disk next to the agent's other state.
pub fn save_job(dir: &Path, job: &ReceivedJob) -> Result<PathBuf, IppError> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(job_file_name(job));
    std::fs::write(&path, &job.document)?;
    Ok(path)
}

/// One HTTP request, decoded.
#[derive(Debug)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

/// Read one HTTP request: request line, headers, body.
///
/// Two things the Windows client does that a naive reader misses: it sends
/// `Expect: 100-continue` and waits for the interim response before writing the
/// body (without it the connection just stalls), and it may use a chunked body.
pub fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, IppError> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    let header_end = loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Err(IppError::Protocol("http: connection closed".into()));
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(pos) = find_subsequence(&raw, b"\r\n\r\n") {
            break pos + 4;
        }
        if raw.len() > 64 * 1024 {
            return Err(IppError::Protocol("http: headers too large".into()));
        }
    };

    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or("/").to_string();

    let lowered = head.to_ascii_lowercase();
    let chunked = lowered.contains("transfer-encoding: chunked");
    let expects_continue = lowered.contains("expect: 100-continue");
    let content_length = lowered
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok());

    if expects_continue {
        // The client is holding the body until this arrives.
        stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        stream.flush()?;
    }

    let mut body = raw[header_end..].to_vec();

    if chunked {
        let mut decoded = Vec::new();
        loop {
            while !body.contains(&b'\n') {
                let n = stream.read(&mut buf)?;
                if n == 0 {
                    return Err(IppError::Protocol("http: chunked body truncated".into()));
                }
                body.extend_from_slice(&buf[..n]);
            }
            let line_end = body
                .iter()
                .position(|&b| b == b'\n')
                .ok_or_else(|| IppError::Protocol("http: chunk size".into()))?;
            let size_line = String::from_utf8_lossy(&body[..line_end])
                .trim()
                .to_string();
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("0").trim(), 16)
                .map_err(|_| IppError::Protocol("http: bad chunk size".into()))?;
            let consumed = line_end + 1;
            if size == 0 {
                break;
            }
            while body.len() < consumed + size {
                let n = stream.read(&mut buf)?;
                if n == 0 {
                    return Err(IppError::Protocol("http: chunked body truncated".into()));
                }
                body.extend_from_slice(&buf[..n]);
            }
            decoded.extend_from_slice(&body[consumed..consumed + size]);

            let mut drop_to = consumed + size;
            while body.len() < drop_to + 2 {
                let n = stream.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&buf[..n]);
            }
            if body.len() >= drop_to + 2 {
                drop_to += 2;
            }
            body.drain(..drop_to);
        }
        return Ok(HttpRequest {
            method,
            path,
            body: decoded,
        });
    }

    match content_length {
        Some(len) => {
            while body.len() < len {
                let n = stream.read(&mut buf)?;
                if n == 0 {
                    return Err(IppError::Protocol("http: body truncated".into()));
                }
                body.extend_from_slice(&buf[..n]);
            }
            body.truncate(len);
            Ok(HttpRequest { method, path, body })
        }
        None if method == "GET" || method == "HEAD" => Ok(HttpRequest {
            method,
            path,
            body: Vec::new(),
        }),
        None => Err(IppError::Protocol("http: no content-length".into())),
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Serve until the listener fails. One job at a time: measuring is fast and a
/// copy shop prints sequentially.
pub fn serve(listener: TcpListener, out_dir: PathBuf, port: u16) -> Result<(), IppError> {
    let mut next_job: u32 = 1;
    let mut seen: u32 = 0;
    for stream in listener.incoming() {
        let mut stream = stream?;
        // A client may reuse the connection for several operations; keep
        // reading until it goes quiet, otherwise Windows sees a drop in the
        // middle of its setup sequence and reports "device not found".
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        loop {
            let request = match read_http_request(&mut stream) {
                Ok(request) => request,
                Err(e) => {
                    // Windows opens extra sockets that never send anything while
                    // it probes the endpoint; a read timeout there is normal and
                    // must not look like a failure in the log.
                    match &e {
                        IppError::Io(err)
                            if matches!(
                                err.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        _ => {
                            let text = e.to_string();
                            if !text.contains("connection closed") {
                                println!("ipp: {text}");
                            }
                        }
                    }
                    break;
                }
            };
            seen += 1;

            // Everything Windows sends is kept: when the queue refuses to be
            // added, the raw bodies are the only way to know where it stopped.
            if request.method == "POST" && !request.body.is_empty() {
                let dir = out_dir.join("_requests");
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(dir.join(format!("{seen:03}.bin")), &request.body);
            }

            if request.method != "POST" {
                let text = format!(
                    "MyPaper IPP endpoint\n\nPOST {PRINT_PATH} for print jobs\ndocuments: {}\n",
                    out_dir.display()
                );
                write_plain(&mut stream, &text)?;
                break;
            }

            let handled = match handle(&request.body, port, next_job) {
                Ok(handled) => handled,
                Err(e) => {
                    println!("ipp: {e}");
                    break;
                }
            };
            println!(
                "ipp: op 0x{:04x} ({} bytes) -> {}",
                handled.operation,
                request.body.len(),
                if handled.job.is_some() {
                    "documento"
                } else {
                    "sin documento"
                }
            );

            if let Some(job) = &handled.job {
                match save_job(&out_dir, job) {
                    Ok(path) => {
                        println!(
                            "ipp: guardado {} ({} bytes)",
                            path.display(),
                            job.document.len()
                        );
                        next_job += 1;
                    }
                    Err(e) => println!("ipp: {e}"),
                }
            }

            let mut response = Vec::new();
            response.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
            response.extend_from_slice(b"Content-Type: application/ipp\r\n");
            response.extend_from_slice(
                format!("Content-Length: {}\r\n", handled.response.len()).as_bytes(),
            );
            response.extend_from_slice(b"Connection: keep-alive\r\n\r\n");
            response.extend_from_slice(&handled.response);
            stream.write_all(&response)?;
            stream.flush()?;
        }
    }
    Ok(())
}

fn write_plain(stream: &mut TcpStream, text: &str) -> Result<(), IppError> {
    let mut response = Vec::new();
    response.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
    response.extend_from_slice(b"Content-Type: text/plain; charset=utf-8\r\n");
    response.extend_from_slice(format!("Content-Length: {}\r\n", text.len()).as_bytes());
    response.extend_from_slice(b"Connection: close\r\n\r\n");
    response.extend_from_slice(text.as_bytes());
    stream.write_all(&response)?;
    stream.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(out: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
        write_attribute(out, tag, name, value);
    }

    /// A Print-Job as Windows sends it: operation attributes, end tag, document.
    fn print_job_request(document: &[u8]) -> Vec<u8> {
        let mut body = vec![0x02, 0x00];
        body.extend_from_slice(&OP_PRINT_JOB.to_be_bytes());
        body.extend_from_slice(&7u32.to_be_bytes());
        body.push(TAG_OPERATION_ATTRIBUTES);
        attr(&mut body, TAG_CHARSET, "attributes-charset", b"utf-8");
        attr(
            &mut body,
            TAG_LANGUAGE,
            "attributes-natural-language",
            b"en",
        );
        attr(
            &mut body,
            TAG_URI,
            "printer-uri",
            b"ipp://127.0.0.1:8631/ipp/print",
        );
        attr(&mut body, TAG_NAME, "job-name", b"cotizacion.pdf");
        attr(&mut body, TAG_MIME, "document-format", b"application/pdf");
        body.push(TAG_END);
        body.extend_from_slice(document);
        body
    }

    #[test]
    fn a_print_job_carries_the_document_out() {
        let pdf = b"%PDF-1.7\n...managed...\n%%EOF\n";
        let handled = handle(&print_job_request(pdf), DEFAULT_PORT, 1).expect("handled");
        let job = handled.job.expect("document extracted");
        assert_eq!(job.document, pdf);
        assert_eq!(job.format, "application/pdf");
        assert_eq!(job.name, "cotizacion.pdf");
        // Status 0x0000 in the response header, right after the version.
        assert_eq!(&handled.response[2..4], &[0x00, 0x00]);
    }

    #[test]
    fn printer_attributes_advertise_pdf_and_nothing_else() {
        let mut body = vec![0x02, 0x00];
        body.extend_from_slice(&OP_GET_PRINTER_ATTRIBUTES.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.push(TAG_OPERATION_ATTRIBUTES);
        attr(&mut body, TAG_CHARSET, "attributes-charset", b"utf-8");
        body.push(TAG_END);

        let handled = handle(&body, DEFAULT_PORT, 1).expect("handled");
        let text = String::from_utf8_lossy(&handled.response);
        assert!(text.contains("document-format-supported"));
        assert!(text.contains("application/pdf"));
        // The formats we must NOT offer: offering them makes Windows rasterise.
        assert!(!text.contains("pwg-raster"));
        assert!(!text.contains("image/jpeg"));
        // And it has to accept jobs, or the client refuses before sending.
        assert!(text.contains("printer-is-accepting-jobs"));
    }

    #[test]
    fn every_attribute_windows_asks_for_comes_back() {
        // The real Get-Printer-Attributes body captured from Add-Printer on
        // Windows. Windows answers "device not found" if any requested
        // attribute is missing, without saying which one.
        let body = include_bytes!("../tests/fixtures/windows-get-printer-attributes.bin");
        let request = Request::parse(body).expect("parsed");
        let requested: Vec<String> = request
            .attributes
            .iter()
            .filter(|a| a.name == "requested-attributes" || a.name.is_empty())
            .flat_map(|a| a.values.iter())
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .collect();
        assert!(requested.len() > 15, "fixture should carry the full list");

        let handled = handle(body, DEFAULT_PORT, 1).expect("handled");
        let text = String::from_utf8_lossy(&handled.response);
        let missing: Vec<&String> = requested
            .iter()
            .filter(|name| !text.contains(name.as_str()))
            .collect();
        assert!(missing.is_empty(), "faltan: {missing:?}");
    }

    #[test]
    fn an_unknown_operation_is_refused_not_ignored() {
        let mut body = vec![0x02, 0x00];
        body.extend_from_slice(&0x0099u16.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.push(TAG_END);
        let handled = handle(&body, DEFAULT_PORT, 1).expect("handled");
        assert_eq!(&handled.response[2..4], &STATUS_NOT_SUPPORTED.to_be_bytes());
    }

    #[test]
    fn a_set_of_values_survives_parsing() {
        // operations-supported arrives as one attribute with several values.
        let mut body = vec![0x02, 0x00];
        body.extend_from_slice(&OP_GET_PRINTER_ATTRIBUTES.to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.push(TAG_OPERATION_ATTRIBUTES);
        write_attribute(
            &mut body,
            TAG_ENUM,
            "requested-attributes",
            &0u32.to_be_bytes(),
        );
        body.extend_from_slice(&[TAG_ENUM, 0x00, 0x00, 0x00, 0x04]);
        body.extend_from_slice(&0x000bu32.to_be_bytes());
        body.push(TAG_END);

        let req = Request::parse(&body).expect("parsed");
        let attr = req
            .attributes
            .iter()
            .find(|a| a.name == "requested-attributes")
            .expect("attribute");
        assert_eq!(attr.values.len(), 2, "both values belong to the attribute");
    }

    #[test]
    fn a_body_cut_inside_the_attributes_is_an_error_not_a_partial_job() {
        // Cut inside the attribute block, before the end-of-attributes tag: the
        // client lost connection mid-send and the job must not be half-parsed.
        let full = print_job_request(b"%PDF-1.7 body");
        let cut = &full[..30];
        assert!(handle(cut, DEFAULT_PORT, 1).is_err());
    }

    #[test]
    fn a_document_shorter_than_declared_is_still_a_job() {
        // IPP carries no document length of its own: the HTTP layer enforces
        // it, so whatever arrived is what the client sent. Cut at the end tag
        // plus a few bytes and the document is simply short.
        let full = print_job_request(b"%PDF-1.7");
        let handled = handle(&full, DEFAULT_PORT, 1).expect("handled");
        assert_eq!(handled.job.expect("job").document, b"%PDF-1.7");
    }

    #[test]
    fn job_file_names_are_safe() {
        let job = ReceivedJob {
            id: 12,
            name: "../../etc/passwd".into(),
            format: "application/pdf".into(),
            document: Vec::new(),
        };
        let name = job_file_name(&job);
        assert!(!name.contains('/'));
        assert!(!name.contains(".."));
        assert!(name.ends_with(".pdf"));
    }

    #[test]
    fn a_job_lands_on_disk_with_its_bytes_intact() {
        let dir = std::env::temp_dir().join(format!("mpa-ipp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let job = ReceivedJob {
            id: 3,
            name: "factura".into(),
            format: "application/pdf".into(),
            document: b"%PDF-1.7 body".to_vec(),
        };
        let path = save_job(&dir, &job).expect("saved");
        assert_eq!(std::fs::read(&path).unwrap(), b"%PDF-1.7 body");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
