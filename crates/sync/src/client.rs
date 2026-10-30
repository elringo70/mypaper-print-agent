use base64::Engine;
use mpa_agent::{OutboxEntry, TargetSelection};
use mpa_domain::{MetricsByMode, PrintMode, Tier};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("http: {0}")]
    Http(String),
    #[error("server {0}: {1}")]
    Status(u16, String),
    #[error("bad response: {0}")]
    Parse(String),
}

impl From<reqwest::Error> for SyncError {
    fn from(e: reqwest::Error) -> Self {
        SyncError::Http(e.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
struct PairBody<'a> {
    code: &'a str,
    #[serde(rename = "pcName")]
    pc_name: &'a str,
    /// Ed25519 public key the device will sign every later request with.
    #[serde(rename = "publicKey")]
    public_key: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
struct PairResponse {
    #[serde(rename = "agentId")]
    agent_id: String,
    #[serde(rename = "agentKey")]
    agent_key: String,
    #[serde(rename = "workspaceId")]
    workspace_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pairing {
    pub agent_id: String,
    pub agent_key: String,
    pub workspace_id: String,
    /// Base64 of the device secret generated locally for this pairing. It never
    /// comes from the server; `pair` fills it so the caller can persist it.
    pub device_secret: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementToken {
    pub token: String,
    /// RFC 3339 instant, for display only: the token's `exp` is the authority.
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionInfo {
    #[serde(rename = "contractVersion")]
    pub contract_version: String,
    #[serde(rename = "minAgentVersion")]
    pub min_agent_version: String,
    #[serde(rename = "latestAgentVersion")]
    pub latest_agent_version: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TiersResponse {
    /// Billing metric per mode; absent on servers predating Fase 8.
    #[serde(default)]
    metrics: MetricsByMode,
    /// Fase 6 billing rules; an older server sends none.
    #[serde(default)]
    pricing: mpa_domain::PrintPricing,
    tiers: Vec<RemoteTier>,
}

/// Tabulador plus the billing metrics and the Fase 6 rules MyPaper prices with.
#[derive(Debug, Clone)]
pub struct TierSync {
    pub tiers: Vec<Tier>,
    pub metrics: MetricsByMode,
    /// Fase 6 rules (intensity, greys, blank pages, paper, volume).
    pub pricing: mpa_domain::PrintPricing,
    /// Tier id → price, so the agent can label a page with the rule set that
    /// would actually be dearest (the server still prices authoritatively).
    pub prices: std::collections::HashMap<String, f32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTier {
    pub id: String,
    pub label: String,
    /// Rule set. Servers predating Fase 1 only had color rules, so a missing
    /// field defaults to COLOR instead of the enum's BW default.
    #[serde(default = "legacy_tier_mode")]
    pub mode: PrintMode,
    pub min_coverage: f64,
    pub max_coverage: f64,
    pub product_id: String,
    pub product_name: Option<String>,
    pub price: Option<f64>,
    pub is_active: bool,
    pub sort_order: i32,
}

fn legacy_tier_mode() -> PrintMode {
    PrintMode::Color
}

impl RemoteTier {
    pub fn to_domain(&self) -> Tier {
        Tier {
            id: self.id.clone(),
            mode: self.mode,
            min_coverage: self.min_coverage as f32,
            max_coverage: self.max_coverage as f32,
            active: self.is_active,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct TicketsResponse {
    tickets: Vec<RemoteTicket>,
}

#[derive(Debug, Clone, Deserialize)]
struct JobsResponse {
    jobs: Vec<RemoteJob>,
}

/// One stored analysis as the server reports it. Extra keys are ignored so the
/// client tolerates response growth.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteJob {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub target_order_id: Option<String>,
    #[serde(default)]
    pub page_count: i32,
    #[serde(default)]
    pub total: f64,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteTicket {
    id: String,
    ticket_number: i32,
    cashier_id: String,
    #[serde(default)]
    cashier_name: Option<String>,
    #[serde(default)]
    customer_name: Option<String>,
    total: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobPage {
    page_no: u32,
    c: f32,
    m: f32,
    y: f32,
    k: f32,
    /// Area and class travel only when the analyzer measured pixels. The
    /// server ignores them unless the workspace bills on AREA.
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage_nonwhite: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_class: Option<&'static str>,
    include: bool,
}

/// Server summary of an injected draft. Extra keys are ignored so the
/// client tolerates future response growth.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializeAck {
    pub order_id: String,
    pub ticket_number: i32,
    #[serde(default)]
    pub already: bool,
    #[serde(default)]
    pub added_total: f64,
    #[serde(default)]
    pub new_total: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct JobBody {
    idempotency_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_order_id: Option<String>,
    /// Rule set the operator chose; MyPaper prices the draft with it.
    mode: mpa_domain::PrintMode,
    /// Páginas que el agente analizó. El servidor rechaza el trabajo si llegan
    /// menos: un documento a medias se cobra a medias.
    page_count: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    document_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media: Option<String>,
    pages: Vec<JobPage>,
}

/// What the server stored, as it reports it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedJob {
    pub id: String,
    /// Pages the server actually priced. `None` on a server that predates the
    /// echo: the agent then accepts the push but cannot claim confirmation.
    pub page_count: Option<i32>,
}

/// One page as the quote endpoint expects it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotePage {
    pub page_no: u32,
    pub c: f32,
    pub m: f32,
    pub y: f32,
    pub k: f32,
    pub include: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteLine {
    pub page_no: u32,
    #[serde(default)]
    pub coverage: f32,
    #[serde(default)]
    pub tier_id: Option<String>,
    #[serde(default)]
    pub tier_label: Option<String>,
    #[serde(default)]
    pub product_id: Option<String>,
    #[serde(default)]
    pub product_name: Option<String>,
    #[serde(default)]
    pub unit_price: f64,
}

/// What one printer role would cost for the analyzed pages.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteMode {
    #[serde(default)]
    pub total: f64,
    #[serde(default)]
    pub page_count: i32,
    /// Pages the tabulador could not price in this mode.
    #[serde(default)]
    pub unpriced: Vec<u32>,
    #[serde(default)]
    pub lines: Vec<QuoteLine>,
}

/// Quote for both rule sets. The server keys them by mode, uppercase.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub struct QuoteModes {
    #[serde(default)]
    pub bw: QuoteMode,
    #[serde(default)]
    pub color: QuoteMode,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Quote {
    #[serde(default)]
    pub modes: QuoteModes,
}

impl Quote {
    /// Price of the rule set the operator picked (BW is the default).
    pub fn for_mode(&self, mode: PrintMode) -> &QuoteMode {
        match mode {
            PrintMode::Bw => &self.modes.bw,
            PrintMode::Color => &self.modes.color,
        }
    }
}

/// Thin client for the Fase 0 contract (`/api/agent/*`). Bearer key on
/// every call except `pair` and `version`.
pub struct SyncClient {
    base_url: String,
    agent_key: Option<String>,
    /// Present when the pairing store holds a device key.
    device: Option<crate::sign::DeviceKey>,
    http: reqwest::blocking::Client,
}

impl std::fmt::Debug for SyncClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncClient")
            .field("base_url", &self.base_url)
            .field("paired", &self.agent_key.is_some())
            .finish_non_exhaustive()
    }
}

impl SyncClient {
    pub fn new(base_url: &str, agent_key: Option<String>) -> Result<Self, SyncError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let base_url = base_url.trim_end_matches('/').to_string();
        // The device key lives in the pairing store. Attaching it here keeps
        // every existing call site honest without threading a parameter
        // through the whole agent; a client pointed at another server (tests,
        // update manifests) simply stays unsigned.
        let device = crate::pairing::PairingStore::load(None)
            .ok()
            .filter(|p| p.server_url == base_url)
            .and_then(|p| p.device_key().ok());
        Ok(Self {
            base_url,
            agent_key,
            device,
            http,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn pair(base_url: &str, code: &str, pc_name: &str) -> Result<Pairing, SyncError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let url = format!("{}/api/agent/pair", base_url.trim_end_matches('/'));
        // A fresh identity per pairing: re-pairing rotates the key, which is
        // also how a suspected copy is invalidated.
        let device = crate::sign::DeviceKey::generate();
        let public_key = device.public_b64();
        let res = http
            .post(&url)
            .json(&PairBody {
                code,
                pc_name,
                public_key: &public_key,
            })
            .send()?;
        if !res.status().is_success() {
            let status = res.status().as_u16();
            let body = res.text().unwrap_or_default();
            return Err(SyncError::Status(status, body));
        }
        let body: PairResponse = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        Ok(Pairing {
            agent_id: body.agent_id,
            agent_key: body.agent_key,
            workspace_id: body.workspace_id,
            device_secret: Some(base64::engine::general_purpose::STANDARD.encode(device.secret())),
        })
    }

    fn auth(&self, req: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        match &self.agent_key {
            Some(key) => req.bearer_auth(key),
            None => req,
        }
    }

    /// Build an agent request: bearer key plus, when a device key exists, the
    /// Ed25519 signature over method, path, timestamp, nonce and body.
    fn signed(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> reqwest::blocking::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let req = if method.eq_ignore_ascii_case("get") {
            self.http.get(&url)
        } else {
            self.http.post(&url)
        };
        let mut req = self.auth(req);
        if let Some(device) = &self.device {
            let signed = device.sign_request(method, path, body.as_deref().unwrap_or(&[]));
            for (name, value) in signed.headers() {
                req = req.header(name, value);
            }
        }
        match body {
            Some(bytes) => req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes),
            None => req,
        }
    }

    fn check(res: reqwest::blocking::Response) -> Result<reqwest::blocking::Response, SyncError> {
        if res.status().is_success() {
            Ok(res)
        } else {
            let status = res.status().as_u16();
            let body = res.text().unwrap_or_default();
            Err(SyncError::Status(status, body))
        }
    }

    /// Raw text of any URL (release manifests and their signature files).
    /// Authenticated when the client carries a key; public manifests work
    /// keyless too.
    pub fn get_text(&self, url: &str) -> Result<String, SyncError> {
        let res = Self::check(self.auth(self.http.get(url)).send()?)?;
        res.text().map_err(|e| SyncError::Parse(e.to_string()))
    }

    /// Raw bytes of any URL (installer downloads).
    pub fn get_bytes(&self, url: &str) -> Result<Vec<u8>, SyncError> {
        let res = Self::check(self.auth(self.http.get(url)).send()?)?;
        res.bytes()
            .map(|b| b.to_vec())
            .map_err(|e| SyncError::Parse(e.to_string()))
    }

    pub fn get_version(&self) -> Result<VersionInfo, SyncError> {
        let url = format!("{}/api/agent/version", self.base_url);
        let res = Self::check(self.http.get(&url).send()?)?;
        res.json().map_err(|e| SyncError::Parse(e.to_string()))
    }

    pub fn get_tiers(&self) -> Result<Vec<RemoteTier>, SyncError> {
        let res = Self::check(self.signed("GET", "/api/agent/tiers", None).send()?)?;
        let body: TiersResponse = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        Ok(body.tiers)
    }

    /// Tabulador mapped to domain types plus the metrics in force.
    pub fn get_tiers_sync(&self) -> Result<TierSync, SyncError> {
        let res = Self::check(self.signed("GET", "/api/agent/tiers", None).send()?)?;
        let body: TiersResponse = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        Ok(TierSync {
            tiers: body.tiers.iter().map(|t| t.to_domain()).collect(),
            metrics: body.metrics,
            pricing: body.pricing,
            prices: body
                .tiers
                .iter()
                .filter_map(|t| t.price.map(|price| (t.id.clone(), price as f32)))
                .collect(),
        })
    }

    pub fn get_open_tickets(&self) -> Result<Vec<mpa_agent::Ticket>, SyncError> {
        let res = Self::check(self.signed("GET", "/api/agent/open-tickets", None).send()?)?;
        let body: TicketsResponse = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        Ok(body
            .tickets
            .into_iter()
            .map(|t| mpa_agent::Ticket {
                id: t.id,
                ticket_number: t.ticket_number,
                // The name matters, not the id: this list is what the operator
                // reads to pick a ticket.
                cashier: t.cashier_name.unwrap_or(t.cashier_id),
                customer: t.customer_name,
                total: t.total,
            })
            .collect())
    }

    /// Drafts the server actually stored, newest first. Read-back for the link
    /// check and for outbox reconciliation.
    /// Server-signed offline entitlement (absolute expiry, never extended
    /// locally). Absent on servers predating the feature.
    pub fn get_entitlement(&self) -> Result<EntitlementToken, SyncError> {
        let res = Self::check(self.signed("GET", "/api/agent/entitlement", None).send()?)?;
        res.json().map_err(|e| SyncError::Parse(e.to_string()))
    }

    pub fn get_recent_jobs(&self) -> Result<Vec<RemoteJob>, SyncError> {
        let res = Self::check(self.signed("GET", "/api/agent/jobs", None).send()?)?;
        let body: JobsResponse = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        Ok(body.jobs)
    }

    /// Push one outbox entry. The server is idempotent on the key and prices
    /// authoritatively; the returned id is stored for reconciliation.
    pub fn post_job(&self, entry: &OutboxEntry) -> Result<PostedJob, SyncError> {
        let body = JobBody {
            idempotency_key: entry.idempotency_key.clone(),
            page_count: entry.pages.len() as i32,
            document_sha256: entry.document_sha256.clone(),
            media: entry.media.clone(),
            target_order_id: match &entry.target {
                TargetSelection::Ticket(id) => Some(id.clone()),
                _ => None,
            },
            mode: entry.mode,
            pages: entry
                .pages
                .iter()
                .map(|p| JobPage {
                    page_no: p.page_no,
                    c: p.coverage.c,
                    m: p.coverage.m,
                    y: p.coverage.y,
                    k: p.coverage.k,
                    coverage_nonwhite: p.area.map(|a| a.area_pct),
                    page_class: p.area.and_then(|a| a.class).map(|c| c.as_str()),
                    include: p.included,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&body).map_err(|e| SyncError::Parse(e.to_string()))?;
        let res = Self::check(self.signed("POST", "/api/agent/jobs", Some(bytes)).send()?)?;
        let value: serde_json::Value = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        let id = value
            .get("jobId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| SyncError::Parse("missing jobId".into()))?;
        let page_count = value
            .get("pageCount")
            .and_then(|v| v.as_i64())
            .map(|n| n as i32);
        Ok(PostedJob { id, page_count })
    }

    /// Prove the operator typed the workspace's pairing code in the agent
    /// window. The code is validated but not consumed, so a handshake for the
    /// local browser channel never blocks a real enrollment.
    pub fn local_grant(&self, code: &str) -> Result<bool, SyncError> {
        let body = serde_json::json!({ "code": code });
        let bytes = serde_json::to_vec(&body).map_err(|e| SyncError::Parse(e.to_string()))?;
        let res = self
            .signed("POST", "/api/agent/local-grant", Some(bytes))
            .send()?;
        match res.status() {
            reqwest::StatusCode::OK => Ok(true),
            reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::FORBIDDEN => Ok(false),
            _ => {
                Self::check(res)?;
                Ok(true)
            }
        }
    }

    /// Prices the analyzed pages in BOTH modes without storing anything: the
    /// operator sees what each printer role would cost before choosing.
    pub fn quote(&self, pages: &[QuotePage]) -> Result<Quote, SyncError> {
        let body = serde_json::json!({ "pages": pages });
        let bytes = serde_json::to_vec(&body).map_err(|e| SyncError::Parse(e.to_string()))?;
        let res = Self::check(
            self.signed("POST", "/api/agent/quote", Some(bytes))
                .send()?,
        )?;
        res.json().map_err(|e| SyncError::Parse(e.to_string()))
    }

    /// Inject a pushed draft into its ticket as order lines. Idempotent
    /// server-side: replaying returns the same summary without duplicating.
    pub fn materialize_job(&self, job_id: &str) -> Result<MaterializeAck, SyncError> {
        let path = format!("/api/agent/jobs/{job_id}/materialize");
        let res = Self::check(self.signed("POST", &path, Some(b"{}".to_vec())).send()?)?;
        res.json().map_err(|e| SyncError::Parse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_quote_with_both_modes() {
        // Shape produced by POST /api/agent/quote.
        let raw = r#"{
            "metrics": {"BW": "K_ONLY", "COLOR": "SUM_CMYK"},
            "modes": {
                "BW": {"total": 4.0, "pageCount": 1, "unpriced": [], "lines": [
                    {"pageNo": 1, "coverage": 62.5, "tierId": "t1", "tierLabel": "B/N 0-100",
                     "productId": "p1", "productName": "IMPRESIÓN B/N", "unitPrice": 2.0}
                ]},
                "COLOR": {"total": 6.5, "pageCount": 0, "unpriced": [2], "lines": []}
            }
        }"#;

        let quote: Quote = serde_json::from_str(raw).expect("quote parses");
        assert_eq!(quote.for_mode(PrintMode::Bw).total, 4.0);
        assert_eq!(
            quote.for_mode(PrintMode::Bw).lines[0]
                .product_name
                .as_deref(),
            Some("IMPRESIÓN B/N")
        );
        assert_eq!(quote.for_mode(PrintMode::Color).total, 6.5);
        assert_eq!(quote.for_mode(PrintMode::Color).unpriced, vec![2]);
    }

    #[test]
    fn a_quote_without_modes_is_empty_not_an_error() {
        let quote: Quote = serde_json::from_str("{}").expect("empty quote parses");
        assert_eq!(quote.for_mode(PrintMode::Bw).total, 0.0);
        assert_eq!(quote.for_mode(PrintMode::Color).page_count, 0);
    }

    #[test]
    fn a_tier_without_a_mode_is_treated_as_color() {
        // Servers predating Fase 1 only had color rules.
        let raw = r#"{"tiers": [{"id": "t1", "label": "x", "minCoverage": 0, "maxCoverage": 50,
            "productId": "p1", "isActive": true, "sortOrder": 0}],
            "metrics": {"BW": "K_ONLY", "COLOR": "SUM_CMYK"},
            "pricing": {"intensityThreshold": 5, "greysAsColor": false,
                        "blankPageFree": true, "paperSurcharge": 0.5,
                        "volumeDiscounts": [{"minPages": 20, "percent": 10}]}}"#;
        let response: TiersResponse = serde_json::from_str(raw).expect("tiers parse");
        assert_eq!(response.tiers[0].to_domain().mode, PrintMode::Color);
        assert_eq!(response.metrics.bw, mpa_domain::PrintMetric::KOnly);
        assert_eq!(response.metrics.color, mpa_domain::PrintMetric::SumCmyk);
        // Fase 6 rules ride the same payload; the agent labels with them.
        assert_eq!(response.pricing.intensity_threshold, 5.0);
        assert!(!response.pricing.greys_as_color);
        assert_eq!(response.pricing.paper_surcharge, 0.5);
        assert_eq!(response.pricing.volume_discount_percent(25), 10.0);
    }
}
