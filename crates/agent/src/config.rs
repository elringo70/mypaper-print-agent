use mpa_domain::{PolicyMode, PrinterPolicy, Tier};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot resolve config dir")]
    NoConfigDir,
    #[error("config io: {0}")]
    Io(#[from] std::io::Error),
    #[error("config parse: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Allowlist,
    #[default]
    Denylist,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TierEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    min: f32,
    #[serde(default)]
    max: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FileConfig {
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    watched: Vec<String>,
    #[serde(default)]
    ignored: Vec<String>,
    #[serde(default)]
    tiers: Vec<TierEntry>,
}

/// `%APPDATA%/mypaper-print-agent/config.toml` on Windows,
/// `$XDG_CONFIG_HOME` (or `~/.config`) elsewhere. Overridable with
/// `--config` or `MPA_CONFIG`.
pub fn config_path(override_path: Option<PathBuf>) -> Result<PathBuf, ConfigError> {
    if let Some(p) = override_path {
        return Ok(p);
    }
    if let Ok(env) = std::env::var("MPA_CONFIG") {
        if !env.trim().is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    let mut dir = dirs::config_dir().ok_or(ConfigError::NoConfigDir)?;
    dir.push("mypaper-print-agent");
    dir.push("config.toml");
    Ok(dir)
}

pub fn load_policy(
    override_path: Option<PathBuf>,
) -> Result<(PrinterPolicy, PathBuf), ConfigError> {
    let path = config_path(override_path)?;
    if !path.exists() {
        return Ok((PrinterPolicy::default(), path));
    }
    let text = std::fs::read_to_string(&path)?;
    let file: FileConfig = toml::from_str(&text)?;
    Ok((
        PrinterPolicy {
            mode: match file.mode {
                Mode::Allowlist => PolicyMode::Allowlist,
                Mode::Denylist => PolicyMode::Denylist,
            },
            watched: file.watched,
            ignored: file.ignored,
        },
        path,
    ))
}

pub fn save_policy(policy: &PrinterPolicy, path: &PathBuf) -> Result<(), ConfigError> {
    let mut file = if path.exists() {
        toml::from_str::<FileConfig>(&std::fs::read_to_string(path)?).unwrap_or_default()
    } else {
        FileConfig::default()
    };
    file.mode = match policy.mode {
        PolicyMode::Allowlist => Mode::Allowlist,
        PolicyMode::Denylist => Mode::Denylist,
    };
    file.watched = policy.watched.clone();
    file.ignored = policy.ignored.clone();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&file)?)?;
    Ok(())
}

/// Persist server tiers as the offline fallback (`[[tiers]]` next to policy).
pub fn save_tiers(tiers: &[Tier], path: &PathBuf) -> Result<(), ConfigError> {
    let mut file = if path.exists() {
        toml::from_str::<FileConfig>(&std::fs::read_to_string(path)?).unwrap_or_default()
    } else {
        FileConfig::default()
    };
    file.tiers = tiers
        .iter()
        .map(|t| TierEntry {
            id: t.id.clone(),
            min: t.min_coverage,
            max: t.max_coverage,
        })
        .collect();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&file)?)?;
    Ok(())
}

/// Tiers synced from MyPaper in Phase 4; until then, optional local `[[tiers]]`.
pub fn load_tiers(path: &std::path::Path) -> Vec<Tier> {
    let file: FileConfig = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default();
    let tiers: Vec<Tier> = file
        .tiers
        .into_iter()
        .filter(|t| !t.id.is_empty() && t.min <= t.max)
        .map(|t| Tier {
            id: t.id,
            min_coverage: t.min,
            max_coverage: t.max,
            active: true,
        })
        .collect();
    if tiers.is_empty() {
        vec![
            Tier {
                id: "BN".into(),
                min_coverage: 0.0,
                max_coverage: 5.0,
                active: true,
            },
            Tier {
                id: "COLOR50".into(),
                min_coverage: 5.01,
                max_coverage: 60.0,
                active: true,
            },
            Tier {
                id: "FOTO".into(),
                min_coverage: 60.01,
                max_coverage: 100.0,
                active: true,
            },
        ]
    } else {
        tiers
    }
}
