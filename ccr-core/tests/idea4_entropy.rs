/// Tests for Idea 4: Semantic Entropy → Dynamic Early Budget
///
/// Verifies that `semantic_entropy` correctly measures embedding variance,
/// and that `entropy_adjusted_budget` returns a tight budget for low-entropy
/// (near-uniform) input and a full budget for high-entropy (diverse) input.
///
/// Expected new APIs:
///   `ccr_core::summarizer::semantic_entropy(embeddings: &[Vec<f32>]) -> f32`
///   `ccr_core::summarizer::entropy_adjusted_budget(text: &str, max_budget: usize) -> usize`
///
/// Run with: cargo test -p ccr-core --test idea4_entropy
use panda_core::summarizer::{embed_batch, entropy_adjusted_budget, semantic_entropy};

// ── helper ────────────────────────────────────────────────────────────────────

fn embeddings_for(lines: &[&str]) -> Vec<Vec<f32>> {
    embed_batch(lines).expect("embed_batch failed")
}

fn uniform_lines(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("added package dependency version lock resolved #{}", i))
        .collect()
}

fn diverse_lines() -> Vec<String> {
    vec![
        "error[E0308]: mismatched types expected `u32` found `str`".to_string(),
        "warning: unused variable `connection_pool` in auth handler".to_string(),
        "   --> src/auth/middleware.rs:45:12".to_string(),
        "test result: FAILED. 3 passed; 7 failed; 0 ignored".to_string(),
        "thread 'main' panicked at 'called unwrap() on Err value'".to_string(),
        "note: run with RUST_BACKTRACE=1 for a backtrace".to_string(),
        "cargo:rerun-if-changed=build.rs".to_string(),
        "Finished dev [unoptimized + debuginfo] target(s) in 8.42s".to_string(),
        "   Compiling my-crate v0.1.0 (/workspace/my-crate)".to_string(),
        "FAILED tests::auth::token_expiry — expected Ok, got Err(Expired)".to_string(),
    ]
}

// ── semantic_entropy ──────────────────────────────────────────────────────────

/// Entropy of near-identical lines must be low (all embeddings near centroid).
#[test]
fn low_entropy_for_uniform_lines() {
    let lines: Vec<&str> = ["downloading package v1"; 20].to_vec();
    let embs = embeddings_for(&lines);
    let entropy = semantic_entropy(&embs);

    assert!(
        entropy < 0.20,
        "Expected low entropy (<0.20) for uniform lines, got {:.4}",
        entropy
    );
}

/// Entropy of semantically diverse lines must be high.
#[test]
fn high_entropy_for_diverse_lines() {
    let lines = diverse_lines();
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    let embs = embeddings_for(&refs);
    let entropy = semantic_entropy(&embs);

    assert!(
        entropy > 0.30,
        "Expected high entropy (>0.30) for diverse lines, got {:.4}",
        entropy
    );
}

/// Entropy must be strictly ordered: uniform < mixed < diverse.
#[test]
fn entropy_is_monotonically_ordered() {
    let uniform_texts = vec!["downloading package v1"; 10];
    let mixed_texts = vec![
        "downloading package v1",
        "downloading package v2",
        "error: build failed",
        "warning: unused import",
        "Compiling foo v1.0",
        "   --> src/lib.rs:12",
        "downloading package v3",
        "downloading package v4",
        "note: see error above",
        "Finished in 1.2s",
    ];
    let diverse_owned = diverse_lines();
    let diverse_texts: Vec<&str> = diverse_owned.iter().map(|s| s.as_str()).take(10).collect();

    let e_uniform = semantic_entropy(&embeddings_for(&uniform_texts));
    let e_mixed = semantic_entropy(&embeddings_for(&mixed_texts));
    let e_diverse = semantic_entropy(&embeddings_for(&diverse_texts));

    assert!(
        e_uniform < e_mixed,
        "Expected uniform ({:.4}) < mixed ({:.4})",
        e_uniform, e_mixed
    );
    assert!(
        e_mixed <= e_diverse,
        "Expected mixed ({:.4}) ≤ diverse ({:.4})",
        e_mixed, e_diverse
    );
}

