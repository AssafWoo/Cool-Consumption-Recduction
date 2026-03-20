//! NL — Cross-session Noise Learning.
//!
//! Learns which output lines are consistently suppressed by the pipeline across
//! many invocations of the same project. Lines suppressed ≥ 90% of the time
//! after ≥ 10 observations are "promoted" to permanent pre-filters, applied
//! before BERT ever sees them. Patterns evict after 30 days of inactivity.
//!
//! Storage: `~/.local/share/ccr/projects/<project_key>/noise.json`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Maximum number of patterns to store per project (explosion guard).
const MAX_PATTERNS: usize = 10_000;
/// Minimum observations before a pattern can be promoted.
const PROMOTE_MIN_COUNT: u32 = 10;
/// Minimum suppression rate (0.0–1.0) required for promotion.
const PROMOTE_MIN_RATE: f32 = 0.90;
/// Patterns not seen for this many seconds are evicted.
const EVICT_AGE_SECS: u64 = 30 * 86_400;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NoisePattern {
    /// Normalized line text used as the map key.
    pub pattern: String,
    /// Times this line was seen in pipeline input.
    pub count: u32,
    /// Unix timestamp of last observation.
    pub last_seen: u64,
    /// Times this line was removed by the pipeline.
    pub suppressed: u32,
    /// True when the pattern is actively pre-filtering input.
    pub promoted: bool,
}

#[derive(Serialize, Deserialize, Default)]
pub struct NoiseStore {
    pub patterns: HashMap<String, NoisePattern>,
}

// ── Persistence ────────────────────────────────────────────────────────────────

pub fn noise_path(project_key: &str) -> Option<PathBuf> {
    Some(
        dirs::data_local_dir()?
            .join("ccr")
            .join("projects")
            .join(project_key)
            .join("noise.json"),
    )
}

impl NoiseStore {
    pub fn load(project_key: &str) -> Self {
        noise_path(project_key)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, project_key: &str) {
        let Some(path) = noise_path(project_key) else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string(self) else { return };
        // Atomic write: write to .tmp then rename
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

// ── Learning ───────────────────────────────────────────────────────────────────

impl NoiseStore {
    /// Record observations: `input_lines` are what the pipeline received;
    /// `output_lines` are what it kept. Lines present in input but absent
    /// from output are counted as suppressed.
    pub fn record_lines(&mut self, input_lines: &[&str], output_lines: &[&str]) {
        let now = now_secs();
        let output_set: std::collections::HashSet<String> =
            output_lines.iter().map(|l| normalize_line(l)).collect();

        for raw in input_lines {
            let key = normalize_line(raw);
            if key.is_empty() {
                continue;
            }
            // Explosion guard
            if !self.patterns.contains_key(&key) && self.patterns.len() >= MAX_PATTERNS {
                continue;
            }
            let entry = self.patterns.entry(key.clone()).or_insert(NoisePattern {
                pattern: key.clone(),
                count: 0,
                last_seen: now,
                suppressed: 0,
                promoted: false,
            });
            entry.count += 1;
            entry.last_seen = now;
            if !output_set.contains(&key) {
                entry.suppressed += 1;
            }
        }
    }

    /// Promote patterns that meet the count and suppression-rate thresholds.
    pub fn promote_eligible(&mut self) {
        for entry in self.patterns.values_mut() {
            if !entry.promoted
                && entry.count >= PROMOTE_MIN_COUNT
                && suppression_rate(entry) >= PROMOTE_MIN_RATE
            {
                entry.promoted = true;
            }
        }
    }

    /// Remove patterns not seen in the last 30 days.
    pub fn evict_stale(&mut self, now: u64) {
        self.patterns.retain(|_, v| now.saturating_sub(v.last_seen) < EVICT_AGE_SECS);
    }
}

// ── Pre-filter ─────────────────────────────────────────────────────────────────

impl NoiseStore {
    /// Return only lines that are NOT promoted noise.
    /// Safety: lines matching the critical pattern are ALWAYS kept regardless.
    pub fn apply_pre_filter<'a>(&self, lines: &[&'a str]) -> Vec<&'a str> {
        lines
            .iter()
            .copied()
            .filter(|line| {
                // Always keep critical lines
                if is_critical(line) {
                    return true;
                }
                let key = normalize_line(line);
                // Keep unless promoted
                self.patterns
                    .get(&key)
                    .map(|p| !p.promoted)
                    .unwrap_or(true)
            })
            .collect()
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Normalize a line for use as a pattern key:
/// trim, lowercase, collapse progress-bar sequences.
pub fn normalize_line(line: &str) -> String {
    static PROGRESS_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = PROGRESS_RE.get_or_init(|| {
        // 4+ consecutive progress-bar characters (entire-line variants)
        regex::Regex::new(r"[=>\-<|\[\] ]{4,}").unwrap()
    });
    let trimmed = line.trim().to_lowercase();
    re.replace_all(&trimmed, "[progress]").to_string()
}

fn is_critical(line: &str) -> bool {
    static CRITICAL_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = CRITICAL_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)error|warning|failed|fatal|panic|exception|critical").unwrap()
    });
    re.is_match(line)
}

