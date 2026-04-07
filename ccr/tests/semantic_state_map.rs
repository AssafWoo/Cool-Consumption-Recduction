/// Integration tests for SD — Semantic State Map.
///
/// Verifies subcommand-aware delta keys, full-content storage for state commands,
/// richer delta output format, and serde backward compatibility.
use ccr::session::SessionState;
use ccr_core::summarizer::embed_batch;

fn embed(text: &str) -> Vec<f32> {
    embed_batch(&[text]).unwrap().pop().unwrap()
}

fn session_with_prior(cmd: &str, content: &str, is_state: bool) -> SessionState {
    let mut s = SessionState::default();
    let emb = embed(content);
    let tokens = content.len() / 4;
    s.record(cmd, emb, tokens, content, is_state, None);
    s
}

// ── subcommand-aware delta key ────────────────────────────────────────────────

#[test]
fn git_status_and_git_log_do_not_delta_each_other() {
    let status_output = "On branch main\nnothing to commit, working tree clean\nYour branch is up to date with 'origin/main'.";
    let session = session_with_prior("git status", status_output, true);

    // A "git log" output is structurally different — should not match
    let log_output = "commit abc123def456\nAuthor: Alice <alice@example.com>\nDate: Mon Mar 18 2026\n\n    fix: typo in README\n\ncommit bbb222\nAuthor: Bob <bob@example.com>\nDate: Sun Mar 17 2026\n\n    feat: add new feature";
    let emb = embed(log_output);
    let lines: Vec<&str> = log_output.lines().collect();

    let result = session.compute_delta("git log", &lines, &emb);
    assert!(result.is_none(), "git log should not match git status history");
}

#[test]
fn git_status_repeated_triggers_delta() {
    // Two nearly-identical git status runs should match.
    let content = "On branch main\nChanges not staged for commit:\n\tmodified: src/main.rs\n\tmodified: src/lib.rs\n\nno changes added to commit";
    let session = session_with_prior("git status", content, true);

    // Same output — delta should fire
    let emb = embed(content);
    let lines: Vec<&str> = content.lines().collect();
    let result = session.compute_delta("git status", &lines, &emb);
    assert!(result.is_some(), "identical git status should trigger delta");
}

// ── state command full-content storage ────────────────────────────────────────

#[test]
fn state_command_stores_full_content_beyond_4000_chars() {
    // Build content clearly exceeding 4000 chars
    let long_content: String = (0..200)
        .map(|i| format!("  modified:   src/module_{:04}.rs\n", i))
        .collect();
    assert!(long_content.len() > 4000, "test setup: content must exceed 4000 chars");

    let mut s = SessionState::default();
    let emb = embed(&long_content[..200]); // embed a short proxy
    s.record("git", emb, long_content.len() / 4, &long_content, true, None);

    let entry = &s.entries[0];
    assert!(
        entry.state_content.is_some(),
        "state command must have state_content"
    );
    assert_eq!(
        entry.state_content.as_ref().unwrap().len(),
        long_content.len(),
        "state_content must store full content"
    );
}

#[test]
fn non_state_command_caps_preview_at_4000() {
    let long_content: String = (0..500).map(|i| format!("log output line number {:04}: some extra padding to make it longer\n", i)).collect();
    assert!(long_content.len() > 4000);

    let mut s = SessionState::default();
    let emb = embed("cargo build");
    s.record("cargo", emb, long_content.len() / 4, &long_content, false, None);

    let entry = &s.entries[0];
    assert!(
        entry.state_content.is_none(),
        "non-state command must not have state_content"
    );
    assert!(
        entry.content_preview.len() <= 4000,
        "preview must be capped at 4000 chars"
    );
}

