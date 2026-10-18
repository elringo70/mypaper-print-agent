use serde::Serialize;
use std::path::PathBuf;

/// Local accountability trail: every interception decision, analysis outcome,
/// completion/cancel and sync summary, one JSON object per line. The server
/// keeps the billing truth; this file answers "who held what, when" on the
/// PC itself. All writes are best-effort and never fail the operation.
#[derive(Debug, Serialize)]
struct AuditEntry {
    ts: String,
    event: &'static str,
    printer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_id: Option<u32>,
    #[serde(default, skip_serializing_if = "str::is_empty")]
    detail: String,
}

/// Append-only log next to the config file. Overridable with
/// `MPA_AUDIT_FILE` (tests and portable installs).
pub fn default_audit_path() -> Result<PathBuf, crate::config::ConfigError> {
    if let Ok(env) = std::env::var("MPA_AUDIT_FILE") {
        if !env.trim().is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    #[cfg(test)]
    {
        Ok(std::env::temp_dir().join("mpa-audit-test.jsonl"))
    }
    #[cfg(not(test))]
    {
        let mut path = crate::config::config_path(None)?;
        path.set_file_name("audit.jsonl");
        Ok(path)
    }
}

/// Single backup rotation: past 10 MiB the log rolls to `audit.1.jsonl`.
const MAX_BYTES: u64 = 10 * 1024 * 1024;

pub fn record(event: &'static str, printer: &str, job_id: Option<u32>, detail: String) {
    let path = match default_audit_path() {
        Ok(path) => path,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_BYTES {
            let backup = path.with_extension("jsonl.1");
            let _ = std::fs::remove_file(&backup);
            let _ = std::fs::rename(&path, &backup);
        }
    }
    let entry = AuditEntry {
        ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        event,
        printer: printer.into(),
        job_id,
        detail,
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(
            file,
            "{}",
            serde_json::to_string(&entry).unwrap_or_default()
        );
    }
}

/// Last `limit` lines, oldest first. Unparseable lines pass through raw so a
/// corrupt tail never hides the readable rest.
pub fn tail_lines(path: &PathBuf, limit: usize) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(limit.max(1));
    lines[start..]
        .iter()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .map(|v| {
                    format!(
                        "{} {} {} {}",
                        v.get("ts").and_then(|t| t.as_str()).unwrap_or("?"),
                        v.get("event").and_then(|t| t.as_str()).unwrap_or("?"),
                        v.get("printer").and_then(|t| t.as_str()).unwrap_or("-"),
                        v.get("detail").and_then(|t| t.as_str()).unwrap_or(""),
                    )
                    .trim_end()
                    .to_string()
                })
                .unwrap_or_else(|_| line.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_back() {
        let dir = std::env::temp_dir().join("mpa-audit-unit");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.jsonl");
        let _ = std::fs::remove_file(&path);
        // `MPA_AUDIT_FILE` is process-global and every other unit test in this
        // binary that records an audit line writes into this same file while
        // we run, so assert on OUR records by content: counting lines or
        // trusting a position fails on whichever parallel test appends first.
        std::env::set_var("MPA_AUDIT_FILE", &path);
        record("paused", "P1", Some(7), "held 3 pages".into());
        record("synced", "P1", None, "1 pushed".into());
        let lines = tail_lines(&path, 50);
        std::env::remove_var("MPA_AUDIT_FILE");
        assert!(lines
            .iter()
            .any(|l| l.contains("paused") && l.contains("P1") && l.contains("held 3 pages")));
        assert!(lines
            .iter()
            .any(|l| l.contains("synced") && l.contains("P1") && l.contains("1 pushed")));
        // The tail limit still truncates.
        assert_eq!(tail_lines(&path, 1).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