fn suppression_rate(p: &NoisePattern) -> f32 {
    if p.count == 0 {
        0.0
    } else {
        p.suppressed as f32 / p.count as f32
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store_with(pattern: &str, count: u32, suppressed: u32, promoted: bool) -> NoiseStore {
        let mut store = NoiseStore::default();
        store.patterns.insert(
            pattern.to_string(),
            NoisePattern {
                pattern: pattern.to_string(),
                count,
                last_seen: now_secs(),
                suppressed,
                promoted,
            },
        );
        store
    }

    #[test]
    fn record_lines_increments_suppressed() {
        let mut store = NoiseStore::default();
        store.record_lines(&["foo", "bar"], &["foo"]);
        let bar = store.patterns.get("bar").unwrap();
        assert_eq!(bar.suppressed, 1);
        let foo = store.patterns.get("foo").unwrap();
        assert_eq!(foo.suppressed, 0);
    }

    #[test]
    fn record_lines_increments_count() {
        let mut store = NoiseStore::default();
        store.record_lines(&["foo", "bar"], &["foo"]);
        assert_eq!(store.patterns.get("foo").unwrap().count, 1);
        assert_eq!(store.patterns.get("bar").unwrap().count, 1);
    }

    #[test]
    fn promote_eligible_promotes_after_threshold() {
        let mut store = make_store_with("downloading", 10, 10, false);
        store.promote_eligible();
        assert!(store.patterns.get("downloading").unwrap().promoted);
    }

    #[test]
    fn promote_does_not_promote_below_rate() {
        let mut store = make_store_with("downloading", 10, 8, false); // 80%
        store.promote_eligible();
        assert!(!store.patterns.get("downloading").unwrap().promoted);
    }

    #[test]
    fn promote_does_not_promote_below_count() {
        let mut store = make_store_with("downloading", 5, 5, false);
        store.promote_eligible();
        assert!(!store.patterns.get("downloading").unwrap().promoted);
    }

    #[test]
    fn pre_filter_removes_promoted_lines() {
        let store = make_store_with("downloading packages", 10, 10, true);
        let lines = vec!["downloading packages", "keep this"];
        let kept = store.apply_pre_filter(&lines);
        assert!(!kept.contains(&"downloading packages"));
        assert!(kept.contains(&"keep this"));
    }

    #[test]
    fn pre_filter_keeps_error_lines_even_if_promoted() {
        let mut store = make_store_with("error: something failed", 10, 10, false);
        store.patterns.get_mut("error: something failed").unwrap().promoted = true;
        let lines = vec!["error: something failed"];
        let kept = store.apply_pre_filter(&lines);
        assert!(kept.contains(&"error: something failed"), "critical lines must never be suppressed");
    }

    #[test]
    fn evict_stale_removes_old_entries() {
        let mut store = NoiseStore::default();
        let old_time = now_secs().saturating_sub(40 * 86_400);
        store.patterns.insert(
            "old".to_string(),
            NoisePattern { pattern: "old".to_string(), count: 5, last_seen: old_time, suppressed: 5, promoted: false },
        );
        store.evict_stale(now_secs());
        assert!(store.patterns.get("old").is_none());
    }

    #[test]
    fn evict_stale_keeps_recent_entries() {
        let mut store = NoiseStore::default();
        store.patterns.insert(
            "recent".to_string(),
            NoisePattern { pattern: "recent".to_string(), count: 5, last_seen: now_secs() - 5 * 86_400, suppressed: 5, promoted: false },
        );
        store.evict_stale(now_secs());
        assert!(store.patterns.get("recent").is_some());
    }

    #[test]
    fn normalize_collapses_progress_bar() {
        let normalized = normalize_line("Downloading [=====>  ] 80%");
        assert!(normalized.contains("[progress]"), "got: {}", normalized);
    }

    #[test]
    fn normalize_lowercases() {
        assert_eq!(normalize_line("Compiling Foo v1.0"), "compiling foo v1.0");
    }

    #[test]
    fn pattern_explosion_guard() {
        let mut store = NoiseStore::default();
        // Fill to limit
        for i in 0..MAX_PATTERNS {
            let key = format!("unique_line_{}", i);
            store.record_lines(&[Box::leak(key.into_boxed_str()) as &str], &[]);
        }
        assert_eq!(store.patterns.len(), MAX_PATTERNS);
        // One more should not grow the map
        store.record_lines(&["overflow_line"], &[]);
        assert_eq!(store.patterns.len(), MAX_PATTERNS);
    }
}
