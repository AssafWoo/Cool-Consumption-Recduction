/// Tests for Idea 6: Zero-Shot Noise Classification (Prototype Embeddings)
///
/// Verifies that `noise_scores` assigns negative scores to boilerplate/progress
/// lines and positive scores to useful development information, using two static
/// prototype embeddings computed once at model load time.
///
/// Score = cosine_similarity(line, USEFUL_PROTOTYPE) - cosine_similarity(line, NOISE_PROTOTYPE)
/// Positive  → keep (useful development information)
/// Negative  → drop (boilerplate noise)
///
/// Expected new API:
///   `ccr_core::summarizer::noise_scores(lines: &[&str]) -> anyhow::Result<Vec<f32>>`
///
/// Run with: cargo test -p ccr-core --test idea6_noise_classifier
use panda_core::summarizer::noise_scores;

// ── helpers ───────────────────────────────────────────────────────────────────

fn scores_for(lines: &[&str]) -> Vec<f32> {
    noise_scores(lines).expect("noise_scores failed")
}

fn avg(v: &[f32]) -> f32 {
    v.iter().sum::<f32>() / v.len() as f32
}

// ── Noise lines score negative ────────────────────────────────────────────────

/// Classic package-manager progress lines must score clearly negative.
#[test]
fn package_manager_progress_scores_negative() {
    let noise_lines = [
        "downloading crate 1/200",
        "downloading crate 2/200",
        "downloading crate 3/200",
        "resolving dependencies...",
        "fetching registry index",
        "updating crate.io-index",
        "already up to date",
    ];

    let s = scores_for(&noise_lines);
    let avg_score = avg(&s);

    assert!(
        avg_score < 0.0,
        "Package-manager noise should average negative score, got {:.4}",
        avg_score
    );
}

/// Compilation progress lines are noise (not useful to a developer).
#[test]
fn compilation_progress_scores_negative() {
    let noise_lines = [
        "   Compiling foo v1.0.0",
        "   Compiling bar v2.3.1",
        "   Compiling baz v0.9.0",
        "   Compiling qux v1.1.0",
        "   Compiling quux v3.0.0",
    ];

    let s = scores_for(&noise_lines);
    let avg_score = avg(&s);

    assert!(
        avg_score < 0.05,
        "Compilation progress lines should score ≤0.05, got {:.4}",
        avg_score
    );
}

// ── Useful lines score positive ───────────────────────────────────────────────

/// Error lines must score clearly positive.
#[test]
fn error_lines_score_positive() {
    let useful_lines = [
        "error[E0308]: mismatched types: expected `u32`, found `str`",
        "error[E0502]: cannot borrow `self` as mutable because it is borrowed as immutable",
        "error[E0277]: the trait `Send` is not implemented for `Rc<T>`",
    ];

    let s = scores_for(&useful_lines);
    let avg_score = avg(&s);

    assert!(
        avg_score > 0.0,
        "Error lines should score positive, got {:.4}",
        avg_score
    );
}

/// Warning lines must score positive (developer-relevant information).
#[test]
fn warning_lines_score_positive() {
    let useful_lines = [
        "warning: unused variable `connection` in function `setup_pool`",
        "warning: dead code: function `validate_token` is never called",
        "warning: deprecated: use `connect_timeout` instead of `timeout`",
    ];

    let s = scores_for(&useful_lines);
    let avg_score = avg(&s);

    assert!(
        avg_score > 0.0,
        "Warning lines should score positive, got {:.4}",
        avg_score
    );
}

/// Test failure lines must score positive.
#[test]
fn test_failure_lines_score_positive() {
    let useful_lines = [
        "FAILED tests::auth::token_should_expire_after_24h",
        "thread 'tests::db::connection_pool_exhausted' panicked at assertion failed",
        "test result: FAILED. 3 passed; 2 failed; 0 ignored",
    ];

    let s = scores_for(&useful_lines);
    let avg_score = avg(&s);

    assert!(
        avg_score > 0.0,
        "Test failure lines should score positive, got {:.4}",
        avg_score
    );
}

// ── Ordering: useful > noise ──────────────────────────────────────────────────

/// Average score of useful lines must exceed average score of noise lines.
#[test]
fn useful_scores_higher_than_noise_scores() {
    let useful_lines = [
        "error[E0308]: mismatched types in function return",
        "warning: unused import `std::collections::HashMap`",
        "FAILED: test_auth_token_expiry — expected Ok got Err(Expired)",
        "  --> src/handlers/auth.rs:45:12",
        "note: expected type `u32`, found type `i64`",
    ];
    let noise_lines = [
        "   Compiling foo v1.0.0",
        "   Compiling bar v2.0.0",
        "downloading crate 1/100",
        "fetching registry metadata",
        "resolving package dependencies",
    ];

    let useful_scores = scores_for(&useful_lines);
    let noise_scores_v = scores_for(&noise_lines);

    let avg_useful = avg(&useful_scores);
    let avg_noise = avg(&noise_scores_v);

    assert!(
        avg_useful > avg_noise,
        "Useful lines (avg {:.4}) should score higher than noise (avg {:.4})",
        avg_useful,
        avg_noise
    );
}

/// Every individual error line must score higher than every compilation progress line.
#[test]
fn each_error_outscores_each_progress_line() {
    let errors = [
        "error[E0502]: cannot borrow `pool` as mutable",
        "error[E0716]: temporary value dropped while borrowed",
    ];
    let progress = [
        "   Compiling registry-index v0.1.0",
        "downloading package metadata 1/50",
    ];

    let error_scores = scores_for(&errors);
    let progress_scores = scores_for(&progress);

    for (i, es) in error_scores.iter().enumerate() {
        for (j, ps) in progress_scores.iter().enumerate() {
            assert!(
                es > ps,
                "Error line [{}] score {:.4} should exceed progress line [{}] score {:.4}",
                i, es, j, ps
            );
        }
    }
}

// ── Output count & shape ──────────────────────────────────────────────────────

/// `noise_scores` must return exactly as many scores as input lines.
#[test]
fn score_count_matches_line_count() {
    let lines = [
        "error: build failed",
        "   Compiling foo v1.0",
        "downloading crate 1/5",
        "warning: unused variable",
        "Finished dev [unoptimized] in 3.2s",
    ];
    let s = scores_for(&lines);
    assert_eq!(s.len(), lines.len(), "Score count must equal line count");
}

/// Empty input must return an empty score vector.
#[test]
fn empty_input_returns_empty_scores() {
    let s = noise_scores(&[]).expect("noise_scores failed on empty input");
    assert!(s.is_empty(), "Expected empty score vector for empty input");
}

/// Single line must return exactly one score.
#[test]
fn single_line_returns_one_score() {
    let s = scores_for(&["error: something went wrong"]);
    assert_eq!(s.len(), 1);
}
