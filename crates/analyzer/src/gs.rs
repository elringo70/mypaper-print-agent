use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GsError {
    #[error("ghostscript not found (set MPA_GHOSTSCRIPT or install it)")]
    NotFound,
    #[error("ghostscript failed: {0}")]
    Failed(String),
    #[error("cannot parse inkcov output: {0}")]
    Parse(String),
}

/// Located Ghostscript interpreter (`gswin64c.exe` on Windows, `gs` elsewhere).
#[derive(Debug, Clone)]
pub struct Ghostscript {
    program: PathBuf,
}

impl Ghostscript {
    /// `MPA_GHOSTSCRIPT` wins, then PATH, then well-known install dirs.
    pub fn discover() -> Result<Self, GsError> {
        if let Ok(env) = std::env::var("MPA_GHOSTSCRIPT") {
            if !env.trim().is_empty() {
                return Ok(Self {
                    program: PathBuf::from(env),
                });
            }
        }
        #[cfg(windows)]
        const CANDIDATES: &[&str] = &["gswin64c.exe", "gswin32c.exe", "gs.exe"];
        #[cfg(not(windows))]
        const CANDIDATES: &[&str] = &["gs"];
        for name in CANDIDATES {
            if probe(name) {
                return Ok(Self {
                    program: PathBuf::from(name),
                });
            }
        }
        #[cfg(windows)]
        if let Some(found) = search_program_files() {
            return Ok(Self { program: found });
        }
        Err(GsError::NotFound)
    }

    pub fn program(&self) -> &std::path::Path {
        &self.program
    }

    /// Placeholder interpreter for paths that never spawn (unit tests).
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            program: PathBuf::from("gs-missing-for-tests"),
        }
    }

    /// Per-page CMYK coverage fractions via the `inkcov` device.
    /// `input` must be a file Ghostscript reads (PDF/PS).
    pub fn inkcov(&self, input: &std::path::Path) -> Result<Vec<[f32; 4]>, GsError> {
        let out = Command::new(&self.program)
            .args([
                "-dBATCH",
                "-dNOPAUSE",
                "-dQUIET",
                "-sDEVICE=inkcov",
                "-o",
                "-",
            ])
            .arg(input)
            .output()
            .map_err(|e| GsError::Failed(format!("spawn: {e}")))?;
        if !out.status.success() {
            return Err(GsError::Failed(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        parse_inkcov(&String::from_utf8_lossy(&out.stdout))
    }

    /// Convert a whole document (PDF/PS/XPS — Ghostscript sniffs the
    /// interpreter from the file) into a fresh PDF via `pdfwrite`.
    pub fn convert_to_pdf(
        &self,
        input: &std::path::Path,
        output: &std::path::Path,
    ) -> Result<(), GsError> {
        let status = Command::new(&self.program)
            .args([
                "-dBATCH",
                "-dNOPAUSE",
                "-dQUIET",
                "-sDEVICE=pdfwrite",
                &format!("-o{}", output.display()),
            ])
            .arg(input)
            .status()
            .map_err(|e| GsError::Failed(format!("spawn: {e}")))?;
        if !status.success() {
            return Err(GsError::Failed("pdfwrite conversion failed".into()));
        }
        Ok(())
    }

    /// Render pages to PNG (`pattern` must contain `%d`, e.g. `page-%d.png`).
    pub fn render_png_pages(
        &self,
        input: &std::path::Path,
        dpi: u32,
        pattern: &std::path::Path,
    ) -> Result<(), GsError> {
        let status = Command::new(&self.program)
            .args([
                "-dBATCH",
                "-dNOPAUSE",
                "-dQUIET",
                "-sDEVICE=png16m",
                &format!("-r{dpi}"),
                &format!("-o{}", pattern.display()),
            ])
            .arg(input)
            .status()
            .map_err(|e| GsError::Failed(format!("spawn: {e}")))?;
        if !status.success() {
            return Err(GsError::Failed("png16m render failed".into()));
        }
        Ok(())
    }
}

fn probe(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn search_program_files() -> Option<PathBuf> {
    for root in ["C:\\Program Files", "C:\\Program Files (x86)"] {
        let gs_dir = PathBuf::from(root).join("gs");
        let entries = std::fs::read_dir(gs_dir).ok()?;
        for entry in entries.flatten() {
            for exe in ["gswin64c.exe", "gswin32c.exe"] {
                let candidate = entry.path().join("bin").join(exe);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Parse `inkcov` stdout.
///
/// Real Ghostscript prints the coverage as its own device line — captured from
/// 10.08.0 with `-dQUIET -o -`, one line per page:
/// ` 0.08033  0.08033  0.08033  0.08033 CMYK OK`
///
/// The `Page N` banner is an interpreter *message*, and `-dQUIET` (which this
/// layer passes) suppresses it, so keying off "Page" — as this used to — found
/// nothing and every analysis died with "no Page lines found". The
/// banner-prefixed single-line shape is still accepted for older builds.
/// Values are 0..1 coverage fractions per channel.
pub fn parse_inkcov(stdout: &str) -> Result<Vec<[f32; 4]>, GsError> {
    let mut pages = Vec::new();
    for line in stdout.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let channels: &[&str] = if tokens.len() >= 6 && tokens[4] == "CMYK" && tokens[5] == "OK" {
            &tokens[0..4]
        } else if tokens.first() == Some(&"Page") && tokens.len() >= 6 {
            &tokens[2..6]
        } else {
            continue;
        };
        let parsed: Result<Vec<f32>, _> = channels.iter().map(|t| t.parse()).collect();
        let parsed = parsed.map_err(|_| GsError::Parse(format!("bad channels in: {line}")))?;
        pages.push([parsed[0], parsed[1], parsed[2], parsed[3]]);
    }
    if pages.is_empty() {
        return Err(GsError::Parse("no inkcov coverage lines found".into()));
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "GPL Ghostscript 10.02.1 (2023-11-01)\n\
        Page 1  0.00000  0.00000  0.00000  0.91234 CMYK OK\n\
        Page 2  0.45210  0.10100  0.00000  0.02000 CMYK OK\n";

    #[test]
    fn parses_inkcov_pages() {
        let pages = parse_inkcov(SAMPLE).unwrap();
        assert_eq!(pages.len(), 2);
        assert!((pages[0][3] - 0.91234).abs() < 1e-5);
        assert!((pages[1][0] - 0.45210).abs() < 1e-5);
    }

    /// Verbatim output of `gs -dBATCH -dNOPAUSE -dQUIET -sDEVICE=inkcov -o -
    /// file.pdf` on 10.08.0: no banner and no `Page N` (‑dQUIET kills it), one
    /// coverage line per page. This is the shape the analyzer actually sees.
    const QUIET_SAMPLE: &str =
        " 0.08033  0.08033  0.08033  0.08033 CMYK OK\n 0.00000  0.00000  0.00000  0.00000 CMYK OK\n";

    #[test]
    fn parses_quiet_inkcov_output() {
        let pages = parse_inkcov(QUIET_SAMPLE).unwrap();
        assert_eq!(pages.len(), 2);
        assert!((pages[0][0] - 0.08033).abs() < 1e-5);
        assert_eq!(pages[1], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn ignores_banner_lines_around_the_coverage() {
        let out = format!(
            "GPL Ghostscript 10.08.0 (2026-09-08)\nCopyright (C) 2026 Artifex Software, Inc.\n{}",
            QUIET_SAMPLE
        );
        assert_eq!(parse_inkcov(&out).unwrap().len(), 2);
    }

    #[test]
    fn rejects_empty_output() {
        assert!(parse_inkcov("nothing here\n").is_err());
    }
}
