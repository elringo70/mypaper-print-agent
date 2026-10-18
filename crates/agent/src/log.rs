use std::path::PathBuf;

/// Best-effort file logging for field diagnostics. The console window is
/// invisible in release builds (`windows_subsystem`), so lifecycle milestones
/// and errors go here too. Never fails the caller.
pub fn default_log_path() -> Result<PathBuf, crate::config::ConfigError> {
    if let Ok(env) = std::env::var("MPA_LOG_FILE") {
        if !env.trim().is_empty() {
            return Ok(PathBuf::from(env));
        }
    }
    let mut path = crate::config::config_path(None)?;
    path.set_file_name("mpa.log");
    Ok(path)
}

fn write_line(level: &str, msg: &str) {
    let path = match default_log_path() {
        Ok(path) => path,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{ts} [{level}] {msg}");
    }
}

pub fn info(msg: String) {
    write_line("INFO", &msg);
    eprintln!("mpa: {msg}");
}

pub fn warn(msg: String) {
    write_line("WARN", &msg);
    eprintln!("mpa: WARN {msg}");
}

pub fn error(msg: String) {
    write_line("ERROR", &msg);
    eprintln!("mpa: ERROR {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_lines_without_failing() {
        let dir = std::env::temp_dir().join("mpa-log-unit");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mpa.log");
        let _ = std::fs::remove_file(&path);
        std::env::set_var("MPA_LOG_FILE", &path);
        info("hello".into());
        warn("careful".into());
        error("boom".into());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[INFO] hello"));
        assert!(text.contains("[WARN] careful"));
        assert!(text.contains("[ERROR] boom"));
        std::env::remove_var("MPA_LOG_FILE");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
