//! IX — Intent Extraction.
//!
//! Reads the most recent assistant message from Claude Code's live JSONL session
//! file and returns it as a BERT query string. This replaces the shallow command
//! name (e.g. "cargo") with Claude's natural-language intent (e.g. "trace where
//! the memory leak occurs in the connection pool"), making BERT importance scoring
//! dramatically more relevant.
//!
//! Every failure returns `None` silently — the caller falls back to the command
//! string. Zero panics, zero stderr output.

use std::io::{Read, Seek, SeekFrom};

/// Extract the last assistant text from the current Claude Code session's JSONL file.
/// Returns `None` on any error (file not found, parse failure, empty content).
pub fn extract_intent() -> Option<String> {
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

    // Read the last 16 KB — enough for the most recent 2-3 turns
    let mut file = std::fs::File::open(&jsonl_path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let tail_size: u64 = 16_384;
    let offset = file_len.saturating_sub(tail_size);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut tail = String::new();
    file.read_to_string(&mut tail).ok()?;

    // Parse lines, find the last assistant text block
    let mut last_text: Option<String> = None;
    for line in tail.lines() {
        if line.trim().is_empty() {
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
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        last_text = Some(trimmed.to_string());
                    }
                }
            }
        }
    }

    let text = last_text?;
    Some(clean_intent(&text))
}

/// Strip markdown, truncate to first sentence boundary within 256 chars.
fn clean_intent(text: &str) -> String {
    // Strip markdown characters
    let stripped: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '#' | '>'))
        .collect();
    let stripped = stripped.trim();

    // Work within 256 chars
    let limit = 256.min(stripped.len());
    let chunk = &stripped[..limit];

    // Truncate at first sentence boundary
    if let Some(pos) = chunk.find(|c| matches!(c, '.' | '?' | '!')) {
        chunk[..=pos].trim().to_string()
    } else {
        chunk.trim().to_string()
    }
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
}
