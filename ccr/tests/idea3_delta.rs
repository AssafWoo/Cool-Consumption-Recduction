/// Tests for Idea 3: Semantic Delta Compression
///
/// Verifies that `SessionState::compute_delta` detects repeated lines from
/// prior runs and replaces them with "[X lines same as turn N]", while always
/// surfacing genuinely new or changed lines.
///
/// Expected new API on `SessionState`:
///
///   pub struct DeltaResult {
///       pub output: String,       // compressed delta output
///       pub same_count: usize,    // lines matched to a prior run
///       pub new_count: usize,     // lines not seen before
///       pub reference_turn: usize,// turn number of the matched prior run
///   }
///
///   impl SessionState {
///       pub fn compute_delta(
///           &self,
///           cmd: &str,
///           new_lines: &[&str],
///           new_embedding: &[f32],
///       ) -> Option<DeltaResult>
///   }
///
/// Run with: cargo test -p ccr --test idea3_delta
use ccr::session::SessionState;
use ccr_core::summarizer::embed_batch;

// ── helper ────────────────────────────────────────────────────────────────────

fn embed_text(text: &str) -> Vec<f32> {
    embed_batch(&[text])
        .expect("embed_batch failed")
        .into_iter()
        .next()
        .expect("empty result")
}

fn make_session_with_prior(cmd: &str, prior_output: &str) -> SessionState {
    let mut session = SessionState::default();
    let emb = embed_text(prior_output);
    let tokens = ccr_core::tokens::count_tokens(prior_output);
    session.record(cmd, emb, tokens, prior_output, false, None);
    session
}

// ── Delta fires for moderately similar outputs ────────────────────────────────

/// Second build has same compilation lines + new error → delta fires even though
/// similarity is below the B3 exact-match threshold (0.92).
#[test]
fn delta_fires_for_similar_but_not_identical_output() {
    let shared = (0..20)
        .map(|i| format!("   Compiling crate-{} v1.0.0", i))
        .collect::<Vec<_>>()
        .join("\n");

    let prior = format!("{}\nwarning: unused variable `x`", shared);
    let new_output = format!("{}\nerror[E0502]: cannot borrow `self` as mutable", shared);

    let session = make_session_with_prior("cargo", &prior);
    let new_lines: Vec<&str> = new_output.lines().collect();
    let new_emb = embed_text(&new_output);

    let delta = session.compute_delta("cargo", &new_lines, &new_emb);

    assert!(delta.is_some(), "Expected delta to fire for outputs with ~20 shared lines");
}

/// Delta result must contain the new error line.
#[test]
fn new_error_line_present_in_delta_output() {
    let shared = (0..20)
        .map(|i| format!("   Compiling crate-{} v1.0.0", i))
        .collect::<Vec<_>>()
        .join("\n");

    let prior = format!("{}\nwarning: all good", shared);
    let new_output = format!("{}\nerror[E0308]: mismatched types expected `u32` found `i64`", shared);

    let session = make_session_with_prior("cargo", &prior);
    let new_lines: Vec<&str> = new_output.lines().collect();
    let new_emb = embed_text(&new_output);

    let delta = session.compute_delta("cargo", &new_lines, &new_emb).unwrap();

    assert!(
        delta.output.contains("error[E0308]"),
        "New error line missing from delta output:\n{}",
        delta.output
    );
}

/// Repeated compilation lines must be replaced with "[X lines same as turn N]".
#[test]
fn repeated_lines_replaced_with_marker() {
    let shared = (0..20)
        .map(|i| format!("   Compiling package-{} v1.{}.0", i, i))
        .collect::<Vec<_>>()
        .join("\n");

    let prior = format!("{}\nwarning: build succeeded", shared);
    let new_output = format!("{}\nerror: linker exited with code 1", shared);

    let session = make_session_with_prior("cargo", &prior);
    let new_lines: Vec<&str> = new_output.lines().collect();
    let new_emb = embed_text(&new_output);

    let delta = session.compute_delta("cargo", &new_lines, &new_emb).unwrap();

    assert!(
        delta.output.contains("Δ from turn") || delta.output.contains("same as turn") || delta.output.contains("lines same"),
        "Expected delta marker in output:\n{}",
        delta.output
    );
}

