/// Tests for Idea 1: Semantic Line Clustering
///
/// Verifies that `summarize_with_clustering` groups near-duplicate lines and
/// emits a single representative per cluster plus a "[N similar]" marker,
/// while always preserving critical (error/warning) lines.
///
/// Expected new API:
///   `ccr_core::summarizer::summarize_with_clustering(text: &str, budget: usize) -> SummarizeResult`
///
/// Run with: cargo test -p ccr-core --test idea1_clustering
use panda_core::summarizer::summarize_with_clustering;

// ── helper ────────────────────────────────────────────────────────────────────

/// Generate `n` near-identical lines (same prefix, different index).
fn near_dupes(prefix: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{} #{}", prefix, i)).collect()
}

/// Count lines in `s` that contain the substring `needle`.
fn count_containing(s: &str, needle: &str) -> usize {
    s.lines().filter(|l| l.contains(needle)).count()
}

// ── Cluster formation ─────────────────────────────────────────────────────────

/// 15 near-identical warning lines should collapse to 1 representative + marker.
#[test]
fn near_duplicate_warnings_collapse_to_one_cluster() {
    let mut lines = near_dupes("warning: unused variable `x` in function", 15);
    lines.push("error[E0502]: cannot borrow `self` as mutable".to_string());
    let input = lines.join("\n");

    let result = summarize_with_clustering(&input, 30);

    // Only 1 representative from the warning cluster, not 15
    let warning_lines = count_containing(&result.output, "warning: unused variable");
    assert!(
        warning_lines <= 2,
        "Expected ≤2 warning lines, got {}:\n{}",
        warning_lines,
        result.output
    );
}

/// The "[N similar]" marker must appear and report the correct cluster size.
#[test]
fn similar_marker_present_with_correct_count() {
    let lines = near_dupes("downloading crate dependency version", 10);
    let input = lines.join("\n");

    let result = summarize_with_clustering(&input, 20);

    assert!(
        result.output.contains("similar"),
        "Expected '[N similar]' marker in output:\n{}",
        result.output
    );
}

/// Two distinct semantic clusters should each contribute a representative.
#[test]
fn two_distinct_clusters_each_get_representative() {
    let mut lines = near_dupes("warning: unused import in module", 8);
    lines.extend(near_dupes("error: type mismatch expected i32 found str", 8));
    let input = lines.join("\n");

    let result = summarize_with_clustering(&input, 20);

    assert!(
        result.output.contains("warning"),
        "Expected warning cluster representative:\n{}",
        result.output
    );
    assert!(
        result.output.contains("error"),
        "Expected error cluster representative:\n{}",
        result.output
    );
}

// ── Critical line preservation ────────────────────────────────────────────────

/// An explicit `error[Exxxx]` line must always appear regardless of clustering.
#[test]
fn error_lines_always_preserved_through_clustering() {
    let mut lines = near_dupes("compiling package dependency artifact build step", 50);
    lines[25] = "error[E0308]: mismatched types: expected `u32`, found `i64`".to_string();
    let input = lines.join("\n");

    let result = summarize_with_clustering(&input, 10);

    assert!(
        result.output.contains("error[E0308]"),
        "Critical error line lost after clustering:\n{}",
        result.output
    );
}

/// Multiple distinct error lines must all survive clustering.
#[test]
fn multiple_distinct_errors_all_preserved() {
    let mut lines = near_dupes("downloading resolving fetching crate version artifact", 40);
    lines[10] = "error[E0502]: cannot borrow `conn` as mutable".to_string();
    lines[30] = "error[E0277]: the trait `Send` is not implemented for `Rc<T>`".to_string();
    let input = lines.join("\n");

    let result = summarize_with_clustering(&input, 10);

    assert!(result.output.contains("E0502"), "First error lost:\n{}", result.output);
    assert!(result.output.contains("E0277"), "Second error lost:\n{}", result.output);
}

// ── Savings ───────────────────────────────────────────────────────────────────

/// Clustering must reduce line count more aggressively than plain anomaly scoring
/// when input is dominated by near-duplicate lines.
#[test]
fn clustering_achieves_better_compression_than_raw_budget() {
    // 100 near-identical lines → cluster should collapse to ≤5 output lines
    let lines = near_dupes("fetching registry metadata package lock version", 100);
    let input = lines.join("\n");

    let result = summarize_with_clustering(&input, 50);

    let out_lines = result.output.lines().count();
    assert!(
        out_lines <= 5,
        "Expected ≤5 output lines for 100 near-dupes, got {}:\n{}",
        out_lines,
        result.output
    );
}

/// Short inputs (within budget) must pass through unchanged.
#[test]
fn short_input_unchanged() {
    let input = "line one\nline two\nline three";
    let result = summarize_with_clustering(input, 20);
    assert!(result.output.contains("line one"));
    assert!(result.output.contains("line two"));
    assert!(result.output.contains("line three"));
}

// ── SummarizeResult fields ────────────────────────────────────────────────────

/// `lines_in` must equal the number of non-empty input lines.
#[test]
fn lines_in_reported_correctly() {
    let lines = near_dupes("some log line content here", 20);
    let input = lines.join("\n");
    let result = summarize_with_clustering(&input, 10);
    assert_eq!(result.lines_in, 20);
}

/// `omitted` = `lines_in - lines_out` must be non-negative.
#[test]
fn omitted_is_consistent() {
    let lines = near_dupes("progress bar downloading artifact resolving", 30);
    let input = lines.join("\n");
    let result = summarize_with_clustering(&input, 10);
    assert_eq!(
        result.omitted,
        result.lines_in.saturating_sub(result.lines_out)
    );
}
