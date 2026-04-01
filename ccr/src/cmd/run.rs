use anyhow::Result;
use ccr_core::tokens;
use std::path::PathBuf;
use std::process::Command;

pub fn run(args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("ccr run: no command specified");
    }

    let cmd_name = args[0].clone();
    // Extract subcommand: first non-flag argument after argv[0]
    // Skip flags like -C, --no-pager so "git -C /path status" → subcommand "status"
    let subcommand = args.iter().skip(1).find(|s| !s.starts_with('-')).cloned();

    // SD: use cmd + subcommand as the delta/session key for better granularity.
    // "git status" history won't match "git log" history.
    let delta_key = match &subcommand {
        Some(sub) => format!("{} {}", cmd_name, sub),
        None => cmd_name.clone(),
    };

    // PC: check structural cache before executing — skip the command entirely on a hit.
    let pre_cache_key = crate::pre_cache::PreCache::compute_key(&args);
    {
        let pre_cache = crate::pre_cache::PreCache::load(&crate::session::session_id());
        if let Some(ref pck) = pre_cache_key {
            if let Some(entry) = pre_cache.lookup(pck) {
                let age = crate::session::format_age(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                        .saturating_sub(entry.ts),
                );
                let mut out = entry.output.clone();
                out.push_str(&format!(
                    "\n[PC: cached from {} ago — ~{} tokens saved; key {}]",
                    age,
                    entry.tokens,
                    &pck.key[..8.min(pck.key.len())]
                ));
                print!("{}", out);
                if !out.ends_with('\n') {
                    println!();
                }
                let analytics = ccr_core::analytics::Analytics::new(
                    entry.tokens,
                    0,
                    Some(cmd_name),
                    subcommand,
                    Some(0),
                );
                append_analytics(&analytics);
                return Ok(());
            }
        }
    }

    let handler = crate::handlers::get_handler(&cmd_name);

    // Rewrite args (e.g. inject --message-format json for cargo)
    let final_args: Vec<String> = if let Some(ref h) = handler {
        h.rewrite_args(&args)
    } else {
        args.clone()
    };

    // Execute the command, capturing stdout+stderr
    let t0 = std::time::Instant::now();
    let output = Command::new(&final_args[0])
        .args(&final_args[1..])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to execute '{}': {}", cmd_name, e))?;
    let duration_ms = t0.elapsed().as_millis() as u64;

    let raw_output = {
        let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !stderr.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&stderr);
        }
        combined
    };

    // Tee: save raw output to disk
    let tee_path = write_tee(&cmd_name, &raw_output);

    // ZI: enable Zoom-In so compressed markers include expand IDs.
    ccr_core::zoom::enable();

    // NL: apply project noise pre-filter before the pipeline sees the output.
    let project_key = crate::util::project_key();
    let (raw_output_for_learning, pipeline_input) = {
        let noise_store = project_key.as_ref().map(|k| crate::noise_learner::NoiseStore::load(k));
        if let Some(ref store) = noise_store {
            let lines: Vec<&str> = raw_output.lines().collect();
            let kept = store.apply_pre_filter(&lines);
            if kept.len() < lines.len() {
                (raw_output.clone(), kept.join("\n"))
            } else {
                (raw_output.clone(), raw_output.clone())
            }
        } else {
            (raw_output.clone(), raw_output.clone())
        }
    };

    // Filter the output
    let filtered = if let Some(ref h) = handler {
        h.filter(&pipeline_input, &args)
    } else {
        // Pipeline fallback for unknown commands
        let config = match crate::config_loader::load_config() {
            Ok(c) => c,
            Err(_) => ccr_core::config::CcrConfig::default(),
        };
        // EC: tighten pipeline proportionally to session context pressure.
        let pressure = {
            let sid_p = crate::session::session_id();
            crate::session::SessionState::load(&sid_p).context_pressure()
        };
        let pipeline = ccr_core::pipeline::Pipeline::new(config.with_pressure(pressure));
        match pipeline.process(&pipeline_input, Some(&cmd_name), Some(&cmd_name), None) {
            Ok(r) => {
                // Persist zoom blocks from pipeline fallback.
                let sid_z = crate::session::session_id();
                let _ = crate::zoom_store::save_blocks(&sid_z, r.zoom_blocks);
                r.output
            }
            Err(_) => pipeline_input.clone(),
        }
    };

    // NL: record what was suppressed so we can learn project noise patterns.
    if let Some(ref key) = project_key {
        let mut store = crate::noise_learner::NoiseStore::load(key);
        let input_lines: Vec<&str> = raw_output_for_learning.lines().collect();
        let output_lines: Vec<&str> = filtered.lines().collect();
        store.record_lines(&input_lines, &output_lines);
        store.promote_eligible();
        store.evict_stale(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        );
        store.save(key);
    }

    // CR: content-retrieval subcommands (git show, git cat-file, etc.) must never have
    // dedup/delta annotations injected — their output is typically piped or redirected
    // to a file, and annotations would corrupt it.
    let is_content_retrieval = cmd_name == "git"
        && matches!(
            subcommand.as_deref(),
            Some("show") | Some("cat-file") | Some("archive") | Some("bundle")
        );

    // Idea 3: Delta compression — suppress lines seen in a prior run of the same command.
    // Skip for short outputs (< 20 lines) where delta overhead exceeds savings.
    let filtered = if is_content_retrieval {
        filtered
    } else if filtered.lines().count() >= 20 {
        if let Ok(mut embs) = ccr_core::summarizer::embed_batch(&[filtered.as_str()]) {
            if let Some(emb) = embs.pop() {
                let sid_pre = crate::session::session_id();
                let session_pre = crate::session::SessionState::load(&sid_pre);
                let lines: Vec<&str> = filtered.lines().collect();
                match session_pre.compute_delta(&delta_key, &lines, &emb) {
                    Some(delta) => delta.output,
                    None => filtered,
                }
            } else {
                filtered
            }
        } else {
            filtered
        }
    } else {
        filtered
    };

    // SD: determine if this is a state command for full-content storage.
    let is_state = {
        if let Ok(cfg) = crate::config_loader::load_config() {
            cfg.global.state_commands.iter().any(|s| s == &cmd_name)
        } else {
            false
        }
    };

    // B3: Session cache — check for semantically identical prior output, record new one.
    // Skip for short outputs: the dedup message itself would be longer than the original.
    // Skip for content-retrieval subcommands: annotations must not appear in raw content.
    let sid = crate::session::session_id();
    let mut session = crate::session::SessionState::load(&sid);
    let filtered = if is_content_retrieval {
        filtered
    } else if ccr_core::tokens::count_tokens(&filtered) < 30 {
        let tokens = ccr_core::tokens::count_tokens(&filtered);
        if let Ok(mut embs) = ccr_core::summarizer::embed_batch(&[filtered.as_str()]) {
            if let Some(emb) = embs.pop() {
                session.record(&delta_key, emb, tokens, &filtered, is_state);
                session.save(&sid);
            }
        }
        filtered
    } else if let Ok(mut embeddings) =
        ccr_core::summarizer::embed_batch(&[filtered.as_str()])
    {
        if let Some(emb) = embeddings.pop() {
            // State commands (git, kubectl, etc.) use exact-content dedup:
            // two different git states can embed at >0.92 similarity while the
            // actual output has changed. Semantic dedup is only safe for
            // non-state commands (build output, test results, etc.).
            let hit = if is_state {
                session.find_exact(&delta_key, &filtered)
            } else {
                session.find_similar(&delta_key, &emb)
            };
            if let Some(hit) = hit {
                let age = crate::session::format_age(hit.age_secs);
                format!(
                    "[same output as turn {} ({} ago) — {} tokens saved]",
                    hit.turn, age, hit.tokens_saved
                )
            } else {
                let tokens = ccr_core::tokens::count_tokens(&filtered);
                session.record(&delta_key, emb, tokens, &filtered, is_state);
                session.save(&sid);
                filtered
            }
        } else {
            filtered
        }
    } else {
        filtered
    };

    // PC: write-through — store the filtered result for future cache hits.
    if let Some(ref pck) = pre_cache_key {
        let tokens_for_cache = ccr_core::tokens::count_tokens(&filtered);
        let mut pc = crate::pre_cache::PreCache::load(&crate::session::session_id());
        pc.evict_old();
        pc.insert(pck.clone(), &filtered, tokens_for_cache);
        pc.save(&crate::session::session_id());
    }

    // Compute analytics
    let input_tokens = tokens::count_tokens(&raw_output);
    let output_tokens = tokens::count_tokens(&filtered);
    let savings_pct = if input_tokens == 0 {
        0.0
    } else {
        let saved = input_tokens.saturating_sub(output_tokens);
        (saved as f32 / input_tokens as f32) * 100.0
    };

    // Append tee hint if compression is aggressive (>60%) and tee was written
    let mut display_output = filtered.clone();
    if savings_pct > 60.0 {
        if let Some(ref path) = tee_path {
            display_output.push_str(&format!("\n[full output: {}]", path.display()));
        }
    }

    print!("{}", display_output);
    // Ensure trailing newline
    if !display_output.ends_with('\n') {
        println!();
    }

    // Record analytics
    let analytics = ccr_core::analytics::Analytics::new(
        input_tokens,
        output_tokens,
        Some(cmd_name),
        subcommand,
        Some(duration_ms),
    );
    append_analytics(&analytics);

    // Propagate exit code
    let code = output.status.code().unwrap_or(1);
    if code != 0 {
        std::process::exit(code);
    }

    Ok(())
}

fn write_tee(cmd: &str, content: &str) -> Option<PathBuf> {
    let tee_dir = dirs::data_local_dir()?.join("ccr").join("tee");
    std::fs::create_dir_all(&tee_dir).ok()?;

    // Rotate: keep max 20 files
    if let Ok(entries) = std::fs::read_dir(&tee_dir) {
        let mut log_files: Vec<(std::time::SystemTime, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .ends_with(".log")
            })
            .filter_map(|e| {
                let modified = e.metadata().ok()?.modified().ok()?;
                Some((modified, e.path()))
            })
            .collect();

        if log_files.len() >= 20 {
            log_files.sort_by_key(|(t, _)| *t);
            for (_, path) in log_files.iter().take(log_files.len() - 19) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let safe_cmd = cmd
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>();

    let path = tee_dir.join(format!("{}_{}.log", ts, safe_cmd));
    std::fs::write(&path, content).ok()?;
    Some(path)
}

fn append_analytics(analytics: &ccr_core::analytics::Analytics) {
    crate::util::append_analytics(analytics);
}
