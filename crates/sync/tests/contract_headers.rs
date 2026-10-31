//! The agent must identify itself on every call, and must not break when the
//! server predates `/v1`. Both halves are cheapest to check against a socket
//! stub: the real server lives in another repo.

use mpa_sync::{
    SyncClient, CONTRACT_BASE, CONTRACT_HEADER, CONTRACT_VERSION, LEGACY_BASE, VERSION_HEADER,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// One-shot HTTP stub: records the raw request text, replies with `status` and
/// `body`, then stops listening.
fn stub(status: u16, body: &'static str) -> (String, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(String::new()));
    let recorded = Arc::clone(&seen);
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = vec![0u8; 65536];
            if let Ok(n) = stream.read(&mut buf) {
                recorded
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
            let reason = if status == 200 {
                "OK"
            } else if status == 404 {
                "Not Found"
            } else {
                "OK"
            };
            let reply = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (format!("http://{addr}"), seen)
}

const VERSION_BODY: &str = r#"{"contractVersion":"1","minAgentVersion":"0.4.0","latestAgentVersion":"0.4.0"}"#;

#[test]
fn every_request_carries_contract_and_version_headers() {
    let (base_url, seen) = stub(200, VERSION_BODY);
    let client = SyncClient::new(&base_url, None)
        .unwrap()
        .with_agent_version("9.9.9-test");
    client.get_version().unwrap();

    let raw = seen.lock().unwrap().clone().to_ascii_lowercase();
    assert!(
        raw.contains(&format!("{}: {}", CONTRACT_HEADER.to_ascii_lowercase(), CONTRACT_VERSION)),
        "missing {CONTRACT_HEADER} in:\n{raw}"
    );
    assert!(
        raw.contains(&format!("{}: 9.9.9-test", VERSION_HEADER.to_ascii_lowercase())),
        "missing or stale {VERSION_HEADER} in:\n{raw}"
    );
}

#[test]
fn capabilities_is_none_when_the_server_has_no_v1_routes() {
    let (base_url, seen) = stub(404, r#"{"error":"not found"}"#);
    let client = SyncClient::new(&base_url, None).unwrap();
    let caps = client.capabilities().unwrap();
    assert!(caps.is_none(), "a 404 must mean 'no v1 yet', not an error");
    let raw = seen.lock().unwrap().to_ascii_lowercase();
    assert!(
        raw.contains("/api/print-agent/v1/capabilities"),
        "capabilities must probe the versioned path:\n{raw}"
    );
}

#[test]
fn a_404_on_capabilities_pins_the_legacy_base() {
    let (base_url, _seen) = stub(404, r#"{"error":"not found"}"#);
    let mut client = SyncClient::new(&base_url, None).unwrap();
    assert_eq!(client.base_path(), CONTRACT_BASE);
    assert!(client.negotiate().unwrap().is_none());
    assert_eq!(
        client.base_path(),
        LEGACY_BASE,
        "an old server must keep working through the unversioned alias"
    );
}

#[test]
fn capabilities_parses_the_greeting() {
    let body = r#"{"contract":1,"server":"2.3.0","minAgentVersion":"0.4.0","features":["tiers","quote"],"limits":{"maxPages":200}}"#;
    let (base_url, _seen) = stub(200, body);
    let client = SyncClient::new(&base_url, None).unwrap();
    let caps = client.capabilities().unwrap().expect("server speaks v1");
    assert_eq!(caps.contract, 1);
    assert!(caps.supports("quote"));
    assert!(!caps.supports("materialize"));
    assert_eq!(caps.limits.get("maxPages").and_then(|v| v.as_u64()), Some(200));
}