/// Entropy of a single line must be 0 (no variance).
#[test]
fn single_line_entropy_is_zero() {
    let texts = vec!["downloading package v1.0.0"];
    let embs = embeddings_for(&texts);
    let entropy = semantic_entropy(&embs);
    assert!(
        entropy < 1e-6,
        "Single-line entropy should be ~0, got {:.6}",
        entropy
    );
}

/// Entropy of empty input must be 0 (or return 0.0 gracefully).
#[test]
fn empty_embeddings_entropy_is_zero() {
    let entropy = semantic_entropy(&[]);
    assert_eq!(entropy, 0.0, "Empty embeddings should return 0.0 entropy");
}

// ── entropy_adjusted_budget ───────────────────────────────────────────────────

/// 200 near-identical lines → budget must be ≤10% of max (aggressively compressed).
#[test]
fn low_entropy_input_gets_tight_budget() {
    let lines = uniform_lines(200);
    let input = lines.join("\n");
    let max_budget = 50;

    let budget = entropy_adjusted_budget(&input, max_budget);

    assert!(
        budget <= (max_budget / 10).max(5),
        "Expected tight budget (≤{}) for 200 near-identical lines, got {}",
        (max_budget / 10).max(5),
        budget
    );
}

/// 200 diverse lines → budget must equal max (no early compression).
#[test]
fn high_entropy_input_gets_full_budget() {
    // Repeat the diverse set to get 200 lines
    let base = diverse_lines();
    let lines: Vec<String> = (0..20)
        .flat_map(|_| base.clone())
        .enumerate()
        .map(|(i, l)| format!("{} [{}]", l, i)) // make slightly different
        .collect();
    let input = lines.join("\n");
    let max_budget = 50;

    let budget = entropy_adjusted_budget(&input, max_budget);

    assert!(
        budget >= max_budget * 8 / 10,
        "Expected full budget (≥{}) for diverse lines, got {}",
        max_budget * 8 / 10,
        budget
    );
}

/// Budget must never exceed max_budget.
#[test]
fn budget_never_exceeds_max() {
    let lines = diverse_lines();
    let input = lines.join("\n");
    let max_budget = 30;

    let budget = entropy_adjusted_budget(&input, max_budget);

    assert!(
        budget <= max_budget,
        "Budget {} must not exceed max_budget {}",
        budget,
        max_budget
    );
}

/// Budget must always be at least 1 (never returns zero).
#[test]
fn budget_minimum_is_one() {
    let lines = uniform_lines(500);
    let input = lines.join("\n");

    let budget = entropy_adjusted_budget(&input, 10);

    assert!(budget >= 1, "Budget must be ≥1 even for highly uniform input");
}

/// Short input (≤ threshold lines) bypasses entropy check and returns max_budget.
#[test]
fn short_input_returns_max_budget() {
    let input = "line one\nline two\nline three";
    let max_budget = 30;

    let budget = entropy_adjusted_budget(input, max_budget);

    assert_eq!(
        budget, max_budget,
        "Short input should return max_budget unchanged"
    );
}

// ── Savings measurement ───────────────────────────────────────────────────────

/// Confirm that using entropy_adjusted_budget on a package-manager-style output
/// yields fewer tokens than using the raw max_budget.
#[test]
fn entropy_budget_saves_tokens_on_npm_style_output() {
    use panda_core::summarizer::summarize;
    use panda_core::tokens::count_tokens;

    // Simulate `npm install` output: hundreds of nearly identical "added X" lines
    let lines: Vec<String> = (1..=200)
        .map(|i| format!("added {} packages from {} contributors", i, i + 5))
        .collect();
    let input = lines.join("\n");

    let max_budget = 50;
    let entropy_budget = entropy_adjusted_budget(&input, max_budget);
    let full_budget_result = summarize(&input, max_budget);
    let entropy_result = summarize(&input, entropy_budget);

    let full_tokens = count_tokens(&full_budget_result.output);
    let entropy_tokens = count_tokens(&entropy_result.output);

    assert!(
        entropy_tokens < full_tokens,
        "Entropy budget ({} lines → {} tokens) should save over max budget ({} lines → {} tokens)",
        entropy_budget, entropy_tokens, max_budget, full_tokens
    );
}
