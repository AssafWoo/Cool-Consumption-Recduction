//! PC — Pre-run Cache.
//!
//! Computes a structural cache key BEFORE executing a command. If the key
//! matches a recent cached result, the command is skipped entirely and the
//! cached output is returned — saving both execution time and output tokens.
//!
//! Currently supports: git (status, diff, log, branch, stash).
//!
//! Cache entries expire after 1 hour. Storage is per-session.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// TTL for cache entries in seconds.
const CACHE_TTL_SECS: u64 = 3_600;

#[derive(Clone)]
pub struct PreCacheKey {
    /// 16-char hex key; stable when the relevant state has not changed.
    pub key: String,
    /// The command string used as the HashMap key (e.g. "git status").
    pub cmd: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PreCacheEntry {
    pub key: String,
    pub output: String,
    pub ts: u64,
    /// Token count of `output` at time of caching.
    pub tokens: usize,
}

#[derive(Serialize, Deserialize, Default)]
pub struct PreCache {
    entries: HashMap<String, PreCacheEntry>,
}

// ── Persistence ────────────────────────────────────────────────────────────────

fn cache_path(session_id: &str) -> Option<PathBuf> {
    Some(
        dirs::data_local_dir()?
            .join("ccr")
            .join("pre_cache")
            .join(format!("{}.json", session_id)),
    )
}

impl PreCache {
    pub fn load(session_id: &str) -> Self {
        cache_path(session_id)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, session_id: &str) {
        let Some(path) = cache_path(session_id) else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string(self) else { return };
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

// ── Key computation ────────────────────────────────────────────────────────────

impl PreCache {
    /// Compute a structural cache key for `args` before executing the command.
    /// Returns `None` if the command is not cacheable or state cannot be determined.
    pub fn compute_key(args: &[String]) -> Option<PreCacheKey> {
        match args.first().map(|s| s.as_str()) {
            Some("git") => git_cache_key(args),
            _ => None,
        }
    }
}

fn git_cache_key(args: &[String]) -> Option<PreCacheKey> {
    let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
    // Only cache read-only state subcommands
    match subcmd {
        "status" | "diff" | "log" | "branch" | "stash" => {}
        _ => return None,
    }
    let cmd_str = args.iter().take(2).cloned().collect::<Vec<_>>().join(" ");

    // HEAD hash — fails in fresh repo or non-git dir
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !head.status.success() {
        return None;
    }
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Staged changes summary
    let staged = std::process::Command::new("git")
        .args(["diff-index", "--cached", "--stat", "HEAD"])
        .output()
        .ok()?;

    // Unstaged changes summary
    let unstaged = std::process::Command::new("git")
        .args(["diff-files", "--stat"])
        .output()
        .ok()?;

    let combined = format!(
        "{}{}{}",
        head_hash,
        String::from_utf8_lossy(&staged.stdout),
        String::from_utf8_lossy(&unstaged.stdout)
    );
    let key = crate::util::hash_str(&combined);

    Some(PreCacheKey { key, cmd: cmd_str })
}

// ── Lookup / insert / evict ───────────────────────────────────────────────────

impl PreCache {
    /// Look up a cache entry. Returns `Some` only when the key hash matches exactly.
    pub fn lookup(&self, key: &PreCacheKey) -> Option<&PreCacheEntry> {
        let entry = self.entries.get(&key.cmd)?;
        if entry.key == key.key {
            Some(entry)
        } else {
            None
        }
    }

    /// Store or update a cache entry.
    pub fn insert(&mut self, key: PreCacheKey, output: &str, tokens: usize) {
        self.entries.insert(
            key.cmd.clone(),
            PreCacheEntry {
                key: key.key,
                output: output.to_string(),
                ts: now_secs(),
                tokens,
            },
        );
    }

    /// Remove entries older than the TTL.
    pub fn evict_old(&mut self) {
        let cutoff = now_secs().saturating_sub(CACHE_TTL_SECS);
        self.entries.retain(|_, v| v.ts >= cutoff);
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

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

    fn make_key(key: &str, cmd: &str) -> PreCacheKey {
        PreCacheKey { key: key.to_string(), cmd: cmd.to_string() }
    }

    #[test]
    fn compute_key_returns_none_for_unknown_command() {
        let result = PreCache::compute_key(&["python3".into(), "main.py".into()]);
        assert!(result.is_none());
    }

    #[test]
    fn compute_key_returns_none_for_git_push() {
        let result = PreCache::compute_key(&["git".into(), "push".into()]);
        assert!(result.is_none());
    }

    #[test]
    fn compute_key_returns_none_for_git_commit() {
        let result = PreCache::compute_key(&["git".into(), "commit".into()]);
        assert!(result.is_none());
    }

    #[test]
    fn lookup_returns_none_on_key_mismatch() {
        let mut cache = PreCache::default();
        cache.insert(make_key("aabbccdd11223344", "git status"), "output", 100);
        let result = cache.lookup(&make_key("eeff001122334455", "git status"));
        assert!(result.is_none());
    }

    #[test]
    fn lookup_returns_entry_on_key_match() {
        let mut cache = PreCache::default();
        cache.insert(make_key("aabbccdd11223344", "git status"), "some output", 42);
        let result = cache.lookup(&make_key("aabbccdd11223344", "git status"));
        assert!(result.is_some());
        assert_eq!(result.unwrap().output, "some output");
    }

    #[test]
    fn evict_old_removes_stale_entries() {
        let mut cache = PreCache::default();
        cache.entries.insert(
            "git status".to_string(),
            PreCacheEntry {
                key: "abc".to_string(),
                output: "old".to_string(),
                ts: now_secs().saturating_sub(CACHE_TTL_SECS + 100),
                tokens: 10,
            },
        );
        cache.evict_old();
        assert!(cache.lookup(&make_key("abc", "git status")).is_none());
    }

    #[test]
    fn evict_old_keeps_fresh_entries() {
        let mut cache = PreCache::default();
        cache.insert(make_key("abc123", "git status"), "fresh", 10);
        cache.evict_old();
        assert!(cache.lookup(&make_key("abc123", "git status")).is_some());
    }

    #[test]
    fn insert_overwrites_existing_cmd() {
        let mut cache = PreCache::default();
        cache.insert(make_key("key1", "git status"), "old output", 10);
        cache.insert(make_key("key2", "git status"), "new output", 20);
        // key2 is the latest
        assert!(cache.lookup(&make_key("key2", "git status")).is_some());
        assert_eq!(cache.lookup(&make_key("key2", "git status")).unwrap().output, "new output");
    }
}
