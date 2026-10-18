use mpa_agent::{OutboxEntry, TargetSelection};
use mpa_domain::Tier;
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
    tiers: Vec<RemoteTier>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTier {
    pub id: String,
    pub label: String,
    pub min_coverage: f64,
    pub max_coverage: f64,
    pub product_id: String,
    pub product_name: Option<String>,
    pub price: Option<f64>,
    pub is_active: bool,
    pub sort_order: i32,
}

impl RemoteTier {
    pub fn to_domain(&self) -> Tier {
        Tier {
            id: self.id.clone(),
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
    pages: Vec<JobPage>,
}

/// Thin client for the Fase 0 contract (`/api/agent/*`). Bearer key on
/// every call except `pair` and `version`.
pub struct SyncClient {
    base_url: String,
    agent_key: Option<String>,
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
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            agent_key,
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
        let res = http.post(&url).json(&PairBody { code, pc_name }).send()?;
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
        })
    }

    fn auth(&self, req: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        match &self.agent_key {
            Some(key) => req.bearer_auth(key),
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
        let url = format!("{}/api/agent/tiers", self.base_url);
        let res = Self::check(self.auth(self.http.get(&url)).send()?)?;
        let body: TiersResponse = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        Ok(body.tiers)
    }

    pub fn get_open_tickets(&self) -> Result<Vec<mpa_agent::Ticket>, SyncError> {
        let url = format!("{}/api/agent/open-tickets", self.base_url);
        let res = Self::check(self.auth(self.http.get(&url)).send()?)?;
        let body: TicketsResponse = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        Ok(body
            .tickets
            .into_iter()
            .map(|t| mpa_agent::Ticket {
                id: t.id,
                ticket_number: t.ticket_number,
                cashier: t.cashier_id,
                total: t.total,
            })
            .collect())
    }

    /// Drafts the server actually stored, newest first. Read-back for the link
    /// check and for outbox reconciliation.
    pub fn get_recent_jobs(&self) -> Result<Vec<RemoteJob>, SyncError> {
        let url = format!("{}/api/agent/jobs", self.base_url);
        let res = Self::check(self.auth(self.http.get(&url)).send()?)?;
        let body: JobsResponse = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        Ok(body.jobs)
    }

    /// Push one outbox entry. The server is idempotent on the key and prices
    /// authoritatively; the returned id is stored for reconciliation.
    pub fn post_job(&self, entry: &OutboxEntry) -> Result<String, SyncError> {
        let url = format!("{}/api/agent/jobs", self.base_url);
        let body = JobBody {
            idempotency_key: entry.idempotency_key.clone(),
            target_order_id: match &entry.target {
                TargetSelection::Ticket(id) => Some(id.clone()),
                _ => None,
            },
            pages: entry
                .pages
                .iter()
                .map(|p| JobPage {
                    page_no: p.page_no,
                    c: p.coverage.c,
                    m: p.coverage.m,
                    y: p.coverage.y,
                    k: p.coverage.k,
                    include: p.included,
                })
                .collect(),
        };
        let res = Self::check(self.auth(self.http.post(&url)).json(&body).send()?)?;
        let value: serde_json::Value = res.json().map_err(|e| SyncError::Parse(e.to_string()))?;
        value
            .get("jobId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| SyncError::Parse("missing jobId".into()))
    }

    /// Inject a pushed draft into its ticket as order lines. Idempotent
    /// server-side: replaying returns the same summary without duplicating.
    pub fn materialize_job(&self, job_id: &str) -> Result<MaterializeAck, SyncError> {
        let url = format!("{}/api/agent/jobs/{job_id}/materialize", self.base_url);
        let res = Self::check(self.auth(self.http.post(&url)).send()?)?;
        res.json().map_err(|e| SyncError::Parse(e.to_string()))
    }
}
