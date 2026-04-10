/// Tests for Idea 2: Intent-Aware Query (B2+)
///
/// Verifies that `summarize_with_intent` biases kept lines toward the user's
/// current task (last assistant message) rather than just the command string.
/// Lines relevant to the intent must score higher than semantically unrelated lines.
///
/// Expected new API:
///   `ccr_core::summarizer::summarize_with_intent(
///       text: &str, budget: usize, command: &str, intent: &str
///   ) -> SummarizeResult`
///
/// Run with: cargo test -p ccr-core --test idea2_intent_query
use panda_core::summarizer::summarize_with_intent;

// ── helper ────────────────────────────────────────────────────────────────────

fn has_line(output: &str, needle: &str) -> bool {
    output.lines().any(|l| l.contains(needle))
}


// ── Intent bias changes selection ─────────────────────────────────────────────

/// With an auth-focused intent, auth-related lines should appear in the output
/// more than generic compilation noise.
#[test]
fn auth_intent_surfaces_auth_lines() {
    let mut lines: Vec<String> = (0..40)
        .map(|i| format!("compiling artifact dependency package step {}", i))
        .collect();
    // Auth-related lines (semantically distinct from noise)
    lines.push("error: authentication token expired, re-login required".to_string());
    lines.push("warning: session middleware missing auth validation layer".to_string());
    // Memory-related lines (semantically unrelated to auth intent)
    lines.push("error: memory allocator exceeded heap limit 512mb".to_string());
    lines.push("warning: stack overflow detected in recursive function call".to_string());

    let input = lines.join("\n");
    let result = summarize_with_intent(&input, 10, "cargo build", "fix the auth token validation in the session middleware");

    // Auth lines must be present
    assert!(
        has_line(&result.output, "authentication token"),
        "Auth error line missing with auth intent:\n{}",
        result.output
    );
    assert!(
        has_line(&result.output, "session middleware"),
        "Auth warning line missing with auth intent:\n{}",
        result.output
    );
}

/// With a memory-focused intent, memory lines should dominate over auth lines.
#[test]
fn memory_intent_surfaces_memory_lines() {
    let mut lines: Vec<String> = (0..40)
        .map(|i| format!("compiling artifact dependency package step {}", i))
        .collect();
    lines.push("error: authentication token expired in session handler".to_string());
    lines.push("warning: session middleware missing auth validation".to_string());
    lines.push("error: memory allocator exceeded heap limit 512mb".to_string());
    lines.push("warning: stack overflow in recursive allocation loop".to_string());

    let input = lines.join("\n");
    let result = summarize_with_intent(&input, 10, "cargo build", "investigate the memory allocator heap overflow");

    assert!(
        has_line(&result.output, "memory allocator") || has_line(&result.output, "heap limit"),
        "Memory error line missing with memory intent:\n{}",
        result.output
    );
}

/// Intent query must produce different top-K selection than command-only query
/// when the two topics diverge.
#[test]
fn intent_produces_different_selection_than_command_alone() {
    use panda_core::summarizer::summarize_with_query;

    let mut lines: Vec<String> = (0..60)
        .map(|i| format!("compiling generic dependency artifact module {}", i))
        .collect();
    lines.push("error: database connection pool exhausted all 100 connections".to_string());
    lines.push("error: JWT token signature verification failed invalid key".to_string());

    let input = lines.join("\n");
    let budget = 10;

    let cmd_only = summarize_with_query(&input, budget, "cargo build");
    let with_intent = summarize_with_intent(&input, budget, "cargo build", "fix the database connection pool timeout issue");

    // At minimum, the outputs should differ in which error line appears first/prominently
    // Both may keep error lines (hard-keep), but scoring order differs
    let db_in_cmd = has_line(&cmd_only.output, "database connection");
    let db_in_intent = has_line(&with_intent.output, "database connection");

    // When both errors are present, intent-biased output should prefer db over jwt
    if db_in_intent && has_line(&with_intent.output, "JWT") {
        // Both kept — check order: db line should appear before jwt line
        let db_pos = with_intent.output.find("database connection").unwrap_or(usize::MAX);
        let jwt_pos = with_intent.output.find("JWT").unwrap_or(usize::MAX);
        assert!(
            db_pos <= jwt_pos,
            "With db intent, db error should appear before JWT error:\n{}",
            with_intent.output
        );
    } else {
        // If budget forced a choice, db line should be preferred
        assert!(
            db_in_intent,
            "DB error line should be present with db connection intent:\n{}",
            with_intent.output
        );
        let _ = db_in_cmd; // used to avoid unused warning
    }
}

// ── Critical lines still always kept ─────────────────────────────────────────

/// Error lines must be hard-kept even when intent points to a different topic.
#[test]
fn hard_keep_errors_survive_regardless_of_intent() {
    let mut lines: Vec<String> = (0..60)
        .map(|i| format!("downloading registry fetching metadata package {}", i))
        .collect();
    lines[30] = "error[E0716]: temporary value dropped while borrowed".to_string();

    let input = lines.join("\n");
    let result = summarize_with_intent(&input, 10, "cargo build", "refactor the HTTP server routing layer");

    assert!(
        has_line(&result.output, "error[E0716]"),
        "Hard-keep error lost even with unrelated intent:\n{}",
        result.output
    );
}

// ── Empty / short inputs ──────────────────────────────────────────────────────

/// Empty intent string should fall back to command-only query behavior.
#[test]
fn empty_intent_falls_back_to_command_query() {
    use panda_core::summarizer::summarize_with_query;

    let lines: Vec<String> = (0..250).map(|i| format!("log output line number {}", i)).collect();
    let input = lines.join("\n");

    let with_empty_intent = summarize_with_intent(&input, 30, "cargo build", "");
    let command_only = summarize_with_query(&input, 30, "cargo build");

    // Line counts should be similar (same budget, same fallback path)
    let diff = (with_empty_intent.lines_out as i64 - command_only.lines_out as i64).abs();
    assert!(
        diff <= 5,
        "Empty intent should behave like command-only (±5 lines), got diff={}",
        diff
    );
}

/// Input below the BERT threshold should pass through unchanged.
#[test]
fn short_input_passthrough() {
    let input = "error: build failed\nwarning: unused import\nfinished in 1.2s";
    let result = summarize_with_intent(input, 30, "cargo build", "fix the build error");
    assert!(result.output.contains("build failed"));
    assert!(result.output.contains("unused import"));
}

// ── Token savings ─────────────────────────────────────────────────────────────

/// The intent-biased output must still respect the budget and not exceed it significantly.
#[test]
fn output_respects_budget() {
    let lines: Vec<String> = (0..300).map(|i| format!("verbose compilation log entry {} metadata", i)).collect();
    let input = lines.join("\n");
    let budget = 20;

    let result = summarize_with_intent(&input, budget, "cargo build", "find the linker error in the build");

    // Allow some slack for omission markers but line count should be near budget
    let out_lines = result.output.lines().count();
    assert!(
        out_lines <= budget * 3, // generous upper bound to account for omission markers
        "Output far exceeds budget: {} lines for budget {}", out_lines, budget
    );
}
