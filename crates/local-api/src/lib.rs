//! Loopback HTTP bridge for the MyPaper webapp.
//!
//! Why it exists: the agent is the source of truth for the ink analysis, and the
//! browser is where the operator turns that analysis into ticket lines. That
//! hand-over must not go through the server — the server prices and stores, it
//! does not relay — so the browser asks this listener directly.
//!
//! Security model (loopback is NOT a trust boundary: any page on this machine can
//! reach it):
//! - binds to `127.0.0.1` only;
//! - reflects `Access-Control-Allow-Origin` only for the paired MyPaper origin
//!   or a loopback dev origin, never `*`;
//! - every data route needs `x-mpa-local-token`, minted by `/v1/handshake` after
//!   the agent proves the operator's pairing code against the server;
//! - no cookies, so a page that guesses the port without the token gets 401.
//!
//! Written on `std` and a small HTTP parser on purpose: the agent has no async
//! runtime today, and one more framework for four routes would be the biggest
//! dependency in the workspace.

use mpa_agent::AgentCore;
use rand_core::{OsRng, RngCore};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default port of the local bridge. The IPP capture endpoint keeps `8631`.
pub const DEFAULT_PORT: u16 = 8632;

/// How long a handshake token lives. Long enough for a work shift, short enough
/// that a forgotten browser tab stops working on its own.
pub const TOKEN_TTL: Duration = Duration::from_secs(8 * 60 * 60);

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum LocalApiError {
    #[error("no se pudo escuchar en {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// Running bridge. Dropping it does not stop the accept loop (it lives on its
/// own thread); the port is released when the process exits.
#[derive(Debug)]
pub struct LocalApi {
    pub addr: SocketAddr,
}

struct Grant {
    token: String,
    expires: Instant,
}

struct Shared {
    core: Arc<Mutex<AgentCore>>,
    grants: Mutex<Vec<Grant>>,
    /// Origin of the paired MyPaper server, read once at startup.
    server_origin: Option<String>,
    /// Version of the application hosting this bridge, not of this crate: the
    /// browser and the update check compare the app they installed.
    app_version: String,
}

/// Start the bridge on `port`. Returns as soon as the listener is bound.
pub fn serve(
    core: Arc<Mutex<AgentCore>>,
    port: u16,
    app_version: &str,
) -> Result<LocalApi, LocalApiError> {
    let listener =
        TcpListener::bind(("127.0.0.1", port)).map_err(|source| LocalApiError::Bind {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            source,
        })?;
    let addr = listener
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], port)));
    let shared = Arc::new(Shared {
        core,
        grants: Mutex::new(Vec::new()),
        server_origin: mpa_sync::PairingStore::load(None)
            .ok()
            .and_then(|p| origin_of(&p.server_url)),
        app_version: app_version.to_string(),
    });

    std::thread::Builder::new()
        .name("mpa-local-api".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || handle(&shared, stream));
            }
        })
        .map_err(|source| LocalApiError::Bind { addr, source })?;

    Ok(LocalApi { addr })
}

/// `scheme://host[:port]` of a URL, enough to compare browser origins.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host.to_ascii_lowercase()
    ))
}

/// Whether a browser origin may read this bridge.
///
/// The paired server is the only production origin: anything else is a page on
/// this machine that has no business reading the shop's print jobs. Loopback
/// stays open because that is how the webapp is developed and previewed.
pub fn origin_allowed(origin: &str, server_origin: Option<&str>) -> bool {
    let origin = origin.trim_end_matches('/').to_ascii_lowercase();
    if origin == "http://localhost" || origin.starts_with("http://localhost:") {
        return true;
    }
    if origin == "http://127.0.0.1" || origin.starts_with("http://127.0.0.1:") {
        return true;
    }
    match server_origin {
        Some(allowed) => origin == allowed.trim_end_matches('/').to_ascii_lowercase(),
        None => false,
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target, String::new()),
    };

    let mut headers = Vec::new();
    let mut consumed = line.len();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).ok()?;
        if read == 0 {
            break;
        }
        consumed += read;
        if consumed > MAX_HEADER_BYTES {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let mut body = Vec::new();
    let length = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_BODY_BYTES);
    if length > 0 {
        body.resize(length, 0);
        reader.read_exact(&mut body).ok()?;
    }

    Some(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn handle(shared: &Shared, mut stream: TcpStream) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let origin = request.header("origin").unwrap_or_default().to_string();
    let allowed = !origin.is_empty() && origin_allowed(&origin, shared.server_origin.as_deref());
    let preflight = request
        .header("access-control-request-private-network")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let cors = cors_headers(&origin, allowed, preflight);

    // A disallowed origin learns nothing: no CORS headers, plain 403.
    if !origin.is_empty() && !allowed {
        let _ = respond(
            &mut stream,
            403,
            "application/json",
            b"{\"error\":\"origen no permitido\"}",
            &[],
        );
        return;
    }
    if request.method == "OPTIONS" {
        let _ = respond(&mut stream, 204, "text/plain", b"", &cors);
        return;
    }

    let reply = route(shared, &request);
    let _ = match reply {
        Ok((status, body)) => respond(
            &mut stream,
            status,
            "application/json",
            body.as_bytes(),
            &cors,
        ),
        Err(err) => respond(
            &mut stream,
            err.status,
            "application/json",
            format!("{{\"error\":\"{}\"}}", err.message).as_bytes(),
            &cors,
        ),
    };
}

