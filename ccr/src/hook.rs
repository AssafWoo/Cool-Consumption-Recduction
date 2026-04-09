use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use dirs;

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

/// Core processing: takes raw hook stdin JSON, returns the JSON string to print
/// (already serialised HookOutput), or `None` for pass-through.
///
/// Does NOT attempt the daemon socket — call this directly from the daemon
/// server and from the fallback path in `run()`.
pub fn process(input: &str) -> Result<Option<String>> {
    let hook_input: HookInput = match serde_json::from_str(input) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    match hook_input.tool_name.as_str() {
        "Read" => process_read(hook_input),
        "Glob" => process_glob(hook_input),
        "Grep" => process_grep(hook_input),
        _ => process_bash(hook_input), // Bash and unknown tools
    }
}

pub fn run() -> Result<()> {
    // Integrity check: warn and exit if hook script has been tampered with.
    // CCR_AGENT env var is set by Cursor's PostToolUse hook command; default = claude.
    let agent = std::env::var("CCR_AGENT").unwrap_or_else(|_| "claude".to_string());
    if let Some(home) = dirs::home_dir() {
        let (script, hashdir) = match agent.as_str() {
            "cursor" => (
                home.join(".cursor").join("hooks").join("ccr-rewrite.sh"),
                home.join(".cursor").join("hooks"),
            ),
            _ => (
                home.join(".claude").join("hooks").join("ccr-rewrite.sh"),
                home.join(".claude").join("hooks"),
            ),
        };
        crate::integrity::runtime_check(&script, &hashdir);
    }

    let mut raw = String::new();
    if io::stdin().read_to_string(&mut raw).is_err() {
        return Ok(());
    }

    if let Ok(Some(output)) = process(&raw) {
        print!("{}", output);
    }
    Ok(())
}

// ── Bash tool handler ─────────────────────────────────────────────────────────

