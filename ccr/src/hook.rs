use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};

#[derive(Debug, Deserialize)]
struct HookInput {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: serde_json::Value,
    #[serde(default)]
    tool_response: ToolResponse,
}

#[derive(Debug, Deserialize, Default)]
struct ToolResponse {
    #[serde(default)]
    output: String,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct HookOutput {
    output: String,
}

pub fn run() -> Result<()> {
    let mut input = String::new();
    if io::stdin().read_to_string(&mut input).is_err() {
        return Ok(());
    }

    let hook_input: HookInput = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    match hook_input.tool_name.as_str() {
        "Read" => process_read(hook_input),
        "Glob" => process_glob(hook_input),
        _ => process_bash(hook_input), // Bash and unknown tools
    }
}

// ── Bash tool handler ─────────────────────────────────────────────────────────

fn process_bash(hook_input: HookInput) -> Result<()> {
    let full_cmd = hook_input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Skip commands that already went through cmd/run.rs or cmd/filter.rs —
    // those paths record their own analytics and the output is already compressed.
    if full_cmd.trim_start().starts_with("ccr ") {
        return Ok(());
    }

    // If command was rewritten by a wrapper (e.g. RTK: "rtk git status"),
    // attribute analytics to the real underlying command, not the wrapper.
    // Also normalize full paths to basename: "/usr/bin/git" → "git".
    let command_hint = {
        let mut tokens = full_cmd.split_whitespace();
        let first = tokens.next().unwrap_or("");
        let real = if first == "rtk" {
            tokens.next().unwrap_or("")
        } else {
            first
        };
        // Basename: strip path prefix so "/usr/bin/git" and "~/.cargo/bin/git" → "git"
        let basename = std::path::Path::new(real)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(real);
        if basename.is_empty() { None } else { Some(basename.to_string()) }
    };

    let output_text = if let Some(err) = &hook_input.tool_response.error {
        err.clone()
    } else if !hook_input.tool_response.output.is_empty() {
        hook_input.tool_response.output.clone()
    } else {
        hook_input.tool_response.stdout.clone()
    };

    if output_text.is_empty() {
        return Ok(());
    }

    let config = match crate::config_loader::load_config() {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    // IX: use Claude's last assistant message as the BERT query when available.
    // Falls back to the command string if no session file is found.
    let query = crate::intent::extract_intent().or_else(|| {
        hook_input
            .tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    let sid = crate::session::session_id();
    let mut session = crate::session::SessionState::load(&sid);

    // cmd_key for session tracking: skip wrapper prefix so "rtk git status" → "git status"
    let effective_cmd = if full_cmd.trim_start().starts_with("rtk ") {
        full_cmd.trim_start().trim_start_matches("rtk ").trim_start()
    } else {
        full_cmd.trim_start()
    };
    let cmd_key: String = effective_cmd
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");

    let historical_centroid = session.command_centroid(&cmd_key).cloned();

    let pressure = session.context_pressure();
    ccr_core::zoom::enable();

    // NL: apply pre-filter to remove lines promoted as permanent noise.
    let project_key = crate::util::project_key();
    let noise_store = project_key
        .as_ref()
        .map(|k| crate::noise_learner::NoiseStore::load(k));

    let raw_lines: Vec<&str> = output_text.lines().collect();
    let filtered_text: String = if let Some(ref store) = noise_store {
        let kept = store.apply_pre_filter(&raw_lines);
        if kept.len() < raw_lines.len() {
            kept.join("\n")
        } else {
            output_text.clone()
        }
    } else {
        output_text.clone()
    };

    let pipeline = ccr_core::pipeline::Pipeline::new(config.with_pressure(pressure));
    let result = match pipeline.process(
        &filtered_text,
        command_hint.as_deref(),
        query.as_deref(),
        historical_centroid.as_deref(),
    ) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };

    // NL: record what the pipeline suppressed so we can learn project noise.
    if let (Some(ref key), Some(mut store)) = (&project_key, noise_store) {
        let output_lines: Vec<&str> = result.output.lines().collect();
        store.record_lines(&raw_lines, &output_lines);
        store.promote_eligible();
        store.evict_stale(now_secs());
        store.save(key);
    }

    let _ = crate::zoom_store::save_blocks(&sid, result.zoom_blocks);

    // ── Session-aware passes ──────────────────────────────────────────────────
    // Skip BERT-based passes for short outputs: semantic compression and dedup
    // add latency without meaningful benefit when there are few lines to work with.
    const BERT_MIN_LINES: usize = 15;
    let pipeline_line_count = result.output.lines().count();

    let pipeline_emb = if pipeline_line_count >= BERT_MIN_LINES {
        ccr_core::summarizer::embed_batch(&[result.output.as_str()])
            .ok()
            .and_then(|mut v| v.pop())
    } else {
        None
    };

    let output_after_delta = if let Some(ref emb) = pipeline_emb {
        let lines: Vec<&str> = result.output.lines().collect();
        session
            .compute_delta(&cmd_key, &lines, emb)
            .map(|d| d.output)
            .unwrap_or_else(|| result.output.clone())
    } else {
        result.output.clone()
    };

    let after_dedup = apply_sentence_dedup(&output_after_delta, &cmd_key, &session);

    let compression_factor = session.compression_factor();
    let centroid_for_c2 = session.command_centroid(&cmd_key).cloned();
    let mut final_output = if compression_factor < 0.90 && pipeline_line_count >= BERT_MIN_LINES {
        let line_count = after_dedup.lines().count();
        let reduced_budget = ((line_count as f32 * compression_factor) as usize).max(10);
        if let Some(ref centroid) = centroid_for_c2 {
            ccr_core::summarizer::summarize_against_centroid(&after_dedup, reduced_budget, centroid)
                .output
        } else {
            ccr_core::summarizer::summarize(&after_dedup, reduced_budget).output
        }
    } else {
        after_dedup
    };

    if pipeline_line_count >= BERT_MIN_LINES {
        if let Ok(mut embeddings) = ccr_core::summarizer::embed_batch(&[final_output.as_str()]) {
            if let Some(emb) = embeddings.pop() {
                let tokens = ccr_core::tokens::count_tokens(&final_output);
                if let Ok(line_centroid) = ccr_core::summarizer::compute_output_centroid(&final_output) {
                    session.update_command_centroid(&cmd_key, line_centroid);
                } else {
                    session.update_command_centroid(&cmd_key, emb.clone());
                }
                let is_state = {
                    if let Ok(cfg) = crate::config_loader::load_config() {
                        cfg.global.state_commands.iter().any(|s| {
                            command_hint.as_deref() == Some(s.as_str())
                        })
                    } else {
                        false
                    }
                };
                session.record(&cmd_key, emb, tokens, &final_output, is_state);
                session.save(&sid);
            }
        }
    }

    if pressure > 0.80 {
        final_output.push_str(
            "\n[⚠ context near full — run `ccr compress --scan-session --dry-run` to estimate savings, or `ccr compress --scan-session` to compress]",
        );
    }

    // Record analytics: use pipeline output tokens (pre-BERT) for input — accurate
    // enough and avoids a BERT dependency for analytics correctness.
    let input_tokens = ccr_core::tokens::count_tokens(&output_text);
    let output_tokens = ccr_core::tokens::count_tokens(&final_output);
    let subcommand = effective_cmd
        .split_whitespace()
        .skip(1)
        .find(|s| !s.starts_with('-'))
        .map(|s| s.to_string());
    let analytics = ccr_core::analytics::Analytics::new(
        input_tokens,
        output_tokens,
        command_hint,
        subcommand,
        None,
    );
    crate::util::append_analytics(&analytics);

    let hook_output = HookOutput { output: final_output };
    println!("{}", serde_json::to_string(&hook_output)?);

    Ok(())
}

// ── Read tool handler ─────────────────────────────────────────────────────────

fn process_read(hook_input: HookInput) -> Result<()> {
    let file_path = hook_input
        .tool_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let output_text = if !hook_input.tool_response.output.is_empty() {
        hook_input.tool_response.output.clone()
    } else {
        hook_input.tool_response.stdout.clone()
    };

    if output_text.is_empty() {
        return Ok(());
    }

    // Binary file guard: if output contains null bytes, pass through unchanged
    if output_text.bytes().any(|b| b == 0) {
        return Ok(());
    }

    // Short files pass through without compression
    let line_count = output_text.lines().count();
    if line_count < 50 {
        return Ok(());
    }

    let config = match crate::config_loader::load_config() {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    // Use file extension as command hint, intent as query
    let ext_hint = std::path::Path::new(&file_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_string());

    let query = crate::intent::extract_intent().or_else(|| {
        std::path::Path::new(&file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    });

    let sid = crate::session::session_id();
    let mut session = crate::session::SessionState::load(&sid);
    let historical_centroid = session.command_centroid(&file_path).cloned();
    let pressure = session.context_pressure();

    ccr_core::zoom::enable();
    let pipeline = ccr_core::pipeline::Pipeline::new(config.with_pressure(pressure));
    let result = match pipeline.process(
        &output_text,
        ext_hint.as_deref(),
        query.as_deref(),
        historical_centroid.as_deref(),
    ) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };

    let _ = crate::zoom_store::save_blocks(&sid, result.zoom_blocks);

    // Session dedup using file_path as cmd_key
    let compressed = if let Ok(mut embs) =
        ccr_core::summarizer::embed_batch(&[result.output.as_str()])
    {
        if let Some(emb) = embs.pop() {
            let tokens = ccr_core::tokens::count_tokens(&result.output);
            if let Some(hit) = session.find_similar(&file_path, &emb) {
                let age = crate::session::format_age(hit.age_secs);
                format!(
                    "[same file content as turn {} ({} ago) — {} tokens saved]",
                    hit.turn, age, hit.tokens_saved
                )
            } else {
                session.record(&file_path, emb, tokens, &result.output, false);
                session.save(&sid);
                result.output
            }
        } else {
            result.output
        }
    } else {
        result.output
    };

    let input_tokens = ccr_core::tokens::count_tokens(&output_text);
    let output_tokens = ccr_core::tokens::count_tokens(&compressed);
    let analytics = ccr_core::analytics::Analytics::new(input_tokens, output_tokens, Some("(read)".to_string()), None, None);
    crate::util::append_analytics(&analytics);

    let hook_output = HookOutput { output: compressed };
    println!("{}", serde_json::to_string(&hook_output)?);

    Ok(())
}

// ── Glob tool handler ─────────────────────────────────────────────────────────

fn process_glob(hook_input: HookInput) -> Result<()> {
    let pattern = hook_input
        .tool_input
        .get("pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let output_text = if !hook_input.tool_response.output.is_empty() {
        hook_input.tool_response.output.clone()
    } else {
        hook_input.tool_response.stdout.clone()
    };

    if output_text.is_empty() {
        return Ok(());
    }

    let paths: Vec<&str> = output_text.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = paths.len();

    // Short results pass through
    if total <= 20 {
        return Ok(());
    }

    let sid = crate::session::session_id();
    let mut session = crate::session::SessionState::load(&sid);
    let cmd_key = format!("glob:{}", pattern);

    // Session dedup: hash the exact path list
    let list_hash = crate::util::hash_str(&output_text);
    if let Some(entry) = session.entries.iter().rev().find(|e| e.cmd == cmd_key) {
        if entry.content_preview.starts_with(&list_hash) {
            let hook_output = HookOutput {
                output: format!(
                    "[same glob result as turn {} — {} paths]",
                    entry.turn, total
                ),
            };
            println!("{}", serde_json::to_string(&hook_output)?);
            return Ok(());
        }
    }

    // Group paths by parent directory
    let mut by_dir: std::collections::BTreeMap<String, Vec<&str>> =
        std::collections::BTreeMap::new();
    for path in &paths {
        let parent = std::path::Path::new(path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(".")
            .to_string();
        by_dir.entry(parent).or_default().push(path);
    }

    let mut output_lines: Vec<String> = Vec::new();
    let mut shown = 0usize;
    const MAX_SHOWN: usize = 60;

    for (dir, files) in &by_dir {
        if shown >= MAX_SHOWN {
            break;
        }
        let remaining = MAX_SHOWN - shown;
        let show_count = files.len().min(remaining);
        for f in &files[..show_count] {
            output_lines.push(f.to_string());
        }
        if files.len() > show_count {
            output_lines.push(format!("  [+{} more in {}/]", files.len() - show_count, dir));
        }
        shown += show_count;
    }

    let hidden = total.saturating_sub(shown);
    if hidden > 0 {
        output_lines.push(format!("[+{} more paths not shown]", hidden));
    }
    output_lines.push(format!("[Glob: {} — {} paths total]", pattern, total));

    let compressed = output_lines.join("\n");

    // Record in session (use hash prefix as content_preview for dedup)
    let tokens = ccr_core::tokens::count_tokens(&compressed);
    let preview = format!("{} {}", list_hash, &compressed[..compressed.len().min(3900)]);
    if let Ok(mut embs) = ccr_core::summarizer::embed_batch(&[compressed.as_str()]) {
        if let Some(emb) = embs.pop() {
            session.entries.push(crate::session::SessionEntry {
                turn: session.total_turns + 1,
                cmd: cmd_key,
                ts: now_secs(),
                tokens,
                embedding: emb,
                content_preview: preview,
                state_content: None,
            });
            session.total_turns += 1;
            session.total_tokens += tokens;
            if session.entries.len() > 30 {
                session.entries.remove(0);
            }
            session.save(&sid);
        }
    }

    let input_tokens = ccr_core::tokens::count_tokens(&output_text);
    let output_tokens = ccr_core::tokens::count_tokens(&compressed);
    let analytics = ccr_core::analytics::Analytics::new(input_tokens, output_tokens, Some("(glob)".to_string()), None, None);
    crate::util::append_analytics(&analytics);

    let hook_output = HookOutput { output: compressed };
    println!("{}", serde_json::to_string(&hook_output)?);

    Ok(())
}

// ── Sentence dedup (C1) ───────────────────────────────────────────────────────

fn apply_sentence_dedup(
    output: &str,
    _cmd: &str,
    session: &crate::session::SessionState,
) -> String {
    use ccr_sdk::deduplicator::deduplicate;
    use ccr_sdk::message::Message;

    let prior = session.recent_content(8);
    if prior.is_empty() {
        return output.to_string();
    }

    let mut messages: Vec<Message> = prior
        .into_iter()
        .map(|(_, content)| Message { role: "user".to_string(), content })
        .collect();

    messages.push(Message {
        role: "user".to_string(),
        content: output.to_string(),
    });

    let deduped = deduplicate(messages);

    deduped
        .into_iter()
        .last()
        .map(|m| m.content)
        .unwrap_or_else(|| output.to_string())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
