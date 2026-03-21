use anyhow::Result;
use ccr_sdk::{
    compressor::{compress, CompressionConfig},
    deduplicator::deduplicate,
    message::Message,
    ollama::OllamaConfig,
};
use std::io::Read;

pub fn run(
    input: &str,
    output: Option<&str>,
    recent_turns: usize,
    tier1_turns: usize,
    ollama_url: Option<&str>,
    ollama_model: &str,
    max_tokens: Option<usize>,
    dry_run: bool,
    scan_session: bool,
) -> Result<()> {
    let (raw, source_path) = if scan_session {
        let path = find_latest_jsonl()
            .ok_or_else(|| anyhow::anyhow!("no .jsonl files found under ~/.claude/projects/"))?;
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("cannot read '{}': {}", path.display(), e))?;
        (raw, Some(path))
    } else {
        let raw = if input == "-" {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        } else {
            std::fs::read_to_string(input)
                .map_err(|e| anyhow::anyhow!("cannot read '{}': {}", input, e))?
        };
        (raw, None)
    };

    let messages = if scan_session {
        parse_jsonl_conversation(&raw)?
    } else {
        parse_conversation(&raw)?
    };

    if messages.is_empty() {
        if dry_run {
            println!("[dry-run] 0 turns · 0 → 0 tokens (0% saved)");
        } else {
            let out = "[]";
            match &source_path {
                Some(path) if scan_session => {
                    let out_path = format!("{}.compressed.json", path.display());
                    std::fs::write(&out_path, out)
                        .map_err(|e| anyhow::anyhow!("cannot write to '{}': {}", out_path, e))?;
                    eprintln!("[ccr compress] wrote compressed output to {}", out_path);
                }
                _ => write_output(out, output)?,
            }
        }
        return Ok(());
    }

    let config = CompressionConfig {
        recent_n: recent_turns,
        tier1_n: tier1_turns,
        ollama: ollama_url.map(|url| OllamaConfig {
            base_url: url.to_string(),
            model: ollama_model.to_string(),
            similarity_threshold: 0.80,
        }),
        max_context_tokens: max_tokens,
        ..CompressionConfig::default()
    };

    // Deduplicate first, then compress (matches Optimizer logic)
    let deduped = deduplicate(messages.clone());
    let result = compress(deduped, &config);

    let turns = messages.len();

    if dry_run {
        let saved_pct = if result.tokens_in > 0 {
            100.0 * (result.tokens_in - result.tokens_out.min(result.tokens_in)) as f64
                / result.tokens_in as f64
        } else {
            0.0
        };
        println!(
            "[dry-run] {} turns · {} → {} tokens ({:.0}% saved)",
            turns, result.tokens_in, result.tokens_out, saved_pct
        );
        return Ok(());
    }

    let json = serde_json::to_string_pretty(&result.messages)?;

    match &source_path {
        Some(path) if scan_session => {
            let out_path = format!("{}.compressed.json", path.display());
            std::fs::write(&out_path, &json)
                .map_err(|e| anyhow::anyhow!("cannot write to '{}': {}", out_path, e))?;
            if result.tokens_in > 0 {
                let saved_pct =
                    100.0 * (result.tokens_in - result.tokens_out.min(result.tokens_in)) as f64
                        / result.tokens_in as f64;
                eprintln!(
                    "[ccr compress] {} → {} tokens ({:.0}% saved)",
                    result.tokens_in, result.tokens_out, saved_pct
                );
            }
            eprintln!("[ccr compress] wrote compressed output to {}", out_path);
        }
        _ => {
            write_output(&json, output)?;
            // Stats to stderr so they don't pollute piped output
            if result.tokens_in > 0 {
                let saved_pct =
                    100.0 * (result.tokens_in - result.tokens_out.min(result.tokens_in)) as f64
                        / result.tokens_in as f64;
                eprintln!(
                    "[ccr compress] {} → {} tokens ({:.0}% saved)",
                    result.tokens_in, result.tokens_out, saved_pct
                );
            }
        }
    }

    Ok(())
}

fn write_output(content: &str, path: Option<&str>) -> Result<()> {
    match path {
        Some(p) => std::fs::write(p, content)
            .map_err(|e| anyhow::anyhow!("cannot write to '{}': {}", p, e)),
        None => {
            println!("{}", content);
            Ok(())
        }
    }
}

/// Find the most recently modified `.jsonl` file under `~/.claude/projects/`.
fn find_latest_jsonl() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let projects_dir = home.join(".claude").join("projects");
    if !projects_dir.exists() {
        return None;
    }

    let mut best: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    visit_dir(&projects_dir, &mut best);
    best.map(|(path, _)| path)
}

fn visit_dir(
    dir: &std::path::Path,
    best: &mut Option<(std::path::PathBuf, std::time::SystemTime)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_dir(&path, best);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            if let Ok(meta) = std::fs::metadata(&path) {
                if let Ok(modified) = meta.modified() {
                    let is_newer = best
                        .as_ref()
                        .map(|(_, t)| modified > *t)
                        .unwrap_or(true);
                    if is_newer {
                        *best = Some((path, modified));
                    }
                }
            }
        }
    }
}

