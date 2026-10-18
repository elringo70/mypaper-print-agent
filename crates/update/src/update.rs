use mpa_domain::{ReleaseManifest, UpdateStatus};
use mpa_sync::{SyncClient, SyncError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error(transparent)]
    Sync(#[from] SyncError),
    #[error(transparent)]
    Manifest(#[from] mpa_domain::ManifestError),
    #[error("no release channel: pair first or set MPA_UPDATE_MANIFEST")]
    NoChannel,
}

/// Check for a newer release. Standalone manifests (`MPA_UPDATE_MANIFEST`)
/// win; otherwise the paired server's version endpoint is read for its
/// embedded `release` object. A check never touches disk; downloading
/// happens only in the apply step.
pub fn check_for_update(
    server_url: Option<&str>,
    manifest_url: Option<&str>,
    agent_key: Option<&str>,
    current_version: &str,
) -> Result<UpdateStatus, UpdateError> {
    let base = manifest_url.or(server_url).ok_or(UpdateError::NoChannel)?;
    let client = SyncClient::new(base, agent_key.map(|key| key.to_string()))?;

    if let Some(url) = manifest_url {
        let manifest = ReleaseManifest::parse(&client.get_text(url)?)?;
        return Ok(manifest.check_against(current_version)?);
    }
    let server = server_url.ok_or(UpdateError::NoChannel)?;
    let url = format!("{}/api/agent/version", server.trim_end_matches('/'));
    match ReleaseManifest::from_version_response(&client.get_text(&url)?)? {
        Some(manifest) => Ok(manifest.check_against(current_version)?),
        None => Ok(UpdateStatus::NoChannel),
    }
}
