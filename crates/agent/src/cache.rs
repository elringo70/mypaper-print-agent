//! Local analysis cache, keyed by the document's content.
//!
//! Measuring a page costs a render; the same file measured twice is wasted
//! work. The key is the SHA-256 of the spool payload plus the engine identity
//! (version and dpi), so an engine change never serves a stale number: a cache
//! hit is always the answer the current engine would have produced.
//!
//! Storage is a directory of JSON files, one per document, written atomically
//! (tmp + rename) so a crash mid-write leaves no half file. Eviction is LRU by
//! modification time up to a size cap — no database, nothing to install.

use base64::Engine as _;
use mpa_analyzer::{JobAnalysis, PageAnalysis};
use mpa_domain::Coverage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Default ceiling for the whole cache.
pub const DEFAULT_MAX_BYTES: u64 = 500 * 1024 * 1024;

const CACHE_FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPage {
    page_no: u32,
    coverage: Coverage,
    #[serde(default)]
    metrics: Option<mpa_analyzer::PageMetrics>,
    /// PNG, base64. Empty when the preview budget skipped the page.
    #[serde(default)]
    thumb: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAnalysis {
    format_version: u32,
    format: mpa_analyzer::SpoolFormat,
    pages: Vec<StoredPage>,
}

/// Where the cache lives and how big it may get.
#[derive(Debug, Clone)]
pub struct AnalysisCache {
    dir: PathBuf,
    max_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: u64,
}

impl AnalysisCache {
    pub fn new(dir: PathBuf, max_bytes: u64) -> Self {
        Self { dir, max_bytes }
    }

    /// Next to `config.toml`, like the rest of the agent's state.
    pub fn open_default() -> Self {
        let dir = default_cache_dir();
        Self::new(dir, DEFAULT_MAX_BYTES)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Content identity of one analysis: payload hash + engine + resolution.
    /// Thresholds travel with the engine version (changing them bumps it), so
    /// two different rules can never share a key.
    pub fn key_for(payload: &[u8], dpi: u32) -> String {
        let mut hasher = Sha256::new();
        hasher.update(payload);
        format!(
            "{}-e{}-d{}",
            hex::encode(hasher.finalize()),
            mpa_analyzer::ENGINE_VERSION,
            dpi
        )
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    /// Cached analysis for this key, or `None` on miss or any unreadable file.
    /// A corrupt entry is treated as absent: measure again, never fail.
    pub fn get(&self, key: &str) -> Option<JobAnalysis> {
        let raw = std::fs::read(self.path_for(key)).ok()?;
        let stored: StoredAnalysis = serde_json::from_slice(&raw).ok()?;
        if stored.format_version != CACHE_FORMAT {
            return None;
        }
        let pages = stored
            .pages
            .into_iter()
            .map(|page| PageAnalysis {
                page_no: page.page_no,
                coverage: page.coverage,
                thumbnail_png: base64::engine::general_purpose::STANDARD
                    .decode(page.thumb)
                    .unwrap_or_default(),
                metrics: page.metrics,
            })
            .collect();
        Some(JobAnalysis {
            pages,
            format: stored.format,
        })
    }

    /// Store one analysis. Best effort: a cache that cannot write must never
    /// fail the job the operator is waiting for.
    pub fn put(&self, key: &str, analysis: &JobAnalysis) {
        let stored = StoredAnalysis {
            format_version: CACHE_FORMAT,
            format: analysis.format,
            pages: analysis
                .pages
                .iter()
                .map(|page| StoredPage {
                    page_no: page.page_no,
                    coverage: page.coverage,
                    metrics: page.metrics,
                    thumb: base64::engine::general_purpose::STANDARD.encode(&page.thumbnail_png),
                })
                .collect(),
        };
        let Ok(bytes) = serde_json::to_vec(&stored) else {
            return;
        };
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let path = self.path_for(key);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn stats(&self) -> CacheStats {
        let mut entries = 0;
        let mut bytes = 0;
        if let Ok(list) = std::fs::read_dir(&self.dir) {
            for entry in list.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                    entries += 1;
                    bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        CacheStats { entries, bytes }
    }

    /// Delete least-recently-used entries until the cache fits under the cap.
    /// Returns how many were removed.
    pub fn gc(&self) -> usize {
        let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
        if let Ok(list) = std::fs::read_dir(&self.dir) {
            for entry in list.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Ok(meta) = entry.metadata() else { continue };
                let at = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                files.push((path, meta.len(), at));
            }
        }
        let total: u64 = files.iter().map(|(_, len, _)| len).sum();
        if total <= self.max_bytes {
            return 0;
        }
        // Oldest first: the cache is an optimisation, so the coldest entry is
        // the one worth losing.
        files.sort_by_key(|(_, _, at)| *at);
        let mut over = total - self.max_bytes;
        let mut removed = 0;
        for (path, len, _) in files {
            if over == 0 {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                over = over.saturating_sub(len);
                removed += 1;
            }
        }
        removed
    }

    /// Drop everything. Returns how many files were removed.
    pub fn clear(&self) -> usize {
        let mut removed = 0;
        if let Ok(list) = std::fs::read_dir(&self.dir) {
            for entry in list.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json")
                    && std::fs::remove_file(&path).is_ok()
                {
                    removed += 1;
                }
            }
        }
        removed
    }
}

/// `analysis-cache/` beside `config.toml`.
pub fn default_cache_dir() -> PathBuf {
    let base = crate::config_path(None)
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(std::env::temp_dir);
    base.join("analysis-cache")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mpa_analyzer::PageClass;

    fn workdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mpa-cache-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn analysis() -> JobAnalysis {
        JobAnalysis {
            format: mpa_analyzer::SpoolFormat::Pdf,
            pages: vec![PageAnalysis {
                page_no: 1,
                coverage: Coverage::new(10.0, 0.0, 0.0, 50.0),
                thumbnail_png: vec![0x89, b'P', b'N', b'G'],
                metrics: Some(analyzed_page().metrics.unwrap()),
            }],
        }
    }

    fn analyzed_page() -> PageAnalysis {
        let metrics = mpa_analyzer::analyze_rgba(&[0, 0, 0, 255], 1, 1).unwrap();
        PageAnalysis {
            page_no: 1,
            coverage: metrics.coverage(),
            thumbnail_png: Vec::new(),
            metrics: Some(metrics),
        }
    }

    #[test]
    fn a_stored_analysis_comes_back_whole() {
        let cache = AnalysisCache::new(workdir("roundtrip"), DEFAULT_MAX_BYTES);
        let key = AnalysisCache::key_for(b"%PDF-1.4 hello", 150);
        cache.put(&key, &analysis());

        let hit = cache.get(&key).expect("cache hit");
        assert_eq!(hit.format, mpa_analyzer::SpoolFormat::Pdf);
        assert_eq!(hit.pages.len(), 1);
        assert_eq!(hit.pages[0].coverage.k, 50.0);
        assert_eq!(hit.pages[0].thumbnail_png, vec![0x89, b'P', b'N', b'G']);
        let metrics = hit.pages[0].metrics.expect("metrics survive the cache");
        assert_eq!(metrics.class, PageClass::Mono);
        let _ = std::fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn the_key_covers_content_and_resolution() {
        let a = AnalysisCache::key_for(b"one", 150);
        let b = AnalysisCache::key_for(b"two", 150);
        let c = AnalysisCache::key_for(b"one", 120);
        assert_ne!(a, b, "different documents must not share an entry");
        assert_ne!(a, c, "a different dpi is a different measurement");
        assert!(a.contains(mpa_analyzer::ENGINE_VERSION));
    }

    #[test]
    fn a_corrupt_entry_is_a_miss_not_a_failure() {
        let cache = AnalysisCache::new(workdir("corrupt"), DEFAULT_MAX_BYTES);
        let key = AnalysisCache::key_for(b"payload", 150);
        std::fs::create_dir_all(cache.dir()).unwrap();
        std::fs::write(cache.dir().join(format!("{key}.json")), b"{ not json").unwrap();
        assert!(cache.get(&key).is_none());
        let _ = std::fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn gc_evicts_the_oldest_until_under_the_cap() {
        let dir = workdir("gc");
        // 1 kB cap with three entries: the oldest go, the newest stays.
        let cache = AnalysisCache::new(dir.clone(), 1024);
        for key in ["oldest", "middle", "newest"] {
            cache.put(key, &analysis());
            // Distinct mtimes, or "oldest" is whatever the directory lists first.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(cache.stats().entries, 3);

        let removed = cache.gc();
        assert!(removed > 0, "over the cap, gc must remove something");
        assert!(cache.stats().bytes <= cache.max_bytes);
        assert!(
            dir.join("newest.json").exists(),
            "the most recently used entry must survive"
        );
        assert!(!dir.join("oldest.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_empties_the_directory() {
        let cache = AnalysisCache::new(workdir("clear"), DEFAULT_MAX_BYTES);
        cache.put("a", &analysis());
        cache.put("b", &analysis());
        assert_eq!(cache.stats().entries, 2);
        assert_eq!(cache.clear(), 2);
        assert_eq!(cache.stats().entries, 0);
        let _ = std::fs::remove_dir_all(cache.dir());
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let cache = AnalysisCache::new(workdir("missing"), DEFAULT_MAX_BYTES);
        assert!(cache.get("nope").is_none());
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.gc(), 0);
        assert_eq!(cache.clear(), 0);
    }
}