struct RouteError {
    status: u16,
    message: String,
}

impl std::fmt::Debug for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteError")
            .field("status", &self.status)
            .field("message", &self.message)
            .finish()
    }
}

fn err(status: u16, message: &str) -> RouteError {
    RouteError {
        status,
        message: message.to_string(),
    }
}

fn route(shared: &Shared, request: &Request) -> Result<(u16, String), RouteError> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/v1/status") => Ok((200, status_json(shared))),
        ("POST", "/v1/handshake") => handshake(shared, request),
        ("GET", "/v1/jobs") => {
            require_token(shared, request)?;
            Ok((200, jobs_json(shared)))
        }
        ("GET", "/v1/page.png") => {
            require_token(shared, request)?;
            page_png(shared, request)
        }
        (_, "/v1/status" | "/v1/handshake" | "/v1/jobs" | "/v1/page.png") => {
            Err(err(405, "método no permitido"))
        }
        _ => Err(err(404, "ruta desconocida")),
    }
}

fn status_json(shared: &Shared) -> String {
    let pairing = mpa_sync::PairingStore::load(None).ok();
    let (paired, workspace_id, agent_id) = match &pairing {
        Some(p) => (true, p.workspace_id.clone(), p.agent_id.clone()),
        None => (false, String::new(), String::new()),
    };
    let pending = shared
        .core
        .lock()
        .map(|c| c.pending_outbox())
        .unwrap_or_default();
    let jobs = shared
        .core
        .lock()
        .map(|c| c.job_count())
        .unwrap_or_default();
    serde_json::json!({
        "ok": true,
        "service": "mypaper-print-agent",
        "version": shared.app_version,
        "contractVersion": "0",
        "paired": paired,
        "workspaceId": workspace_id,
        "agentId": agent_id,
        "pendingJobs": jobs,
        "pendingOutbox": pending,
    })
    .to_string()
}

fn handshake(shared: &Shared, request: &Request) -> Result<(u16, String), RouteError> {
    let body: serde_json::Value =
        serde_json::from_slice(&request.body).map_err(|_| err(400, "cuerpo inválido"))?;
    let code = body
        .get("code")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    if code.is_empty() {
        return Err(err(400, "falta el código"));
    }

    let pairing =
        mpa_sync::PairingStore::load(None).map_err(|_| err(401, "el agente no está enlazado"))?;
    // `connect` negotiates the contract base: a server that has not been updated
    // yet answers 404 on the versioned path and the client falls back to the
    // legacy one instead of failing the handshake with a 502.
    let client = mpa_sync::connect(&pairing.server_url, Some(pairing.agent_key))
        .map_err(|_| err(502, "enlace no disponible"))?;
    match client.local_grant(&code) {
        Ok(true) => {}
        Ok(false) => return Err(err(403, "código inválido, usado o expirado")),
        Err(_) => return Err(err(502, "no se pudo validar el código con MyPaper")),
    }

    let mut raw = [0u8; 32];
    OsRng.fill_bytes(&mut raw);
    let token = hex::encode(raw);
    let expires = Instant::now() + TOKEN_TTL;
    if let Ok(mut grants) = shared.grants.lock() {
        grants.retain(|g| g.expires > Instant::now());
        grants.push(Grant {
            token: token.clone(),
            expires,
        });
    }
    Ok((
        200,
        serde_json::json!({ "token": token, "expiresIn": TOKEN_TTL.as_secs() }).to_string(),
    ))
}

fn require_token(shared: &Shared, request: &Request) -> Result<(), RouteError> {
    let header = request.header("x-mpa-local-token").unwrap_or_default();
    let from_query = query_param(&request.query, "token").unwrap_or_default();
    let candidate = if header.is_empty() {
        from_query
    } else {
        header.to_string()
    };
    if candidate.is_empty() {
        return Err(err(401, "falta el token local"));
    }
    let mut grants = shared
        .grants
        .lock()
        .map_err(|_| err(500, "estado bloqueado"))?;
    grants.retain(|g| g.expires > Instant::now());
    match grants.iter().any(|g| g.token == candidate) {
        true => Ok(()),
        false => Err(err(401, "token local inválido o expirado")),
    }
}

