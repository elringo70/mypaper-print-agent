//! A document printed to our own IPP queue must land in the agent's job map,
//! measured — the same place the window reads.
//!
//! This is the capture path that replaced watching a physical printer: the PCL
//! dead end is gone, and what arrives is already the document. The test goes
//! through a real socket, so it covers the HTTP framing as well as the wiring.

use mpa_agent::{AgentCore, AgentRuntime, MockTickets};
use mpa_domain::{PolicyMode, PrinterPolicy, Tier};
use mpa_spool::{PrinterInfo, SpoolBackend, SpoolError};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// No printer at all: capture must work on a machine whose only queue is ours.
#[derive(Default)]
struct NoPrinters;

impl SpoolBackend for NoPrinters {
    fn list_printers(&self) -> Result<Vec<PrinterInfo>, SpoolError> {
        Ok(Vec::new())
    }
    fn list_jobs(&self, _printer: &str) -> Result<Vec<mpa_spool::JobInfo>, SpoolError> {
        Ok(Vec::new())
    }
    fn pause_job(&self, _printer: &str, _job_id: u32) -> Result<(), SpoolError> {
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
        _timeout: Duration,
    ) -> Result<Option<u32>, SpoolError> {
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

fn attribute(tag: u8, name: &str, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    out
}

fn print_job(document: &[u8], name: &str) -> Vec<u8> {
    let mut body = vec![0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01];
    body.push(0x01);
    body.extend(attribute(0x47, "attributes-charset", b"utf-8"));
    body.extend(attribute(0x42, "job-name", name.as_bytes()));
    body.extend(attribute(0x49, "document-format", b"image/png"));
    body.push(0x03); // end-of-attributes, then the document
    body.extend_from_slice(document);
    body
}

#[test]
fn a_printed_document_appears_in_the_window_measured() {
    let dir = std::env::temp_dir().join(format!("mpa-capture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let core = Arc::new(Mutex::new(AgentCore::new(
        Arc::new(NoPrinters) as Arc<dyn SpoolBackend>,
        Arc::new(MockTickets::demo()),
        tiers(),
    )));
    let policy = PrinterPolicy {
        mode: PolicyMode::Denylist,
        watched: vec![],
        ignored: vec![],
    };
    let mut runtime = AgentRuntime::start(Arc::clone(&core), policy, Duration::from_millis(20));

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    runtime
        .start_capture(listener, dir.clone(), port)
        .expect("capture starts");

    // 8x8 PNG: white sheet with a black block, so the page has measurable ink.
    let document = include_bytes!("fixtures/capture.png");
    let body = print_job(document, "cotizacion.png");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    stream
        .write_all(
            format!(
                "POST /ipp/print HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .expect("head");
    stream.write_all(&body).expect("body");
    let mut response = [0u8; 256];
    let n = stream.read(&mut response).expect("response");
    assert!(
        String::from_utf8_lossy(&response[..n]).starts_with("HTTP/1.1 200"),
        "the client must be answered before anything is measured"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut found = None;
    while Instant::now() < deadline {
        {
            let core = core.lock().expect("core");
            for job in core.snapshot() {
                if job.printer == mpa_agent::runtime::CAPTURE_PRINTER && !job.pages.is_empty() {
                    found = Some(job);
                }
            }
        }
        if found.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let job = found.expect("the captured document must appear in the job map");
    assert_eq!(job.document, "cotizacion.png");
    assert_eq!(job.pages.len(), 1, "one page measured");
    let page = &job.pages[0];
    let area = page.area.expect("area measured");
    assert!(
        (area.area_pct - 25.0).abs() < 1.0,
        "a quarter of the sheet is inked, got {}",
        area.area_pct
    );
    assert!(
        job.spool_path.as_ref().is_some_and(|p| p.exists()),
        "the document is on disk, so a restart loses nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
