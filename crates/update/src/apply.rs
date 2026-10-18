use crate::verify::{verify_asset, VerifyError};
use mpa_domain::ReleaseManifest;
use mpa_sync::SyncClient;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error(transparent)]
    Sync(#[from] mpa_sync::SyncError),
    #[error(transparent)]
    Verify(#[from] VerifyError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("spawn installer: {0}")]
    Spawn(String),
}

/// Where the verified asset lands before handoff.
pub fn stage_asset(bytes: &[u8], file_name: &str) -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("mpa-update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(file_name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Download + verify a release. Returns the staged installer path; the
/// caller decides how to hand off (installer spawn on Windows, file drop
/// elsewhere).
pub fn fetch_verified(
    client: &SyncClient,
    manifest: &ReleaseManifest,
) -> Result<PathBuf, ApplyError> {
    let bytes = client.get_bytes(&manifest.url)?;
    verify_asset(&bytes, &manifest.sha256, &manifest.signature)?;
    let file_name = manifest
        .url
        .rsplit(['/', '\\'])
        .next()
        .filter(|n: &&str| !n.is_empty())
        .unwrap_or("mpa-setup.exe");
    Ok(stage_asset(&bytes, file_name)?)
}

/// Launch the staged installer and exit so it can replace our files.
/// Windows installers cannot overwrite a running exe; the installer owns
/// the relaunch (it carries the autostart entry already).
pub fn launch_installer_and_exit(path: &PathBuf, extra_args: &[String]) -> Result<(), ApplyError> {
    std::process::Command::new(path)
        .args(extra_args)
        .spawn()
        .map_err(|e| ApplyError::Spawn(e.to_string()))?;
    // Give the installer a beat to detach before our handles close.
    std::thread::sleep(std::time::Duration::from_millis(500));
    std::process::exit(0);
}

/// Portable fallback (no installer): swap the running exe for the new one.
/// Windows refuses to overwrite a running image, so rename-then-move while
/// a `.old` copy keeps the rollback path.
pub fn replace_running_exe(new_bytes: &[u8]) -> Result<PathBuf, ApplyError> {
    let current = std::env::current_exe()?;
    let backup = current.with_extension("exe.old");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&current, &backup)?;
    if let Err(e) = std::fs::write(&current, new_bytes) {
        let _ = std::fs::rename(&backup, &current);
        return Err(ApplyError::Io(e));
    }
    Ok(backup)
}
