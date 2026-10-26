use mpa_agent::{AgentCore, MockTickets, OutboxEntry, OutboxPage, TargetSelection};
use mpa_domain::Coverage;
use mpa_spool::MockSpool;
use mpa_sync::{drain_outbox, PairingStore, SyncClient};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Minimal canned HTTP stub. Each connection: read request, store it, reply
/// with the next canned response.
struct Stub {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    fn start(bodies: Vec<&str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let bodies: Vec<String> = bodies.into_iter().map(|b| b.to_string()).collect();
        std::thread::spawn(move || {
            for body in bodies {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = vec![0u8; 65536];
                let Ok(n) = stream.read(&mut buf) else { break };
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                // Consume the declared body fully (small test payloads).
                recorded.lock().unwrap().push(text);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            requests,
        }
    }

    fn status_stub(status: u16, body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let body = body.to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = vec![0u8; 65536];
                if let Ok(n) = stream.read(&mut buf) {
                    recorded
                        .lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                }
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            requests,
        }
    }
}

const TIERS_JSON: &str = r#"{"tiers":[{"id":"t6","label":"BN","minCoverage":0,"maxCoverage":5,"productId":"p1","productName":"IMP BN","price":6,"isActive":true,"sortOrder":0}]}"#;
const TICKETS_JSON: &str = r#"{"tickets":[{"id":"order-101","ticketNumber":101,"createdAt":"2026-01-01","total":85.5,"cashierId":"cash-1","customerPhone":null}]}"#;

fn entry() -> OutboxEntry {
    OutboxEntry {
        idempotency_key: "PC-1-42-7".into(),
        printer: "Mock-LaserJet".into(),
        job_id: 42,
        target: TargetSelection::Ticket("order-101".into()),
        pages: vec![
            OutboxPage {
                area: None,
                page_no: 1,
                coverage: Coverage::new(80.0, 10.0, 5.0, 2.0),
                tier_id: Some("t10".into()),
                included: true,
            },
            OutboxPage {
                area: None,
                page_no: 2,
                coverage: Coverage::new(0.0, 0.0, 0.0, 3.0),
                tier_id: Some("t6".into()),
                included: false,
            },
        ],
        format: Some(mpa_analyzer::SpoolFormat::Pdf),
        spool_path: None,
        mode: mpa_domain::PrintMode::Bw,
        deliver_printer: None,
        applied_settings: None,
        options: None,
        reprint_pending: false,
        synced: false,
        remote_id: None,
        materialized: false,
    }
}

#[test]
fn pair_posts_code_and_pc_name() {
    let stub = Stub::start(vec![
        r#"{"agentId":"a1","agentKey":"k1","workspaceId":"w1"}"#,
    ]);
    let pairing = SyncClient::pair(&stub.base_url, "ABCD1234", "PC-03").unwrap();
    assert_eq!(pairing.agent_key, "k1");
    let req = stub.requests.lock().unwrap()[0].clone();
    assert!(req.contains("POST /api/agent/pair"));
    assert!(req.contains(r#""code":"ABCD1234""#));
    assert!(req.contains(r#""pcName":"PC-03""#));
}

#[test]
fn tiers_and_tickets_parse() {
    let stub = Stub::start(vec![TIERS_JSON, TICKETS_JSON]);
    let client = SyncClient::new(&stub.base_url, Some("k1".into())).unwrap();
    let tiers = client.get_tiers().unwrap();
    assert_eq!(tiers.len(), 1);
    assert_eq!(tiers[0].to_domain().id, "t6");
    assert_eq!(tiers[0].price, Some(6.0));
    let req = stub.requests.lock().unwrap()[0].clone();
    assert!(req.contains("Bearer k1"));

    let tickets = client.get_open_tickets().unwrap();
    assert_eq!(tickets[0].ticket_number, 101);
    assert_eq!(tickets[0].cashier, "cash-1");
}

#[test]
fn post_job_sends_pages_with_flags_and_target() {
    let stub = Stub::start(vec![r#"{"jobId":"job-9","status":"PENDING"}"#]);
    let client = SyncClient::new(&stub.base_url, Some("k1".into())).unwrap();
    let remote = client.post_job(&entry()).unwrap();
    assert_eq!(remote, "job-9");
    let req = stub.requests.lock().unwrap()[0].clone();
    assert!(req.contains("POST /api/agent/jobs"));
    assert!(req.contains(r#""idempotencyKey":"PC-1-42-7""#));
    assert!(req.contains(r#""targetOrderId":"order-101""#));
    assert!(req.contains(r#""pageNo":1"#));
    assert!(req.contains(r#""include":true"#));
    assert!(req.contains(r#""pageNo":2"#));
    assert!(req.contains(r#""include":false"#));
}

const JOBS_JSON: &str = r#"{"jobs":[{"id":"job-9","status":"PENDING","targetOrderId":null,"pageCount":3,"total":30,"createdAt":"2026-10-17T02:24:20.112Z"}]}"#;

#[test]
fn recent_jobs_read_back_the_server_view() {
    let stub = Stub::start(vec![
        JOBS_JSON,
        // A minimal body must parse too: the optional fields default.
        r#"{"jobs":[{"id":"job-10","status":"ATTACHED"}]}"#,
    ]);
    let client = SyncClient::new(&stub.base_url, Some("k1".into())).unwrap();

    let jobs = client.get_recent_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, "job-9");
    assert_eq!(jobs[0].status, "PENDING");
    assert_eq!(jobs[0].page_count, 3);
    assert_eq!(jobs[0].total, 30.0);
    assert!(jobs[0].target_order_id.is_none());
    let req = stub.requests.lock().unwrap()[0].clone();
    assert!(req.contains("GET /api/agent/jobs"));
    assert!(req.contains("Bearer k1"));

    let minimal = client.get_recent_jobs().unwrap();
    assert_eq!(minimal[0].status, "ATTACHED");
    assert_eq!(minimal[0].page_count, 0);
}

#[test]
fn unauthorized_maps_to_relink_hint_in_drain() {
    let stub = Stub::status_stub(401, r#"{"error":"No autenticado"}"#);
    let client = SyncClient::new(&stub.base_url, Some("bad".into())).unwrap();
    let mut core = AgentCore::new(
        Arc::new(MockSpool::new()),
        Arc::new(MockTickets::demo()),
        vec![],
    );
    core.set_outbox(vec![entry()]);
    let core = Arc::new(Mutex::new(core));
    let report = drain_outbox(&core, &client);
    assert_eq!(report.pushed.len(), 0);
    assert_eq!(report.push_failed.len(), 1);
    assert!(report.push_failed[0].1.contains("re-enlaza"));
    // Failed push keeps the entry unsynced.
    assert!(!core.lock().unwrap().outbox()[0].synced);
}

const MATERIALIZE_JSON: &str = r#"{"orderId":"order-101","ticketNumber":7,"already":false,"addedTotal":16,"newSubtotal":101.5,"newTotal":101.5}"#;

#[test]
fn drain_pushes_and_removes_finished_entries() {
    let stub = Stub::start(vec![
        r#"{"jobId":"job-9","status":"PENDING"}"#,
        MATERIALIZE_JSON,
    ]);
    let client = SyncClient::new(&stub.base_url, Some("k1".into())).unwrap();
    let mut core = AgentCore::new(
        Arc::new(MockSpool::new()),
        Arc::new(MockTickets::demo()),
        vec![],
    );
    core.set_outbox(vec![entry()]);
    let core = Arc::new(Mutex::new(core));
    let report = drain_outbox(&core, &client);
    assert_eq!(report.pushed.len(), 1);
    assert_eq!(report.removed, 1);
    assert!(core.lock().unwrap().outbox().is_empty());
    assert!(report.summary().contains("1 enviados"));
}

#[test]
fn materialize_posts_to_job_url_and_parses_ack() {
    let stub = Stub::start(vec![MATERIALIZE_JSON]);
    let client = SyncClient::new(&stub.base_url, Some("k1".into())).unwrap();
    let ack = client.materialize_job("job-9").unwrap();
    assert_eq!(ack.order_id, "order-101");
    assert_eq!(ack.ticket_number, 7);
    assert!(!ack.already);
    assert_eq!(ack.added_total, 16.0);
    let req = stub.requests.lock().unwrap()[0].clone();
    assert!(req.contains("POST /api/agent/jobs/job-9/materialize"));
    assert!(req.contains("Bearer k1"));
}

#[test]
fn drain_materializes_ticket_entries_after_push() {
    let stub = Stub::start(vec![
        r#"{"jobId":"job-9","status":"PENDING"}"#,
        MATERIALIZE_JSON,
    ]);
    let client = SyncClient::new(&stub.base_url, Some("k1".into())).unwrap();
    let mut core = AgentCore::new(
        Arc::new(MockSpool::new()),
        Arc::new(MockTickets::demo()),
        vec![],
    );
    core.set_outbox(vec![entry()]);
    let core = Arc::new(Mutex::new(core));
    let report = drain_outbox(&core, &client);
    assert_eq!(report.materialized.len(), 1);
    assert!(report.materialized[0].contains("ticket #7"));
    assert!(core.lock().unwrap().outbox().is_empty());
}

#[test]
fn drain_keeps_entry_when_materialize_fails() {
    let stub = Stub::start(vec![
        r#"{"jobId":"job-9","status":"PENDING"}"#,
        // No second body: the materialize connection fails, entry stays.
    ]);
    let client = SyncClient::new(&stub.base_url, Some("k1".into())).unwrap();
    let mut core = AgentCore::new(
        Arc::new(MockSpool::new()),
        Arc::new(MockTickets::demo()),
        vec![],
    );
    core.set_outbox(vec![entry()]);
    let core = Arc::new(Mutex::new(core));
    let report = drain_outbox(&core, &client);
    assert_eq!(report.pushed.len(), 1);
    assert_eq!(report.materialize_failed.len(), 1);
    assert_eq!(report.removed, 0);
    // Draft stored, lines not yet injected: retry next sync.
    assert!(core.lock().unwrap().outbox()[0].synced);
    assert!(!core.lock().unwrap().outbox()[0].materialized);
}

#[test]
fn drain_skips_materialize_for_ticketless_entries() {
    let stub = Stub::start(vec![r#"{"jobId":"job-9","status":"PENDING"}"#]);
    let client = SyncClient::new(&stub.base_url, Some("k1".into())).unwrap();
    let mut core = AgentCore::new(
        Arc::new(MockSpool::new()),
        Arc::new(MockTickets::demo()),
        vec![],
    );
    let mut pending = entry();
    pending.target = TargetSelection::NewDraft;
    core.set_outbox(vec![pending]);
    let core = Arc::new(Mutex::new(core));
    let report = drain_outbox(&core, &client);
    assert_eq!(report.pushed.len(), 1);
    assert!(report.materialized.is_empty());
    assert!(report.materialize_failed.is_empty());
    assert!(core.lock().unwrap().outbox().is_empty());
}

#[test]
fn pairing_store_roundtrip_with_owner_only_file() {
    let dir = std::env::temp_dir().join("mpa-pairing-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("agent.json");
    let store = PairingStore {
        server_url: "https://demo.test".into(),
        agent_id: "a1".into(),
        workspace_id: "w1".into(),
        agent_key: "secret".into(),
    };
    store.save(Some(path.clone())).unwrap();
    let loaded = PairingStore::load(Some(path.clone())).unwrap();
    assert_eq!(loaded.agent_key, "secret");
    assert_eq!(loaded.server_url, "https://demo.test");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    PairingStore::clear(Some(path.clone())).unwrap();
    assert!(PairingStore::load(Some(path)).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
