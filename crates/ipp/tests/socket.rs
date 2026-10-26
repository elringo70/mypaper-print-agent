//! The socket path, not just the parser: a real listener, a real client, and
//! the two things the Windows IPP client does that break naive servers —
//! `Expect: 100-continue` and a chunked body.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mpa-ipp-sock-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Start the endpoint on an ephemeral port and return it plus a join handle.
fn start(dir: PathBuf) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let handle = std::thread::spawn(move || {
        // Exactly the one connection this test opens, then return: a second
        // accept() would block and the join below would hang.
        for _ in 0..1 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let request = match mpa_ipp::read_http_request(&mut stream) {
                Ok(request) => request,
                Err(_) => continue,
            };
            let handled = mpa_ipp::handle(&request.body, port, 1).expect("handled");
            if let Some(job) = &handled.job {
                mpa_ipp::save_job(&dir, job).expect("saved");
            }
            let mut out = Vec::new();
            out.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\n");
            out.extend_from_slice(
                format!("Content-Length: {}\r\n\r\n", handled.response.len()).as_bytes(),
            );
            out.extend_from_slice(&handled.response);
            let _ = stream.write_all(&out);
        }
    });
    (port, handle)
}

/// Read exactly one HTTP response: headers, then the body by Content-Length.
/// `read_to_end` is wrong now that the endpoint keeps the connection open.
fn read_one_response(stream: &mut TcpStream) -> String {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf).expect("read");
        assert!(n > 0, "connection closed before headers");
        raw.extend_from_slice(&buf[..n]);
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&raw[..header_end]).to_string();
    let length = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|l| {
            l.strip_prefix("content-length:")
                .map(|v| v.trim().to_string())
        })
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    while raw.len() < header_end + length {
        let n = stream.read(&mut buf).expect("read body");
        assert!(n > 0, "connection closed before body");
        raw.extend_from_slice(&buf[..n]);
    }
    String::from_utf8_lossy(&raw).to_string()
}

fn attribute(tag: u8, name: &str, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    out
}

fn print_job(document: &[u8]) -> Vec<u8> {
    let mut body = vec![0x02, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01];
    body.push(0x01);
    body.extend(attribute(0x47, "attributes-charset", b"utf-8"));
    body.extend(attribute(0x42, "job-name", b"socket.pdf"));
    body.extend(attribute(0x49, "document-format", b"application/pdf"));
    body.push(0x03);
    body.extend_from_slice(document);
    body
}

#[test]
fn a_client_that_waits_for_continue_still_gets_its_document_saved() {
    let dir = workdir("continue");
    let (port, handle) = start(dir.clone());

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let document = b"%PDF-1.7 expect continue";
    let body = print_job(document);
    let head = format!(
        "POST /ipp/print HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/ipp\r\n\
         Content-Length: {}\r\nExpect: 100-continue\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).expect("head");

    // The interim response must arrive before the body, or the client stalls.
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).expect("interim");
    let interim = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(
        interim.starts_with("HTTP/1.1 100"),
        "expected an interim 100, got {interim:?}"
    );

    stream.write_all(&body).expect("body");
    let text = read_one_response(&mut stream);
    assert!(text.starts_with("HTTP/1.1 200"), "got {text:?}");

    handle.join().expect("server");
    let saved: Vec<_> = std::fs::read_dir(&dir)
        .expect("dir")
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(saved.len(), 1, "one job, one file");
    assert_eq!(std::fs::read(&saved[0]).expect("read"), document);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_chunked_body_arrives_whole() {
    let dir = workdir("chunked");
    let (port, handle) = start(dir.clone());

    let document = b"%PDF-1.7 chunked body";
    let body = print_job(document);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    stream
        .write_all(
            b"POST /ipp/print HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/ipp\r\n\
              Transfer-Encoding: chunked\r\n\r\n",
        )
        .expect("head");
    // Two chunks, so the decoder has to concatenate.
    for piece in body.chunks(97) {
        stream
            .write_all(format!("{:x}\r\n", piece.len()).as_bytes())
            .expect("size");
        stream.write_all(piece).expect("piece");
        stream.write_all(b"\r\n").expect("crlf");
    }
    stream.write_all(b"0\r\n\r\n").expect("end");

    let text = read_one_response(&mut stream);
    assert!(text.starts_with("HTTP/1.1 200"), "got {text:?}");

    handle.join().expect("server");
    let saved: Vec<_> = std::fs::read_dir(&dir)
        .expect("dir")
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(std::fs::read(&saved[0]).expect("read"), document);
    let _ = std::fs::remove_dir_all(&dir);
}
