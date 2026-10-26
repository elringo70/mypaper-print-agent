use crate::client::Pairing;
use crate::sign::DeviceKey;
use base64::Engine;
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
    /// Base64 of the 32-byte Ed25519 secret. Absent on pairings made before
    /// request signing existed; `device_key` returns an error until the device
    /// re-pairs, which is the safe outcome.
    #[serde(default)]
    pub device_secret: String,
    /// Base64 of the matching public key, sent at pairing time.
    #[serde(default)]
    pub device_public: String,
}

impl PairingStore {
    pub fn from_pairing(server_url: &str, pairing: Pairing) -> Self {
        let device_public = pairing
            .device_secret
            .as_deref()
            .and_then(|encoded| {
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .ok()
            })
            .and_then(|bytes| DeviceKey::from_secret(&bytes).ok())
            .map(|key| key.public_b64())
            .unwrap_or_default();
        Self {
            server_url: server_url.trim_end_matches('/').to_string(),
            agent_id: pairing.agent_id,
            workspace_id: pairing.workspace_id,
            agent_key: pairing.agent_key,
            device_secret: pairing.device_secret.unwrap_or_default(),
            device_public,
        }
    }

    /// The device identity, when this pairing has one.
    pub fn device_key(&self) -> Result<DeviceKey, PairingError> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(self.device_secret.trim())
            .map_err(|_| PairingError::NoDeviceKey)?;
        DeviceKey::from_secret(&raw).map_err(|_| PairingError::NoDeviceKey)
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
                    device_secret: String::new(),
                    device_public: String::new(),
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
    #[error("this device has no signing key: re-run `mpa pair <code>` to get one")]
    NoDeviceKey,
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
