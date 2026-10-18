use semver::Version;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("bad version in manifest: {0}")]
    Version(String),
    #[error("bad manifest: {0}")]
    Parse(String),
}

/// Release manifest. Served either as a standalone `latest.json` or embedded
/// in MyPaper's `GET /api/agent/version` as the `release` object (fields
/// come from the server's `AGENT_RELEASE_*` env).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseManifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub signature: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate {
        version: String,
    },
    Available {
        manifest: ReleaseManifest,
    },
    /// The channel answered but carries no release (server not configured).
    NoChannel,
}

impl ReleaseManifest {
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        serde_json::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))
    }

    /// Extract the embedded manifest from a MyPaper version response.
    /// Returns `Ok(None)` when the server has no release configured.
    pub fn from_version_response(text: &str) -> Result<Option<Self>, ManifestError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))?;
        match value.get("release") {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(release) => serde_json::from_value(release.clone())
                .map(Some)
                .map_err(|e| ManifestError::Parse(e.to_string())),
        }
    }

    pub fn check_against(&self, current: &str) -> Result<UpdateStatus, ManifestError> {
        let remote = Version::parse(self.version.trim_start_matches('v'))
            .map_err(|_| ManifestError::Version(self.version.clone()))?;
        let local = Version::parse(current.trim_start_matches('v'))
            .map_err(|_| ManifestError::Version(current.into()))?;
        if remote > local {
            Ok(UpdateStatus::Available {
                manifest: self.clone(),
            })
        } else {
            Ok(UpdateStatus::UpToDate {
                version: current.into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
        "version": "0.2.0",
        "url": "https://example.test/mpa-0.2.0-setup.exe",
        "sha256": "abc123",
        "signature": "sig",
        "notes": "fixes"
    }"#;

    #[test]
    fn detects_newer_remote() {
        let manifest = ReleaseManifest::parse(MANIFEST).unwrap();
        assert!(matches!(
            manifest.check_against("0.1.0"),
            Ok(UpdateStatus::Available { .. })
        ));
    }

    #[test]
    fn same_or_older_is_up_to_date() {
        let manifest = ReleaseManifest::parse(MANIFEST).unwrap();
        assert!(matches!(
            manifest.check_against("0.2.0"),
            Ok(UpdateStatus::UpToDate { .. })
        ));
        assert!(matches!(
            manifest.check_against("0.3.0"),
            Ok(UpdateStatus::UpToDate { .. })
        ));
    }

    #[test]
    fn tolerates_v_prefixes() {
        let manifest = ReleaseManifest::parse(MANIFEST).unwrap();
        assert!(matches!(
            manifest.check_against("v0.1.0"),
            Ok(UpdateStatus::Available { .. })
        ));
    }

    #[test]
    fn reads_embedded_release_from_version_endpoint() {
        let body = r#"{"contractVersion":"0","release":null}"#;
        assert!(ReleaseManifest::from_version_response(body)
            .unwrap()
            .is_none());

        let body = format!(r#"{{"contractVersion":"0","release":{MANIFEST}}}"#);
        let manifest = ReleaseManifest::from_version_response(&body)
            .unwrap()
            .unwrap();
        assert_eq!(manifest.version, "0.2.0");
    }

    #[test]
    fn rejects_garbage() {
        assert!(ReleaseManifest::parse("not json").is_err());
        let bad = MANIFEST.replace("0.2.0", "tomorrow");
        assert!(ReleaseManifest::parse(&bad)
            .unwrap()
            .check_against("0.1.0")
            .is_err());
    }
}
