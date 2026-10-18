use crate::client::Pairing;
use mpa_agent::{config_path, ConfigError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persisted pairing: server + Bearer key. Stored in `agent.json` next to
/// `config.toml` with owner-only permissions on Unix. (Windows Credential
/// Manager migration is tracked hardening, not Phase 4 scope.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingStore {
    pub server_url: String,
    pub agent_id: String,
    pub workspace_id: String,
    pub agent_key: String,
}

impl PairingStore {
    pub fn from_pairing(server_url: &str, pairing: Pairing) -> Self {
        Self {
            server_url: server_url.trim_end_matches('/').to_string(),
            agent_id: pairing.agent_id,
            workspace_id: pairing.workspace_id,
            agent_key: pairing.agent_key,
        }
    }

    /// Explicit env wins (CI/tests), then the file.
    pub fn load(override_path: Option<PathBuf>) -> Result<Self, PairingError> {
        if let (Ok(server_url), Ok(agent_key)) = (
            std::env::var("MPA_SERVER_URL"),
            std::env::var("MPA_AGENT_KEY"),
        ) {
            if !server_url.trim().is_empty() && !agent_key.trim().is_empty() {
                return Ok(Self {
                    server_url,
                    agent_id: String::new(),
                    workspace_id: String::new(),
                    agent_key,
                });
            }
        }
        let path = override_path.unwrap_or(default_pairing_path()?);
        let text = std::fs::read_to_string(&path).map_err(|_| PairingError::NotPaired)?;
        serde_json::from_str(&text).map_err(|_| PairingError::NotPaired)
    }

    pub fn save(&self, override_path: Option<PathBuf>) -> Result<PathBuf, PairingError> {
        let path = override_path.unwrap_or(default_pairing_path()?);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(PairingError::Io)?;
        }
        let mut tmp = path.clone();
        tmp.set_extension("tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(self).expect("pairing serializes"),
        )
        .map_err(PairingError::Io)?;
        restrict(&tmp)?;
        std::fs::rename(&tmp, &path).map_err(PairingError::Io)?;
        Ok(path)
    }

    pub fn clear(override_path: Option<PathBuf>) -> Result<(), PairingError> {
        let path = override_path.unwrap_or(default_pairing_path()?);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PairingError::Io(e)),
        }
    }
}

fn default_pairing_path() -> Result<PathBuf, PairingError> {
    let mut path = config_path(None).map_err(|_| PairingError::NotPaired)?;
    path.set_file_name("agent.json");
    Ok(path)
}

#[cfg(unix)]
fn restrict(path: &PathBuf) -> Result<(), PairingError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(PairingError::Io)
}

#[cfg(not(unix))]
fn restrict(_path: &PathBuf) -> Result<(), PairingError> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    #[error("not paired: run `mpa pair <code>` first")]
    NotPaired,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("config: {0}")]
    Config(String),
}

impl From<ConfigError> for PairingError {
    fn from(e: ConfigError) -> Self {
        PairingError::Config(e.to_string())
    }
}