fn process_bash(hook_input: HookInput) -> Result<Option<String>> {
    let full_cmd = hook_input
        .tool_input
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Skip commands that already went through cmd/run.rs or cmd/filter.rs —
    // those paths record their own analytics and the output is already compressed.
    if full_cmd.trim_start().starts_with("ccr ") {
        return Ok(None);
    }

    // If command was rewritten by a wrapper (e.g. RTK: "rtk git status"),
    // attribute analytics to the real underlying command, not the wrapper.
    // Also normalize full paths to basename: "/usr/bin/git" → "git".
    // Skip leading KEY=VALUE env var assignments (e.g. "GIT_COMMITTER_NAME=Assaf git commit").
    let command_hint = {
        let mut tokens = full_cmd.split_whitespace()
            .skip_while(|t| {
                let eq = t.find('=').unwrap_or(0);
                eq > 0 && t[..eq].chars().all(|c| c.is_ascii_uppercase() || c == '_')
            });
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
        return Ok(None);
    }

    // Skip the entire pipeline (including BERT) for trivially small outputs.
    // Commands like `which`, `mkdir`, `wc` produce <15 tokens — nothing to compress.
    const MIN_PIPELINE_TOKENS: usize = 15;
    if ccr_core::tokens::count_tokens(&output_text) < MIN_PIPELINE_TOKENS {
        return Ok(None);
    }

    // Fix 3: skip shell infrastructure commands that never produce meaningful
    // output of their own. Any output attributed to these is leaked from a
    // compound command (e.g. `sleep 30 && curl ...`) and should pass through
    // unmodified rather than being logged as a low-savings run.
    // Guard: only skip when there are no error signals in the output.
    if command_hint.as_deref().map(is_no_output_cmd).unwrap_or(false)
        && !output_text.contains("error")
        && !output_text.contains("Error")
        && !output_text.contains("invalid")
    {
        return Ok(None);
    }

    let config = match crate::config_loader::load_config() {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    // IX: use the last 3 assistant messages as the BERT query when available.
    // Multi-turn accumulation weights by recency [200, 150, 100 chars] so that
    // repeated themes (e.g. "auth", "token refresh") dominate the query embedding.
    // Falls back to the command string if no session file is found.
    let query = crate::intent::extract_intent_multi(3).or_else(|| {
        hook_input
            .tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    });

    let sid = crate::session::session_id();
    let mut session = crate::session::SessionState::load(&sid);

    // RC: result cache — return byte-identical output on hit (prompt cache stability)
    let rc_key = crate::result_cache::ResultCache::compute_key(&output_text, command_hint.as_deref());
    {
        let mut rc = crate::result_cache::ResultCache::load(&sid);
        rc.evict_old();
        if let Some(entry) = rc.lookup(&rc_key) {
            let cached_output = entry.output.clone();
            let analytics = ccr_core::analytics::Analytics::new_cache_hit(
                entry.input_tokens,
                entry.output_tokens,
                command_hint.clone(),
                None,
            );
            crate::util::append_analytics(&analytics);
            let hook_output = HookOutput { output: cached_output };
            return Ok(Some(serde_json::to_string(&hook_output)?));
        }
    }

    // cmd_key for session tracking: skip leading KEY=VALUE env vars and wrapper prefix.
    // "GIT_COMMITTER_NAME=Assaf git commit -m foo" → "git commit"
    // "rtk git status" → "git status"
    let cmd_key: String = {
        fn is_env_assign(t: &&str) -> bool {
            let eq = t.find('=').unwrap_or(0);
            eq > 0 && t[..eq].chars().all(|c| c.is_ascii_uppercase() || c == '_')
        }
        let mut real_tokens = full_cmd.split_whitespace().skip_while(is_env_assign);
        let first = real_tokens.next().unwrap_or("");
        let rest: Vec<&str> = real_tokens.collect();
        let real_iter: Box<dyn Iterator<Item = &str>> = if first == "rtk" {
            Box::new(rest.into_iter())
        } else {
            Box::new(std::iter::once(first).chain(rest.into_iter()))
        };
        real_iter.take(2).collect::<Vec<_>>().join(" ")
    };

    // Fix 1+2: Polling suppressor with timestamp-normalized embeddings.
    // If the same command ran within the last 120 seconds, embed a
    // timestamp-stripped version of the output and check similarity at a
    // lower threshold (0.80 vs 0.92).  Catches polling loops where only
    // clocks/counters/UUIDs differ between otherwise identical responses.
    if session.has_recent_entry(&cmd_key, 120) {
        let normalized = strip_temporal_noise(&output_text);
        if let Ok(mut embs) = ccr_core::summarizer::embed_batch(&[normalized.as_str()]) {
            if let Some(emb) = embs.pop() {
                if let Some(hit) = session.find_similar_recent(&cmd_key, &emb) {
                    let age = crate::session::format_age(hit.age_secs);
                    let marker = format!(
                        "[same output as turn {} ({} ago) — {} tokens saved]",
                        hit.turn, age, hit.tokens_saved
                    );
                    let in_tok = ccr_core::tokens::count_tokens(&output_text);
                    let out_tok = ccr_core::tokens::count_tokens(&marker);
                    crate::util::append_analytics(&ccr_core::analytics::Analytics::new(
                        in_tok, out_tok, command_hint.clone(), None, None,
                    ));
                    return Ok(Some(serde_json::to_string(&HookOutput { output: marker })?));
                }
            }
        }
    }

    let historical_centroid = session.command_centroid(&cmd_key).cloned();

    // Adaptive pressure: if the same command has been producing near-identical output
    // (high centroid_delta similarity), it's stale context — compress it more.
    // Similarity < 0.85 → no modifier (content is novel).
    // Similarity 0.85→1.0 → linearly maps to modifier 0.0→0.32 (capped).
    let stability_pressure = session
        .last_centroid_delta(&cmd_key)
        .map(stability_to_pressure)
        .unwrap_or(0.0);
    let pressure = (session.context_pressure() + stability_pressure).min(1.0);
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
    let mut result = match pipeline.process(
        &filtered_text,
        command_hint.as_deref(),
        query.as_deref(),
        historical_centroid.as_deref(),
    ) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    // NL: record what the pipeline suppressed so we can learn project noise.
    if let (Some(ref key), Some(mut store)) = (&project_key, noise_store) {
        let output_lines: Vec<&str> = result.output.lines().collect();
        store.record_lines(&raw_lines, &output_lines);
        store.promote_eligible();
        store.evict_stale(now_secs());
        store.save(key);
    }

    // Enrich zoom block labels with content-derived categories before persisting.
    result.output = enrich_zoom_labels(&result.output, &result.zoom_blocks);
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

    // Determine is_state early so cross-command dedup can respect the guard.
    let is_state = {
        if let Ok(cfg) = crate::config_loader::load_config() {
            cfg.global.state_commands.iter().any(|s| {
                command_hint.as_deref() == Some(s.as_str())
            })
        } else {
            false
        }
    };

    let after_xdedup = cross_command_dedup(&after_dedup, &cmd_key, &session, &sid, is_state);

    // Save any new zoom blocks registered during cross-command dedup.
    let xdedup_blocks = ccr_core::zoom::drain();
    if !xdedup_blocks.is_empty() {
        let _ = crate::zoom_store::save_blocks(&sid, xdedup_blocks);
    }

    let compression_factor = session.compression_factor();
    let centroid_for_c2 = session.command_centroid(&cmd_key).cloned();
    let mut final_output = if compression_factor < 0.90 && pipeline_line_count >= BERT_MIN_LINES {
        let line_count = after_xdedup.lines().count();
        let reduced_budget = ((line_count as f32 * compression_factor) as usize).max(10);
        if let Some(ref centroid) = centroid_for_c2 {
            ccr_core::summarizer::summarize_against_centroid(&after_xdedup, reduced_budget, centroid)
                .output
        } else {
            ccr_core::summarizer::summarize(&after_xdedup, reduced_budget).output
        }
    } else {
        after_xdedup
    };

    if pipeline_line_count >= BERT_MIN_LINES {
        if let Ok(mut embeddings) = ccr_core::summarizer::embed_batch(&[final_output.as_str()]) {
            if let Some(emb) = embeddings.pop() {
                let tokens = ccr_core::tokens::count_tokens(&final_output);
                // Compute centroid_delta: cosine similarity of new centroid vs historical.
                // Stored on the entry so the NEXT invocation can derive stability pressure.
                let new_centroid = ccr_core::summarizer::compute_output_centroid(&final_output)
                    .ok()
                    .unwrap_or_else(|| emb.clone());
                let centroid_delta = historical_centroid
                    .as_ref()
                    .map(|hist| crate::handlers::util::cosine_similarity(hist, &new_centroid));
                session.update_command_centroid(&cmd_key, new_centroid);
                session.record(&cmd_key, emb, tokens, &final_output, is_state, centroid_delta);
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
    // subcommand is the second non-flag token of the real command (already in cmd_key)
    let subcommand = cmd_key
        .split_whitespace()
        .nth(1)
        .filter(|s| !s.starts_with('-'))
        .map(|s| s.to_string());
    let analytics = ccr_core::analytics::Analytics::new(
        input_tokens,
        output_tokens,
        command_hint,
        subcommand,
        None,
    );
    crate::util::append_analytics(&analytics);

    {
        let mut rc = crate::result_cache::ResultCache::load(&sid);
        rc.insert(rc_key, final_output.clone(), input_tokens, output_tokens);
        rc.save(&sid);
    }

    let hook_output = HookOutput { output: final_output };
    Ok(Some(serde_json::to_string(&hook_output)?))
}

// ── Read tool handler ─────────────────────────────────────────────────────────

fn process_read(hook_input: HookInput) -> Result<Option<String>> {
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
        return Ok(None);
    }

    // Binary file guard: if output contains null bytes, pass through unchanged
    if output_text.bytes().any(|b| b == 0) {
        return Ok(None);
    }

    // Short files pass through without compression
    let line_count = output_text.lines().count();
    if line_count < 50 {
        return Ok(None);
    }

    let config = match crate::config_loader::load_config() {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };

    // Aggressive read mode early-exit — bypasses BERT pipeline entirely
    {
        use ccr_core::config::ReadMode;
        if config.read.mode != ReadMode::Passthrough {
            use crate::handlers::{Handler, read::ReadHandlerLevel};
            let handler = ReadHandlerLevel::from_read_mode(&config.read.mode);
            let filtered = handler.filter(&output_text, &[file_path.clone()]);
            let in_tok  = ccr_core::tokens::count_tokens(&output_text);
            let out_tok = ccr_core::tokens::count_tokens(&filtered);
            crate::util::append_analytics(&ccr_core::analytics::Analytics::new(
                in_tok, out_tok, Some("(read-level)".to_string()), None, None,
            ));
            return Ok(Some(serde_json::to_string(&HookOutput { output: filtered })?));
        }
    }

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
        Err(_) => return Ok(None),
    };

    let _ = crate::zoom_store::save_blocks(&sid, result.zoom_blocks);

    // Session dedup using file_path as cmd_key.
    // Threshold scales by file size — see `read_dedup_threshold()`.
    let compressed = if let Ok(mut embs) =
        ccr_core::summarizer::embed_batch(&[result.output.as_str()])
    {
        if let Some(emb) = embs.pop() {
            let tokens = ccr_core::tokens::count_tokens(&result.output);
            let line_count = result.output.lines().count();
            let threshold = read_dedup_threshold(line_count);
            if let Some(hit) = session.find_similar_with_threshold(&file_path, &emb, threshold) {
                let age = crate::session::format_age(hit.age_secs);
                format!(
                    "[same file content as turn {} ({} ago) — {} tokens saved]",
                    hit.turn, age, hit.tokens_saved
                )
            } else {
                session.record(&file_path, emb, tokens, &result.output, false, None);
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
    Ok(Some(serde_json::to_string(&hook_output)?))
}

// ── Glob tool handler ─────────────────────────────────────────────────────────

fn process_glob(hook_input: HookInput) -> Result<Option<String>> {
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
        return Ok(None);
    }

    let paths: Vec<&str> = output_text.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = paths.len();

    // Short results pass through
    if total <= 20 {
        return Ok(None);
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
            return Ok(Some(serde_json::to_string(&hook_output)?));
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
                centroid_delta: None,
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
    Ok(Some(serde_json::to_string(&hook_output)?))
}

// ── Grep tool handler ─────────────────────────────────────────────────────────

fn process_grep(hook_input: HookInput) -> Result<Option<String>> {
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
        return Ok(None);
    }

    // Short results pass through unchanged
    if output_text.lines().count() <= 10 {
        return Ok(None);
    }

    use crate::handlers::Handler;
    let handler = crate::handlers::grep::GrepHandler;
    let args: Vec<String> = vec!["grep".to_string(), pattern];
    let filtered = handler.filter(&output_text, &args);

    let input_tokens = ccr_core::tokens::count_tokens(&output_text);
    let output_tokens = ccr_core::tokens::count_tokens(&filtered);
    let analytics = ccr_core::analytics::Analytics::new(
        input_tokens,
        output_tokens,
        Some("(grep-tool)".to_string()),
        None,
        None,
    );
    crate::util::append_analytics(&analytics);

    let hook_output = HookOutput { output: filtered };
    Ok(Some(serde_json::to_string(&hook_output)?))
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

/// Cosine similarity threshold for Read dedup, scaled by filtered output length.
/// Longer outputs need higher similarity to trigger dedup because a small
/// edit barely moves the overall BERT embedding.
///
/// Calibrated against all-MiniLM-L6-v2 on synthetic Rust files.
/// Re-validate if the embedding model changes:
///   θ=0.92 → ~25% of lines must change to drop below
///   θ=0.95 → ~8% of lines must change
///   θ=0.96 → ~4% of lines must change
fn read_dedup_threshold(line_count: usize) -> f32 {
    if line_count > 200 {
        0.96
    } else if line_count > 50 {
        0.95
    } else {
        0.92
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Fix 3: infrastructure command skip ───────────────────────────────────────

/// Commands that never produce meaningful output of their own.
/// Any output attributed to them is compound-command stdout leakage.
fn is_no_output_cmd(cmd: &str) -> bool {
    matches!(cmd, "sleep" | "wait" | "true" | "false" | ":")
}

// ── Fix 2: timestamp/counter normalization for polling dedup ─────────────────

/// Strip temporal noise (timestamps, counters, UUIDs, git SHAs, IPs) from
/// `text` before computing the embedding for polling suppressor B3 comparison.
/// The output itself is never modified — only the embedding input is normalized.
fn strip_temporal_noise(text: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?x)
            \d{4}-\d{2}-\d{2}[T\ ]\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?  # ISO timestamp
            | \d{2}:\d{2}:\d{2}(\.\d+)?          # HH:MM:SS
            | \d{2}:\d{2}                          # HH:MM
            | \b\d{10,13}\b                        # unix timestamp (10-13 digits)
            | \b\d+(\.\d+)?\s*(ms|µs|ns|seconds?|mins?|hours?) # durations
            | \b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b  # UUID
            | \b[0-9a-f]{40}\b                     # git SHA
            | \b0x[0-9a-f]{6,}\b                   # hex address
            | \b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b  # IPv4
            ",
        )
        .expect("strip_temporal_noise regex")
    });
    re.replace_all(text, "").to_string()
}

// ── Adaptive pressure helper (Feature 2) ─────────────────────────────────────

/// Convert centroid similarity to an additive pressure modifier.
/// Similarity < 0.85 → 0 (novel content, no extra pressure).
/// Similarity 0.85→1.0 → linearly maps to 0.0→0.32.
fn stability_to_pressure(sim: f32) -> f32 {
    if sim < 0.85 {
        return 0.0;
    }
    ((sim - 0.85) / 0.15 * 0.8).min(0.8) * 0.4
}

// ── Zoom label enrichment (Feature 3) ────────────────────────────────────────

/// Infer a human-readable category for a collapsed block from its lines.
fn categorize_block(lines: &[String]) -> &'static str {
    if lines.is_empty() {
        return "filtered output";
    }
    // Build/download progress: all lines start with well-known prefixes
    if lines.iter().all(|l| {
        let l = l.trim_start();
        l.starts_with("Compiling")
            || l.starts_with("Checking")
            || l.starts_with("Download")
            || l.starts_with("Fetching")
            || l.starts_with("Updating")
    }) {
        return "build/download progress";
    }

    let joined = lines.iter().map(|l| l.as_str()).collect::<Vec<_>>().join("\n");
    let low = joined.to_lowercase();

    if low.contains("error:") || low.contains("error[") {
        return "error details";
    }
    if low.contains("panicked") || low.contains("stack backtrace") {
        return "stack trace";
    }
    if (low.contains("passed") || low.contains(" ok")) && !low.contains("failed") && !low.contains("error:") {
        if low.contains("test") {
            return "passing tests";
        }
    }
    if low.contains("warning:") {
        return "compiler warnings";
    }
    if low.contains("note:") {
        return "compiler notes";
    }
    if lines.iter().all(|l| l.starts_with("  ") || l.starts_with('\t')) {
        return "indented detail";
    }
    "filtered output"
}

/// Replace generic zoom markers like `[20 matching lines collapsed — ccr expand ZI_1]`
/// with descriptive ones like `[20 lines: compiler warnings — ccr expand ZI_1]`.
fn enrich_zoom_labels(output: &str, zoom_blocks: &[ccr_core::zoom::ZoomBlock]) -> String {
    let mut result = output.to_string();
    for block in zoom_blocks {
        let label = categorize_block(&block.lines);
        let expand_tag = format!("ccr expand {}", block.id);
        result = result
            .lines()
            .map(|line| {
                if line.contains(&expand_tag) {
                    format!("[{} lines: {} — {}]", block.lines.len(), label, expand_tag)
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    result
}

// ── Cross-command semantic deduplication (Feature 4) ─────────────────────────

/// Split `text` into paragraph-sized segments (groups of non-blank lines separated
/// by blank lines). Only segments with at least `min_lines` lines are returned.
fn segment_output(text: &str, min_lines: usize) -> Vec<String> {
    let mut segments: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    for line in text.lines() {
        if line.trim().is_empty() {
            if current.len() >= min_lines {
                segments.push(current.join("\n"));
            }
            current.clear();
        } else {
            current.push(line);
        }
    }
    if current.len() >= min_lines {
        segments.push(current.join("\n"));
    }
    segments
}

/// Rebuild `original` output, replacing suppressed segments with zoom block markers.
/// `suppress[i] = Some(turn)` means segment `i` was seen in `turn` of a different command.
fn rebuild_with_suppressions(
    original: &str,
    segments: &[String],
    suppress: &[Option<usize>],
) -> String {
    // Build a lookup from segment text → referenced turn
    let suppressed: std::collections::HashMap<&str, usize> = segments
        .iter()
        .zip(suppress.iter())
        .filter_map(|(seg, s)| s.map(|turn| (seg.as_str(), turn)))
        .collect();

    if suppressed.is_empty() {
        return original.to_string();
    }

    let mut output_lines: Vec<String> = Vec::new();
    let mut current_para: Vec<&str> = Vec::new();
    // Chain a sentinel blank line so the last paragraph is always flushed.
    let lines_with_sentinel: Vec<&str> = original.lines().chain(std::iter::once("")).collect();

    for line in lines_with_sentinel {
        if line.trim().is_empty() {
            if !current_para.is_empty() {
                let para_text = current_para.join("\n");
                if let Some(&turn) = suppressed.get(para_text.as_str()) {
                    let n = current_para.len();
                    // Register as a zoom block so `ccr expand ZI_N` works.
                    let zi_id = ccr_core::zoom::register(
                        current_para.iter().map(|l| l.to_string()).collect(),
                    );
                    output_lines.push(format!(
                        "[{} lines: already shown (turn {}) — ccr expand {}]",
                        n, turn, zi_id
                    ));
                } else {
                    output_lines.extend(current_para.iter().map(|l| l.to_string()));
                    output_lines.push(String::new());
                }
                current_para.clear();
            }
        } else {
            current_para.push(line);
        }
    }

    // Strip trailing blank sentinel if present
    if output_lines.last().map_or(false, |l| l.is_empty()) {
        output_lines.pop();
    }

    output_lines.join("\n")
}

/// Suppress segments of the current output that are semantically similar
/// (cosine ≥ 0.85) to segments from the last 5 entries of *different* commands.
/// Replaces suppressed segments with zoom block back-reference markers.
///
/// Guards: only runs when output ≥ 20 lines, skips state commands, and never
/// suppresses more than 40% of total output lines.
fn cross_command_dedup(
    output: &str,
    cmd_key: &str,
    session: &crate::session::SessionState,
    sid: &str,
    is_state: bool,
) -> String {
    const MIN_LINES: usize = 20;
    const MIN_SEGMENT_LINES: usize = 5;
    const SIMILARITY_THRESHOLD: f32 = 0.85;
    const MAX_SUPPRESS_RATIO: f32 = 0.40;

    if is_state {
        return output.to_string();
    }
    let total_lines = output.lines().count();
    if total_lines < MIN_LINES {
        return output.to_string();
    }

    let current_segments = segment_output(output, MIN_SEGMENT_LINES);
    if current_segments.is_empty() {
        return output.to_string();
    }

    // Gather up to 5 entries from different commands (most recent first)
    let prior_entries: Vec<&crate::session::SessionEntry> = session
        .entries
        .iter()
        .rev()
        .filter(|e| e.cmd != cmd_key)
        .take(5)
        .collect();
    if prior_entries.is_empty() {
        return output.to_string();
    }

    // Build (turn, segment_text) pairs from prior entries
    let prior_segments: Vec<(usize, String)> = prior_entries
        .iter()
        .flat_map(|e| {
            let content = e.state_content.as_deref().unwrap_or(&e.content_preview);
            segment_output(content, MIN_SEGMENT_LINES)
                .into_iter()
                .map(|s| (e.turn, s))
        })
        .collect();

    if prior_segments.is_empty() {
        return output.to_string();
    }

    // Batch-embed all segments in one call
    let all_texts: Vec<&str> = current_segments
        .iter()
        .map(|s| s.as_str())
        .chain(prior_segments.iter().map(|(_, s)| s.as_str()))
        .collect();

    let embeddings = match ccr_core::summarizer::embed_batch(&all_texts) {
        Ok(e) => e,
        Err(_) => return output.to_string(),
    };

    let n_cur = current_segments.len();
    let cur_embs = &embeddings[..n_cur];
    let prior_embs = &embeddings[n_cur..];

    // For each current segment, find the most similar prior segment
    let mut suppress: Vec<Option<usize>> = vec![None; n_cur];
    let mut suppressed_lines = 0usize;

    for (i, cur_emb) in cur_embs.iter().enumerate() {
        for (j, prior_emb) in prior_embs.iter().enumerate() {
            let sim = crate::handlers::util::cosine_similarity(cur_emb, prior_emb);
            if sim >= SIMILARITY_THRESHOLD {
                suppress[i] = Some(prior_segments[j].0);
                suppressed_lines += current_segments[i].lines().count();
                break;
            }
        }
    }

    // Guard: don't suppress more than MAX_SUPPRESS_RATIO of all lines
    if suppressed_lines as f32 / total_lines as f32 > MAX_SUPPRESS_RATIO {
        return output.to_string();
    }

    if suppress.iter().all(|s| s.is_none()) {
        return output.to_string();
    }

    let _ = sid; // session_id reserved for future use (e.g. incremental zoom block saving)
    rebuild_with_suppressions(output, &current_segments, &suppress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_dedup_threshold_scales_with_size() {
        // Small files: most lenient
        assert_eq!(read_dedup_threshold(10), 0.92);
        assert_eq!(read_dedup_threshold(50), 0.92);
        // Medium files: stricter
        assert_eq!(read_dedup_threshold(51), 0.95);
        assert_eq!(read_dedup_threshold(200), 0.95);
        // Large files: strictest
        assert_eq!(read_dedup_threshold(201), 0.96);
        assert_eq!(read_dedup_threshold(5000), 0.96);
    }

    #[test]
    fn read_dedup_threshold_zero_lines() {
        assert_eq!(read_dedup_threshold(0), 0.92);
    }

    #[test]
    fn read_dedup_threshold_monotonically_increases() {
        let small = read_dedup_threshold(30);
        let medium = read_dedup_threshold(100);
        let large = read_dedup_threshold(500);
        assert!(small <= medium);
        assert!(medium <= large);
    }

    #[test]
    fn is_no_output_cmd_matches_infrastructure() {
        assert!(is_no_output_cmd("sleep"));
        assert!(is_no_output_cmd("wait"));
        assert!(is_no_output_cmd("true"));
        assert!(is_no_output_cmd("false"));
        assert!(is_no_output_cmd(":"));
    }

    #[test]
    fn is_no_output_cmd_does_not_match_real_commands() {
        assert!(!is_no_output_cmd("cargo"));
        assert!(!is_no_output_cmd("git"));
        assert!(!is_no_output_cmd("curl"));
        assert!(!is_no_output_cmd("cd"));
        assert!(!is_no_output_cmd("echo"));
        assert!(!is_no_output_cmd("export"));
    }

    #[test]
    fn strip_temporal_noise_removes_iso_timestamp() {
        let input = "Started at 2026-04-08T21:16:30Z, status: running";
        let out = strip_temporal_noise(input);
        assert!(!out.contains("2026-04-08"), "got: {}", out);
        assert!(out.contains("status: running"), "got: {}", out);
    }

    #[test]
    fn strip_temporal_noise_removes_hms_time() {
        let input = "Log entry 21:16:30 — process started";
        let out = strip_temporal_noise(input);
        assert!(!out.contains("21:16:30"), "got: {}", out);
        assert!(out.contains("process started"), "got: {}", out);
    }

    #[test]
    fn strip_temporal_noise_removes_uuid() {
        let input = "id=550e8400-e29b-41d4-a716-446655440000 status=ok";
        let out = strip_temporal_noise(input);
        assert!(!out.contains("550e8400"), "got: {}", out);
        assert!(out.contains("status=ok"), "got: {}", out);
    }

    #[test]
    fn strip_temporal_noise_preserves_semantic_content() {
        let input = "Build failed: missing symbol `foo` in auth.rs";
        let out = strip_temporal_noise(input);
        assert!(out.contains("Build failed"), "got: {}", out);
        assert!(out.contains("missing symbol"), "got: {}", out);
        assert!(out.contains("auth.rs"), "got: {}", out);
    }
}