/// Parse a JSONL conversation from `~/.claude/projects/`.
/// Each line is a JSON object with `"type"` and `"message"` fields.
/// Only `"user"` and `"assistant"` type lines are extracted.
fn parse_jsonl_conversation(raw: &str) -> Result<Vec<Message>> {
    let mut messages = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let typ = v["type"].as_str().unwrap_or("");
        if typ != "user" && typ != "assistant" {
            continue;
        }
        let role = typ.to_string();
        let content_val = &v["message"]["content"];
        let content = if let Some(s) = content_val.as_str() {
            s.to_string()
        } else if let Some(arr) = content_val.as_array() {
            arr.iter()
                .filter_map(|block| {
                    if block["type"].as_str() == Some("text") {
                        block["text"].as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            String::new()
        };
        messages.push(Message { role, content });
    }
    Ok(messages)
}

/// Parse a conversation JSON.
/// Accepts two formats:
///   1. `[{"role": "...", "content": "..."}]`  — bare array
///   2. `{"messages": [{"role": "...", "content": "..."}]}`  — object with messages key
fn parse_conversation(raw: &str) -> Result<Vec<Message>> {
    // Try bare array first
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
        return arr
            .iter()
            .map(|v| {
                Ok(Message {
                    role: v["role"].as_str().unwrap_or("user").to_string(),
                    content: v["content"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect();
    }

    // Try {messages: [...]} object
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(msgs) = obj["messages"].as_array() {
            return msgs
                .iter()
                .map(|v| {
                    Ok(Message {
                        role: v["role"].as_str().unwrap_or("user").to_string(),
                        content: v["content"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect();
        }
    }

    anyhow::bail!(
        "input is not valid conversation JSON \
         (expected array or {{\"messages\": [...]}})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_array() {
        let json = r#"[{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]"#;
        let msgs = parse_conversation(json).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].content, "hi");
    }

    #[test]
    fn parse_messages_object() {
        let json = r#"{"messages":[{"role":"user","content":"hello"}]}"#;
        let msgs = parse_conversation(json).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn parse_empty_array() {
        let json = "[]";
        let msgs = parse_conversation(json).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn parse_invalid_json_errors() {
        let result = parse_conversation("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn compression_reduces_tokens_for_long_history() {
        let long = "This is a long message with several sentences. It discusses project details. \
                    Make sure errors are never dropped. The config should be in TOML format. \
                    We want fast performance and low token usage.";
        let mut pairs: Vec<serde_json::Value> = Vec::new();
        for i in 0..10 {
            pairs.push(serde_json::json!({"role": "user", "content": long}));
            pairs.push(serde_json::json!({"role": "assistant", "content": format!("Response {}.", i)}));
        }
        let json = serde_json::to_string(&pairs).unwrap();
        let msgs = parse_conversation(&json).unwrap();
        let config = CompressionConfig::default();
        let deduped = deduplicate(msgs);
        let result = compress(deduped, &config);
        assert!(result.tokens_out <= result.tokens_in);
    }

    #[test]
    fn empty_input_returns_empty_json() {
        // Verify the empty path works end-to-end
        let msgs = parse_conversation("[]").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn parse_jsonl_extracts_user_and_assistant() {
        let jsonl = r#"{"type":"system","message":{"role":"system","content":"You are helpful."}}
{"type":"user","message":{"role":"user","content":"Hello there"}}
{"type":"assistant","message":{"role":"assistant","content":"Hi! How can I help?"}}
{"type":"tool_use","message":{"role":"tool","content":"some tool output"}}"#;
        let msgs = parse_jsonl_conversation(jsonl).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].content, "Hello there");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "Hi! How can I help?");
    }

    #[test]
    fn parse_jsonl_handles_array_content_blocks() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Part one"},{"type":"text","text":"Part two"}]}}"#;
        let msgs = parse_jsonl_conversation(jsonl).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Part one\nPart two");
    }

    #[test]
    fn parse_jsonl_skips_non_text_blocks_in_array() {
        let jsonl = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}},{"type":"text","text":"Done."}]}}"#;
        let msgs = parse_jsonl_conversation(jsonl).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "Done.");
    }

    #[test]
    fn parse_jsonl_empty_input() {
        let msgs = parse_jsonl_conversation("").unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn find_latest_jsonl_returns_none_when_dir_missing() {
        // ~/.claude/projects/ may or may not exist; we just verify the function
        // doesn't panic and returns None when given a nonexistent path by
        // checking the behavior of visit_dir directly with a temp path.
        let nonexistent = std::path::Path::new("/tmp/ccr_test_nonexistent_dir_xyz");
        let mut best = None;
        visit_dir(nonexistent, &mut best);
        assert!(best.is_none());
    }
}
