use mpa_domain::{
    MetricsByMode, PolicyMode, PrintMetric, PrintMode, PrintPreset, PrinterPolicy, PrinterRoles,
    Tier,
};
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
    /// Missing in configs written before Fase 1: those were color rule sets.
    #[serde(default = "default_tier_mode")]
    mode: PrintMode,
    #[serde(default)]
    min: f32,
    #[serde(default)]
    max: f32,
}

fn default_tier_mode() -> PrintMode {
    PrintMode::Color
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetricsFile {
    #[serde(default)]
    bw: PrintMetric,
    #[serde(default = "default_color_metric")]
    color: PrintMetric,
}

impl Default for MetricsFile {
    fn default() -> Self {
        Self {
            bw: PrintMetric::KOnly,
            color: PrintMetric::SumCmyk,
        }
    }
}

fn default_color_metric() -> PrintMetric {
    PrintMetric::SumCmyk
}

impl From<MetricsFile> for MetricsByMode {
    fn from(value: MetricsFile) -> Self {
        Self {
            bw: value.bw,
            color: value.color,
        }
    }
}

impl From<&MetricsByMode> for MetricsFile {
    fn from(value: &MetricsByMode) -> Self {
        Self {
            bw: value.bw,
            color: value.color,
        }
    }
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
    /// Billing metric per mode, synced with the tabulador.
    #[serde(default)]
    metrics: MetricsFile,
    /// Mono / color printer roles (see `PrinterRoles`).
    #[serde(default)]
    printers: PrinterRoles,
    /// Named print options the operator picks at print time (Fase 4).
    #[serde(default)]
    presets: Vec<PrintPreset>,
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
            mode: t.mode,
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

/// Persist the printer roles (mono / color) into `config.toml`.
pub fn save_roles(roles: &PrinterRoles, path: &PathBuf) -> Result<(), ConfigError> {
    let mut file = if path.exists() {
        toml::from_str::<FileConfig>(&std::fs::read_to_string(path)?).unwrap_or_default()
    } else {
        FileConfig::default()
    };
    file.printers = roles.clone();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&file)?)?;
    Ok(())
}

/// Printer roles from `config.toml`; missing block = nothing assigned yet.
pub fn load_roles(path: &std::path::Path) -> PrinterRoles {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<FileConfig>(&text).ok())
        .map(|file| file.printers)
        .unwrap_or_default()
}

/// Persist the print presets (Fase 4) into `config.toml`.
pub fn save_presets(presets: &[PrintPreset], path: &PathBuf) -> Result<(), ConfigError> {
    let mut file = if path.exists() {
        toml::from_str::<FileConfig>(&std::fs::read_to_string(path)?).unwrap_or_default()
    } else {
        FileConfig::default()
    };
    file.presets = presets.to_vec();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&file)?)?;
    Ok(())
}

/// Print presets from `config.toml`; missing block = none configured.
pub fn load_presets(path: &std::path::Path) -> Vec<PrintPreset> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<FileConfig>(&text).ok())
        .map(|file| file.presets)
        .unwrap_or_default()
}

/// Persist the billing metrics alongside the tabulador.
pub fn save_metrics(metrics: &MetricsByMode, path: &PathBuf) -> Result<(), ConfigError> {
    let mut file = if path.exists() {
        toml::from_str::<FileConfig>(&std::fs::read_to_string(path)?).unwrap_or_default()
    } else {
        FileConfig::default()
    };
    file.metrics = MetricsFile::from(metrics);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&file)?)?;
    Ok(())
}

/// Billing metrics from `config.toml`; missing block = per-mode defaults.
pub fn load_metrics(path: &std::path::Path) -> MetricsByMode {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<FileConfig>(&text).ok())
        .map(|file| MetricsByMode::from(file.metrics))
        .unwrap_or_default()
}

/// Tiers synced from MyPaper; until the first sync, a local fallback with one
/// rule set per mode so both printer roles can always classify a page.
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
            mode: t.mode,
            min_coverage: t.min,
            max_coverage: t.max,
            active: true,
        })
        .collect();
    if tiers.is_empty() {
        vec![
            Tier {
                id: "BW".into(),
                mode: PrintMode::Bw,
                min_coverage: 0.0,
                max_coverage: 100.0,
                active: true,
            },
            Tier {
                id: "COLOR".into(),
                mode: PrintMode::Color,
                min_coverage: 0.01,
                max_coverage: 100.0,
                active: true,
            },
        ]
    } else {
        tiers
    }
}
