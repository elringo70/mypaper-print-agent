use crate::config::config_path;
use crate::core::{AgentCore, AgentMode, OutboxEntry};
use mpa_domain::Tier;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Durable slice of the core: mode, tiers and the outbox. Held spool jobs
/// are ephemeral (their handles die with the process) and never persisted.
#[derive(Debug, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    mode: AgentMode,
    #[serde(default)]
    tiers: Vec<Tier>,
    /// Billing metrics per mode; missing in states written before Fase 8, which
    /// fall back to the per-mode defaults.
    #[serde(default)]
    metrics: mpa_domain::MetricsByMode,
    /// Fase 6 billing rules; missing in states written before then, which fall
    /// back to the historical behaviour (greys are color, nothing free).
    #[serde(default)]
    pricing: mpa_domain::PrintPricing,
    /// Tier prices from the last sync; missing in older states.
    #[serde(default)]
    tier_prices: std::collections::HashMap<String, f32>,
    #[serde(default)]
    outbox: Vec<OutboxEntry>,
}

/// `agent-state.json` next to `config.toml`. Overridable per-call for tests.
pub fn default_state_path() -> Result<PathBuf, crate::config::ConfigError> {
    let mut path = config_path(None)?;
    path.set_file_name("agent-state.json");
    Ok(path)
}

pub fn save_to(core: &AgentCore, path: &PathBuf) -> std::io::Result<()> {
    let state = PersistedState {
        mode: core.mode(),
        tiers: core.tiers().to_vec(),
        metrics: core.metrics(),
        pricing: core.pricing(),
        tier_prices: core.tier_prices(),
        outbox: core.outbox().to_vec(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.clone();
    tmp.set_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&state).expect("state serializes"),
    )?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Loads mode/tiers/outbox into a fresh-or-live core. Missing file = defaults.
pub fn load_into(core: &mut AgentCore, path: &PathBuf) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let state: PersistedState = serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    core.set_mode(state.mode);
    if !state.tiers.is_empty() {
        core.set_tiers(state.tiers);
    }
    core.set_metrics(state.metrics);
    core.set_pricing(state.pricing);
    core.set_tier_prices(state.tier_prices);
    core.set_outbox(state.outbox);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{OutboxEntry, OutboxPage, TargetSelection};
    use mpa_domain::Coverage;

    #[test]
    fn roundtrip_mode_tiers_outbox() {
        let dir = std::env::temp_dir().join("mpa-persist-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-state.json");

        let backend: std::sync::Arc<dyn mpa_spool::SpoolBackend> =
            std::sync::Arc::new(mpa_spool::MockSpool::new());
        let mut core = AgentCore::new(
            backend,
            std::sync::Arc::new(crate::tickets::MockTickets::demo()),
            vec![mpa_domain::Tier {
                id: "t6".into(),
                mode: mpa_domain::PrintMode::Color,
                min_coverage: 0.0,
                max_coverage: 50.0,
                active: true,
            }],
        );
        core.set_mode(AgentMode::Suspended);
        core.set_outbox(vec![OutboxEntry {
            idempotency_key: "k-1".into(),
            printer: "P".into(),
            job_id: 1,
            target: TargetSelection::NewDraft,
            pages: vec![OutboxPage {
                page_no: 1,
                coverage: Coverage::new(1.0, 2.0, 3.0, 4.0),
                tier_id: Some("t6".into()),
                included: true,
                area: None,
            }],
            format: Some(mpa_analyzer::SpoolFormat::Image),
            spool_path: None,
            mode: mpa_domain::PrintMode::Bw,
            deliver_printer: None,
            options: None,
            applied_settings: None,
            reprint_pending: false,
            reprint_attempts: 0,
            document_sha256: None,
            synced: true,
            remote_id: Some("job-1".into()),
            materialized: true,
        }]);
        save_to(&core, &path).unwrap();

        let backend2: std::sync::Arc<dyn mpa_spool::SpoolBackend> =
            std::sync::Arc::new(mpa_spool::MockSpool::new());
        let mut core2 = AgentCore::new(
            backend2,
            std::sync::Arc::new(crate::tickets::MockTickets::demo()),
            vec![],
        );
        load_into(&mut core2, &path).unwrap();
        assert_eq!(core2.mode(), AgentMode::Suspended);
        assert_eq!(core2.tiers().len(), 1);
        assert_eq!(core2.outbox().len(), 1);
        assert_eq!(core2.outbox()[0].remote_id.as_deref(), Some("job-1"));

        // Missing file = clean defaults, no error.
        load_into(&mut core2, &dir.join("nope.json")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