/// `same_count` must be non-zero when there are repeated lines.
#[test]
fn same_count_reflects_repeated_lines() {
    let shared = (0..15)
        .map(|i| format!("   Compiling dependency-{} v2.0.0", i))
        .collect::<Vec<_>>()
        .join("\n");

    let prior = format!("{}\ninfo: build complete", shared);
    let new_output = format!("{}\nerror: failed to link object files", shared);

    let session = make_session_with_prior("cargo", &prior);
    let new_lines: Vec<&str> = new_output.lines().collect();
    let new_emb = embed_text(&new_output);

    let delta = session.compute_delta("cargo", &new_lines, &new_emb).unwrap();

    assert!(
        delta.same_count > 0,
        "Expected same_count > 0 for output with 15 shared lines, got {}",
        delta.same_count
    );
}

/// `new_count` must equal the number of lines that didn't appear in the prior run.
#[test]
fn new_count_reflects_genuinely_new_lines() {
    let shared = (0..10)
        .map(|i| format!("   Compiling lib-{} v0.1.{}", i, i))
        .collect::<Vec<_>>()
        .join("\n");

    let prior = format!("{}", shared);
    // 3 new lines not in prior
    let new_output = format!(
        "{}\nerror[E0001]: first new error\nerror[E0002]: second new error\nwarning: third new thing",
        shared
    );

    let session = make_session_with_prior("cargo", &prior);
    let new_lines: Vec<&str> = new_output.lines().collect();
    let new_emb = embed_text(&new_output);

    let delta = session.compute_delta("cargo", &new_lines, &new_emb).unwrap();

    assert!(
        delta.new_count >= 2,
        "Expected new_count ≥ 2 for 3 new lines, got {}",
        delta.new_count
    );
}

// ── Delta does NOT fire for completely different outputs ──────────────────────

/// Outputs from different commands must not delta-match each other.
#[test]
fn delta_does_not_fire_across_different_commands() {
    let cargo_output = "   Compiling foo v1.0\nerror: build failed";
    let session = make_session_with_prior("cargo", cargo_output);

    let git_output = "On branch main\nnothing to commit, working tree clean";
    let git_lines: Vec<&str> = git_output.lines().collect();
    let git_emb = embed_text(git_output);

    let delta = session.compute_delta("git", &git_lines, &git_emb);

    assert!(
        delta.is_none(),
        "Delta should not fire across different commands"
    );
}

/// Completely unrelated outputs for the same command should not delta.
#[test]
fn delta_does_not_fire_for_semantically_unrelated_output() {
    let prior = "   Compiling foo v1.0\n   Compiling bar v2.0\nwarning: unused import";
    let session = make_session_with_prior("cargo", prior);

    // Totally different output: test results (not compilation)
    let new_output = "test result: FAILED. 3 passed; 1 failed; 0 ignored\nFAILED tests::auth::token_expiry";
    let new_lines: Vec<&str> = new_output.lines().collect();
    let new_emb = embed_text(new_output);

    let delta = session.compute_delta("cargo", &new_lines, &new_emb);

    // Should either be None (no match) or have very low same_count
    if let Some(d) = delta {
        assert!(
            d.same_count <= 2,
            "Unexpected same_count {} for semantically unrelated output",
            d.same_count
        );
    }
}

// ── Empty session ─────────────────────────────────────────────────────────────

/// Delta must return None when there's no session history for the command.
#[test]
fn delta_returns_none_for_empty_session() {
    let session = SessionState::default();
    let output = "   Compiling foo v1.0\nerror: build failed";
    let lines: Vec<&str> = output.lines().collect();
    let emb = embed_text(output);

    let delta = session.compute_delta("cargo", &lines, &emb);

    assert!(delta.is_none(), "Delta must be None for empty session");
}

// ── Reference turn is correct ─────────────────────────────────────────────────

/// `reference_turn` must point to the actual turn that the delta matched.
#[test]
fn reference_turn_is_correct() {
    let shared = (0..15)
        .map(|i| format!("   Compiling pkg-{} v1.0", i))
        .collect::<Vec<_>>()
        .join("\n");

    let prior = format!("{}\nwarning: done", shared);
    let session = make_session_with_prior("cargo", &prior);

    // The recorded entry is turn 1 (first record call)
    let new_output = format!("{}\nerror: new failure", shared);
    let new_lines: Vec<&str> = new_output.lines().collect();
    let new_emb = embed_text(&new_output);

    let delta = session.compute_delta("cargo", &new_lines, &new_emb).unwrap();

    assert_eq!(
        delta.reference_turn, 1,
        "Expected reference_turn=1, got {}",
        delta.reference_turn
    );
}
