/// Tests for Idea 5: Contextual Anchoring Around Anomalies
///
/// Verifies that `summarize_with_anchoring` keeps the N semantically nearest
/// neighbors of each high-anomaly line, so that errors are shown in context.
/// Anchors must not blow up the budget or introduce noise.
///
/// Expected new API:
///   `ccr_core::summarizer::summarize_with_anchoring(
///       text: &str, budget: usize, anchor_neighbors: usize
///   ) -> SummarizeResult`
///
/// Run with: cargo test -p ccr-core --test idea5_anchoring
use panda_core::summarizer::summarize_with_anchoring;

// ── helpers ───────────────────────────────────────────────────────────────────

fn has(output: &str, needle: &str) -> bool {
    output.contains(needle)
}

fn noise(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("downloading resolving fetching registry crate artifact #{}", i))
        .collect()
}

// ── Context is kept for anomalies ─────────────────────────────────────────────

/// When an error line is surrounded by a function signature, both must appear.
/// The function signature is the semantic "context" of the error.
#[test]
fn function_signature_kept_alongside_error() {
    let mut lines = noise(80);
    // Plant a function signature + the error it triggers
    lines[38] = "fn process_request(req: &HttpRequest, pool: &DbPool) -> Result<Response>".to_string();
    lines[39] = "error[E0502]: cannot borrow `pool` as mutable because it is also borrowed as immutable".to_string();

    let input = lines.join("\n");
    // anchor_neighbors=2: keep up to 2 semantic neighbors of each anomaly
    let result = summarize_with_anchoring(&input, 15, 2);

    assert!(
        has(&result.output, "error[E0502]"),
        "Error line missing:\n{}",
        result.output
    );
    assert!(
        has(&result.output, "fn process_request"),
        "Function signature (context anchor) missing:\n{}",
        result.output
    );
}

/// The file:line location pointer (e.g. `  --> src/lib.rs:45:12`) is the most
/// common context line for a Rust error — it must be kept alongside the error.
#[test]
fn file_location_pointer_kept_as_anchor() {
    let mut lines = noise(80);
    lines[40] = "error[E0716]: temporary value dropped while borrowed".to_string();
    lines[41] = "  --> src/handlers/auth.rs:102:18".to_string();

    let input = lines.join("\n");
    let result = summarize_with_anchoring(&input, 15, 2);

    assert!(has(&result.output, "error[E0716]"), "Error lost:\n{}", result.output);
    assert!(
        has(&result.output, "src/handlers/auth.rs"),
        "File pointer anchor missing:\n{}",
        result.output
    );
}

/// An error surrounded by multiple context lines keeps the most semantically
/// relevant ones, not arbitrary neighbors.
#[test]
fn most_relevant_context_kept_over_unrelated_noise() {
    let mut lines = noise(80);
    lines[20] = "fn authenticate_user(token: &str, db: &Database) -> Result<User>".to_string();
    // Gap of noise lines
    lines[40] = "error[E0277]: the trait `Serialize` is not implemented for `User`".to_string();
    lines[41] = "  --> src/auth/handler.rs:88:5".to_string();
    // Unrelated noise continues

    let input = lines.join("\n");
    let result = summarize_with_anchoring(&input, 15, 2);

    // Error must be present
    assert!(has(&result.output, "error[E0277]"), "Error lost:\n{}", result.output);
    // File pointer is most immediately relevant
    assert!(
        has(&result.output, "src/auth/handler.rs"),
        "Direct location pointer missing:\n{}",
        result.output
    );
}

// ── No false anchors ──────────────────────────────────────────────────────────

/// An isolated anomaly with no semantic neighbors must not pull in unrelated noise.
#[test]
fn isolated_anomaly_does_not_add_noise_anchors() {
    let mut lines: Vec<String> = (0..80)
        .map(|i| format!("downloading registry fetching metadata artifact crate #{}", i))
        .collect();
    // A completely isolated error with no related lines in the output
    lines[40] = "FATAL: out of memory — allocator returned null pointer at heap boundary".to_string();

    let input = lines.join("\n");
    let result = summarize_with_anchoring(&input, 15, 2);

    // Error must be present
    assert!(has(&result.output, "FATAL: out of memory"), "Error lost:\n{}", result.output);

    // Output must not contain random downloading lines as false anchors
    let downloading_count = result.output.lines()
        .filter(|l| l.contains("downloading registry"))
        .count();

    assert!(
        downloading_count <= 2,
        "Too many noise lines kept as false anchors ({}): \n{}",
        downloading_count,
        result.output
    );
}

// ── Budget is respected ───────────────────────────────────────────────────────

/// Anchoring must not exceed budget * (1 + anchor_neighbors) significantly.
#[test]
fn anchoring_does_not_blow_up_budget() {
    let mut lines = noise(200);
    for i in (10..200).step_by(20) {
        lines[i] = format!("error[E{:04}]: some error in module {}", i, i);
    }

    let input = lines.join("\n");
    let budget = 20;
    let anchor_neighbors = 2;
    let result = summarize_with_anchoring(&input, budget, anchor_neighbors);

    let out_lines = result.output.lines().count();
    // Generous upper bound: budget + (errors × neighbors) + omission markers
    let max_allowed = budget * 3;
    assert!(
        out_lines <= max_allowed,
        "Output ({} lines) significantly exceeds budget {} × 3 = {}:\n{}",
        out_lines, budget, max_allowed, result.output
    );
}

/// anchor_neighbors=0 must produce the same result as plain summarize.
#[test]
fn zero_anchor_neighbors_matches_plain_summarize() {
    use panda_core::summarizer::summarize;

    let mut lines = noise(200);
    lines[100] = "error[E0308]: mismatched types".to_string();
    let input = lines.join("\n");

    let plain = summarize(&input, 30);
    let anchored = summarize_with_anchoring(&input, 30, 0);

    // Same error must be present in both
    assert!(plain.output.contains("error[E0308]"));
    assert!(anchored.output.contains("error[E0308]"));

    // Line counts should be equal (or within 1 for rounding)
    let diff = (plain.lines_out as i64 - anchored.lines_out as i64).abs();
    assert!(diff <= 2, "With 0 anchor_neighbors, output should match plain summarize (±2 lines), diff={}", diff);
}

// ── Short input ───────────────────────────────────────────────────────────────

/// Short input must pass through unchanged regardless of anchor_neighbors.
#[test]
fn short_input_passes_through() {
    let input = "error: build failed\nwarning: unused variable\nfinished in 1.2s";
    let result = summarize_with_anchoring(input, 30, 3);
    assert!(result.output.contains("build failed"));
    assert!(result.output.contains("unused variable"));
}

// ── SummarizeResult correctness ───────────────────────────────────────────────

/// `lines_in` must equal actual non-empty input line count.
#[test]
fn lines_in_is_correct() {
    let lines = noise(100);
    let input = lines.join("\n");
    let result = summarize_with_anchoring(&input, 20, 2);
    assert_eq!(result.lines_in, 100);
}

/// `omitted` + `lines_out` must equal `lines_in`.
#[test]
fn omitted_is_consistent() {
    let lines = noise(150);
    let input = lines.join("\n");
    let result = summarize_with_anchoring(&input, 20, 2);
    assert_eq!(result.omitted, result.lines_in.saturating_sub(result.lines_out));
}