#[test]
fn state_content_used_for_delta_beyond_preview_boundary() {
    // Build a long git status output where changes appear after the 4000-char mark.
    let lines: Vec<String> = (0..150)
        .map(|i| format!("  modified:   src/module_{:04}.rs", i))
        .collect();
    let content = lines.join("\n");
    assert!(content.len() > 4000, "test setup: must exceed preview boundary");

    let session = session_with_prior("git status", &content, true);

    // New run: same 150 lines but the last one changed (appears after 4000-char boundary)
    let mut new_lines = lines.clone();
    *new_lines.last_mut().unwrap() = "  deleted:    src/module_0149.rs".to_string();
    let new_text = new_lines.join("\n");

    let emb = embed(&new_text);
    let refs: Vec<&str> = new_text.lines().collect();

    let result = session
        .compute_delta("git status", &refs, &emb)
        .expect("delta should fire for similar git status");

    // The changed last line should appear in output (new line)
    assert!(
        result.output.contains("module_0149") || result.new_count >= 1,
        "change beyond 4000-char boundary must be detected in delta output"
    );
}

// ── richer delta output format ────────────────────────────────────────────────

#[test]
fn delta_output_uses_richer_format() {
    let content = (0..25)
        .map(|i| format!("cargo:warning=unused variable `var{}`", i))
        .collect::<Vec<_>>()
        .join("\n");
    let session = session_with_prior("cargo build", &content, false);

    let mut new_lines: Vec<String> = (0..25)
        .map(|i| format!("cargo:warning=unused variable `var{}`", i))
        .collect();
    new_lines.push("error[E0308]: mismatched types in src/main.rs:42".to_string());
    let new_text = new_lines.join("\n");
    let emb = embed(&new_text);
    let refs: Vec<&str> = new_text.lines().collect();

    let result = session
        .compute_delta("cargo build", &refs, &emb)
        .expect("delta should fire");

    assert!(
        result.output.contains("Δ from turn"),
        "output should use new richer format, got: {}",
        &result.output[..result.output.len().min(200)]
    );
    assert!(
        !result.output.contains("lines same as turn"),
        "old format must not appear"
    );
    assert!(
        result.output.contains("error[E0308]"),
        "new error line must survive delta"
    );
}

#[test]
fn richer_format_contains_new_and_repeated_counts() {
    let content = (0..20)
        .map(|i| format!("log line {}: everything is fine", i))
        .collect::<Vec<_>>()
        .join("\n");
    let session = session_with_prior("myapp", &content, false);

    let mut new_lines: Vec<String> = (0..20)
        .map(|i| format!("log line {}: everything is fine", i))
        .collect();
    new_lines.push("CRITICAL: disk full".to_string());
    new_lines.push("CRITICAL: out of memory".to_string());
    let new_text = new_lines.join("\n");
    let emb = embed(&new_text);
    let refs: Vec<&str> = new_text.lines().collect();

    let result = session.compute_delta("myapp", &refs, &emb).expect("delta should fire");
    // The marker should mention +2 new
    assert!(result.new_count >= 2, "should have at least 2 new lines");
    assert!(result.same_count > 0, "should have repeated lines");
}

// ── serde backward compatibility ──────────────────────────────────────────────

#[test]
fn session_with_missing_state_content_deserializes_ok() {
    let json = r#"{
        "entries": [{
            "turn": 1, "cmd": "git", "ts": 0, "tokens": 10,
            "embedding": [0.1, 0.2],
            "content_preview": "On branch main"
        }],
        "total_turns": 1,
        "total_tokens": 10,
        "command_centroids": {}
    }"#;
    let session: SessionState = serde_json::from_str(json).expect("must deserialize without state_content");
    assert!(
        session.entries[0].state_content.is_none(),
        "missing state_content field should default to None"
    );
}

// ── config defaults ───────────────────────────────────────────────────────────

#[test]
fn state_commands_default_includes_git_and_kubectl() {
    let config = ccr_core::config::CcrConfig::default();
    assert!(
        config.global.state_commands.iter().any(|s| s == "git"),
        "git must be in default state_commands"
    );
    assert!(
        config.global.state_commands.iter().any(|s| s == "kubectl"),
        "kubectl must be in default state_commands"
    );
}

#[test]
fn state_commands_can_be_overridden_in_toml() {
    let toml_str = r#"
[global]
state_commands = ["custom_tool", "my_status"]
"#;
    let config: ccr_core::config::CcrConfig = toml::from_str(toml_str).unwrap();
    assert!(config.global.state_commands.contains(&"custom_tool".to_string()));
    assert!(
        !config.global.state_commands.contains(&"git".to_string()),
        "explicit state_commands should replace defaults"
    );
}