fn jobs_json(shared: &Shared) -> String {
    let Ok(core) = shared.core.lock() else {
        return serde_json::json!({ "jobs": [] }).to_string();
    };
    let jobs: Vec<serde_json::Value> = core
        .job_keys()
        .into_iter()
        .filter_map(|(printer, job_id)| {
            let job = core.job(&printer, job_id)?;
            let pages: Vec<serde_json::Value> = job
                .pages
                .iter()
                .map(|page| {
                    serde_json::json!({
                        "pageNo": page.page_no,
                        "c": page.coverage.c,
                        "m": page.coverage.m,
                        "y": page.coverage.y,
                        "k": page.coverage.k,
                        "areaPct": page.area.map(|a| a.area_pct),
                        "class": page.area.and_then(|a| a.class).map(|c| c.as_str()),
                        "included": page.included,
                    })
                })
                .collect();
            Some(serde_json::json!({
                "printer": &job.printer,
                "jobId": job.job_id,
                "document": &job.document,
                "state": format!("{:?}", job.state),
                "format": job.format.as_ref().map(|f| format!("{f:?}")),
                "media": &job.media,
                "printMode": format!("{:?}", job.print_mode),
                "pages": pages,
            }))
        })
        .collect();
    serde_json::json!({ "jobs": jobs }).to_string()
}

fn page_png(shared: &Shared, request: &Request) -> Result<(u16, String), RouteError> {
    let printer =
        query_param(&request.query, "printer").ok_or_else(|| err(400, "falta printer"))?;
    let job_id: u32 = query_param(&request.query, "job")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| err(400, "falta job"))?;
    let page_no: u32 = query_param(&request.query, "page")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| err(400, "falta page"))?;

    let core = shared
        .core
        .lock()
        .map_err(|_| err(500, "estado bloqueado"))?;
    let png = core
        .page(&printer, job_id, page_no)
        .map(|p| p.thumbnail_png.clone())
        .unwrap_or_default();
    drop(core);

    match png.is_empty() {
        true => Err(err(404, "página sin vista previa")),
        false => Ok((
            200,
            serde_json::json!({ "dataUrl": format!("data:image/png;base64,{}", base64_std(&png)) })
                .to_string(),
        )),
    }
}

fn base64_std(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn cors_headers(origin: &str, allowed: bool, private_network: bool) -> Vec<String> {
    if !allowed {
        return Vec::new();
    }
    let mut headers = vec![
        format!("Access-Control-Allow-Origin: {origin}"),
        "Vary: Origin".to_string(),
        "Access-Control-Allow-Methods: GET, POST, OPTIONS".to_string(),
        "Access-Control-Allow-Headers: content-type, x-mpa-local-token".to_string(),
        "Access-Control-Max-Age: 600".to_string(),
    ];
    if private_network {
        // Chrome/Edge preflight for a request from a public page to loopback.
        headers.push("Access-Control-Allow-Private-Network: true".to_string());
    }
    headers
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key != name {
            return None;
        }
        Some(percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra: &[String],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for line in extra {
        head.push_str(line);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_of_keeps_scheme_and_host() {
        assert_eq!(
            origin_of("https://mypaper.example.com/app?x=1"),
            Some("https://mypaper.example.com".into())
        );
        assert_eq!(
            origin_of("http://localhost:3000"),
            Some("http://localhost:3000".into())
        );
        assert_eq!(origin_of("sin-esquema"), None);
    }

    #[test]
    fn only_the_paired_server_and_loopback_are_allowed() {
        let paired = origin_of("https://mypaper.example.com");
        assert!(origin_allowed(
            "https://mypaper.example.com",
            paired.as_deref()
        ));
        assert!(origin_allowed("http://localhost:5173", paired.as_deref()));
        assert!(origin_allowed("http://127.0.0.1:3000", paired.as_deref()));
        assert!(!origin_allowed("https://evil.example", paired.as_deref()));
        assert!(!origin_allowed("https://evil.example", None));
    }

    #[test]
    fn percent_decoding_handles_spaces_and_case() {
        assert_eq!(percent_decode("MyPaper%20Cola"), "MyPaper Cola");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("na_legal_8.5x14in"), "na_legal_8.5x14in");
    }

    #[test]
    fn query_params_are_extracted_by_name() {
        let query = "printer=MyPaper%20Cola&job=7&page=2";
        assert_eq!(
            query_param(query, "printer").as_deref(),
            Some("MyPaper Cola")
        );
        assert_eq!(query_param(query, "job").as_deref(), Some("7"));
        assert_eq!(query_param(query, "page").as_deref(), Some("2"));
        assert_eq!(query_param(query, "missing"), None);
    }

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64_std(b""), "");
        assert_eq!(base64_std(b"f"), "Zg==");
        assert_eq!(base64_std(b"fo"), "Zm8=");
        assert_eq!(base64_std(b"foo"), "Zm9v");
        assert_eq!(base64_std(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn cors_is_silent_for_a_foreign_origin() {
        assert!(cors_headers("https://evil.example", false, false).is_empty());
        let headers = cors_headers("http://localhost:3000", true, true);
        assert!(headers
            .iter()
            .any(|h| h == "Access-Control-Allow-Origin: http://localhost:3000"));
        assert!(headers
            .iter()
            .any(|h| h == "Access-Control-Allow-Private-Network: true"));
    }
}
