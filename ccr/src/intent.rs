//! IX — Intent Extraction.
//!
//! Reads the most recent assistant message(s) from Claude Code's live JSONL session
//! file and returns them as a BERT query string. This replaces the shallow command
//! name (e.g. "cargo") with Claude's natural-language intent (e.g. "trace where
//! the memory leak occurs in the connection pool"), making BERT importance scoring
//! dramatically more relevant.
//!
//! `extract_intent_multi(n)` collects up to n recent assistant messages, weighted
//! by recency via char budgets [200, 150, 100], and joins them with " | ".
//! Repeated themes (e.g. "auth", "token refresh") naturally dominate the query
//! embedding without explicit weighting.
//!
//! Every failure returns `None` silently — the caller falls back to the command
//! string. Zero panics, zero stderr output.

use std::io::{Read, Seek, SeekFrom};

/// Extract up to `n` recent assistant messages from the current Claude Code session,
/// concatenated with " | " and weighted by recency (newest first).
///
/// Char budgets per message position: [200, 150, 100]. Positions beyond index 2
/// get 100 chars each. Returns `None` on any error or when no messages are found.
pub fn extract_intent_multi(n: usize) -> Option<String> {
    if n == 0 {
        return None;
    }
    let projects_dir = dirs::home_dir()?.join(".claude").join("projects");
    let project_dir = crate::util::project_dir_from_cwd()?;
    let session_dir = projects_dir.join(&project_dir);

    // Find the most recently modified .jsonl file in the project dir
    let jsonl_path = std::fs::read_dir(&session_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let mtime = meta.modified().ok()?;
            Some((mtime, e.path()))
        })
        .max_by_key(|(t, _)| *t)?
        .1;

    // Scan backwards in 256 KB chunks to find recent assistant messages.
    // Assistant messages can be preceded by very large tool-result lines (file reads,
    // long command output), so a fixed small tail would miss them in large sessions.
    let mut file = std::fs::File::open(&jsonl_path).ok()?;
    let file_len = file.metadata().ok()?.len();

    const CHUNK: u64 = 262_144; // 256 KB per pass
    const MAX_SCAN: u64 = 4 * CHUNK; // give up after 1 MB
    const BUDGETS: [usize; 3] = [200, 150, 100]; // char limits by recency

    let mut collected: Vec<String> = Vec::with_capacity(n);
    let mut scanned: u64 = 0;

    while scanned < MAX_SCAN && collected.len() < n {
        let window = CHUNK.min(file_len.saturating_sub(scanned));
        if window == 0 {
            break;
        }
        let offset = file_len.saturating_sub(scanned + window);
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut buf = vec![0u8; window as usize];
        file.read_exact(&mut buf).ok()?;

        // Walk lines from the END of this chunk so we get the most recent first
        let text = String::from_utf8_lossy(&buf);
        let mut lines: Vec<&str> = text.lines().collect();
        lines.reverse();

        for line in lines {
            if collected.len() >= n {
                break;
            }
            if !line.contains("\"type\":\"assistant\"") {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if obj.get("type").and_then(|t| t.as_str()) != Some("assistant") {
                continue;
            }
            let Some(content) = obj
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            else {
                continue;
            };
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        let trimmed = t.trim();
                        if !trimmed.is_empty() {
                            let budget = BUDGETS.get(collected.len()).copied().unwrap_or(100);
                            collected.push(clean_intent_with_limit(trimmed, budget));
                            break;
                        }
                    }
                }
            }
        }
        scanned += window;
    }

    if collected.is_empty() {
        return None;
    }
    Some(collected.join(" | "))
}

/// Extract the last assistant text from the current Claude Code session's JSONL file.
/// Returns `None` on any error (file not found, parse failure, empty content).
///
/// Thin wrapper around `extract_intent_multi(1)`.
pub fn extract_intent() -> Option<String> {
    extract_intent_multi(1)
}

/// Strip markdown and return the first sentence up to `char_limit` chars.
/// Truncates at the first sentence boundary (`.`, `?`, `!`).
/// If no boundary exists within `char_limit` chars, returns up to `char_limit` chars as-is.
fn clean_intent_with_limit(text: &str, char_limit: usize) -> String {
    let stripped: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '#' | '>'))
        .collect();
    let stripped = stripped.trim();

    let limit = stripped.len().min(char_limit);
    let chunk = &stripped[..limit];
    // Find the first sentence boundary and truncate there.
    if let Some(pos) = chunk.find(|c| matches!(c, '.' | '?' | '!')) {
        chunk[..=pos].trim().to_string()
    } else {
        chunk.trim().to_string()
    }
}

/// Strip markdown and return the first sentence up to 256 chars.
fn clean_intent(text: &str) -> String {
    clean_intent_with_limit(text, 256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_intent_returns_none_when_no_session() {
        // Should not panic with a nonexistent session
        // (project dir may or may not exist; either way, no panic)
        let _ = extract_intent();
    }

    #[test]
    fn extract_intent_multi_zero_returns_none() {
        assert!(extract_intent_multi(0).is_none());
    }

    #[test]
    fn clean_intent_strips_markdown() {
        let result = clean_intent("**Run** the `cargo build` command");
        assert!(!result.contains("**"), "got: {}", result);
        assert!(!result.contains('`'), "got: {}", result);
    }

    #[test]
    fn clean_intent_truncates_to_256() {
        let long: String = "x".repeat(500);
        let result = clean_intent(&long);
        assert!(result.len() <= 256);
    }

    #[test]
    fn clean_intent_truncates_at_sentence() {
        let result = clean_intent("First sentence. Second very long sentence that goes on.");
        assert_eq!(result, "First sentence.");
    }

    #[test]
    fn clean_intent_handles_question() {
        let result = clean_intent("Where is the bug? More text here.");
        assert_eq!(result, "Where is the bug?");
    }

    #[test]
    fn clean_intent_no_boundary_returns_chunk() {
        let result = clean_intent("no sentence boundary here at all no punctuation");
        assert!(!result.is_empty());
        assert!(result.len() <= 256);
    }

    #[test]
    fn clean_intent_with_limit_respects_limit() {
        let result = clean_intent_with_limit("hello world no punctuation", 10);
        assert!(result.len() <= 10);
    }

    #[test]
    fn clean_intent_with_limit_truncates_at_sentence() {
        let result = clean_intent_with_limit("Short. Much longer second sentence.", 200);
        assert_eq!(result, "Short.");
    }
}
