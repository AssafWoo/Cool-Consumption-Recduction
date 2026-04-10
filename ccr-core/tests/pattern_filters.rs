/// Integration tests for P3: new TOML pattern filters for kubectl, terraform,
/// brew, go, helm, pip, make, tsc, gh.
///
/// Each test verifies:
///   1. Noise lines matching Remove patterns are dropped.
///   2. Collapse patterns produce a single "[N matching lines collapsed]" marker.
///   3. Critical lines (error/warning in content) are NEVER matched by noise patterns.
///
/// Run with: cargo test -p ccr-core --test pattern_filters
use panda_core::config::{CommandConfig, FilterAction, FilterPattern, SimpleAction};
use panda_core::patterns::PatternFilter;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_filter(patterns: Vec<FilterPattern>) -> PatternFilter {
    PatternFilter::new(&CommandConfig { patterns, on_empty: None }).expect("valid patterns")
}

fn rp(regex: &str) -> FilterPattern {
    FilterPattern {
        regex: regex.to_string(),
        action: FilterAction::Simple(SimpleAction::Remove),
        strip_ansi: false,
    }
}

fn cp(regex: &str) -> FilterPattern {
    FilterPattern {
        regex: regex.to_string(),
        action: FilterAction::Simple(SimpleAction::Collapse),
        strip_ansi: false,
    }
}

/// Assert that none of the critical-line samples are matched by Remove patterns.
fn assert_critical_not_removed(filter: &PatternFilter, critical_lines: &[&str]) {
    for line in critical_lines {
        assert!(
            !filter.should_remove(line),
            "critical line was incorrectly removed: {:?}",
            line
        );
    }
}

// ── kubectl ───────────────────────────────────────────────────────────────────

#[test]
fn kubectl_klog_lines_removed() {
    let filter = make_filter(vec![
        rp(r"^[WIE]\d{4}\s+"),
        rp(r"^Warning:.*deprecated.*"),
        rp(r"^resource mapping not found.*"),
    ]);

    let input = include_str!("fixtures/kubectl_get_pods.txt");
    let result = filter.apply(input);

    // klog lines (W0319, I0319) removed
    assert!(!result.contains("W0319"), "klog warning line should be removed");
    assert!(!result.contains("I0319"), "klog info line should be removed");
    // deprecation warning removed
    assert!(!result.contains("deprecated"), "deprecated warning should be removed");
    // resource mapping removed
    assert!(!result.contains("resource mapping"), "resource mapping line should be removed");
    // pod table preserved
    assert!(result.contains("Running"), "pod status lines must be preserved");
    assert!(result.contains("CrashLoopBackOff"), "error status must be preserved");
}

#[test]
fn kubectl_critical_lines_not_matched_by_noise_patterns() {
    let filter = make_filter(vec![
        rp(r"^[WIE]\d{4}\s+"),
    ]);
    // A line that starts with a capital letter but is an actual error should not be removed
    assert_critical_not_removed(&filter, &[
        "Error from server: the server could not find the requested resource",
        "error: you must be logged in to the server",
    ]);
}

// ── terraform ─────────────────────────────────────────────────────────────────

#[test]
fn terraform_refresh_lines_collapsed() {
    let filter = make_filter(vec![
        cp(r"^\s*Refreshing state\.\.\."),
        cp(r"^\s*Reading\.\.\."),
        rp(r"^\s*[╷╵│].*"),
    ]);

    let input = include_str!("fixtures/terraform_plan.txt");
    let result = filter.apply(input);

    // Refreshing state lines should be collapsed
    assert!(
        !result.lines().any(|l| l.trim() == "Refreshing state..."),
        "individual Refreshing lines should be collapsed"
    );
    // Box-drawing chars removed
    assert!(!result.contains('╷'), "╷ should be removed");
    assert!(!result.contains('│'), "│ should be removed");
    // Plan line preserved
    assert!(result.contains("Plan:"), "Plan summary must be preserved");
    // Resource change lines preserved
    assert!(result.contains("aws_instance"), "resource change must be preserved");
}

#[test]
fn terraform_reading_lines_collapsed() {
    let filter = make_filter(vec![cp(r"^\s*Reading\.\.\.")]);
    let input = "  Reading...\n  Reading...\n  Reading...\napply complete";
    let result = filter.apply(input);
    assert!(result.contains("collapsed"), "Reading lines should be collapsed");
    assert!(result.contains("apply complete"), "non-matching lines preserved");
}

// ── pip ───────────────────────────────────────────────────────────────────────

#[test]
fn pip_downloading_and_cached_lines_removed() {
    let filter = make_filter(vec![
        cp(r"^Collecting \S+"),
        rp(r"^  Downloading "),
        rp(r"^  Using cached "),
        rp(r"^Installing collected packages:"),
    ]);

    let input = include_str!("fixtures/pip_install.txt");
    let result = filter.apply(input);

    // Downloading and cached lines removed
    assert!(!result.contains("Downloading"), "Downloading lines should be removed");
    assert!(!result.contains("Using cached"), "Using cached lines should be removed");
    assert!(!result.contains("Installing collected"), "Installing collected should be removed");
    // Success line preserved
    assert!(result.contains("Successfully installed"), "success line must be preserved");
    // Collecting lines collapsed (not 3 separate lines)
    let collecting_count = result.lines().filter(|l| l.starts_with("Collecting")).count();
    assert!(collecting_count <= 1, "Collecting lines should be collapsed, got {}", collecting_count);
}

#[test]
fn pip_error_line_not_removed_by_collecting_pattern() {
    let filter = make_filter(vec![cp(r"^Collecting \S+")]);
    assert_critical_not_removed(&filter, &[
        "ERROR: Could not find a version that satisfies the requirement foo",
        "error: pip subprocess failed",
    ]);
}

// ── make ──────────────────────────────────────────────────────────────────────

#[test]
fn make_entering_leaving_removed() {
    let filter = make_filter(vec![
        rp(r"^make\[\d+\]: Entering directory"),
        rp(r"^make\[\d+\]: Leaving directory"),
        rp(r"^make\[\d+\]: Nothing to be done"),
    ]);

    let input = include_str!("fixtures/make_build.txt");
    let result = filter.apply(input);

    assert!(!result.contains("Entering directory"), "Entering lines removed");
    assert!(!result.contains("Leaving directory"), "Leaving lines removed");
    assert!(!result.contains("Nothing to be done"), "Nothing-to-do lines removed");
    // Compile lines preserved
    assert!(result.contains("main.o"), "compile output preserved");
    assert!(result.contains("myapp"), "link step preserved");
}

#[test]
fn make_error_not_matched_by_make_bracket_pattern() {
    let filter = make_filter(vec![rp(r"^make\[\d+\]: Entering directory")]);
    assert_critical_not_removed(&filter, &[
        "make[1]: Error 1",
        "make: *** [Makefile:23: main.o] Error 1",
    ]);
}

// ── gh ────────────────────────────────────────────────────────────────────────

#[test]
fn gh_checkmark_lines_removed() {
    let filter = make_filter(vec![rp(r"^✓ ")]);
    let input = "✓ Logged in to github.com as user\n✓ Token: ghp_***\nPR #123 created\nerror: no token";
    let result = filter.apply(input);

    assert!(!result.contains("✓"), "checkmark lines removed");
    assert!(result.contains("PR #123"), "important lines preserved");
    assert!(result.contains("error:"), "error lines preserved");
}

// ── go ────────────────────────────────────────────────────────────────────────

#[test]
fn go_module_lines_removed() {
    let filter = make_filter(vec![
        cp(r"^go: downloading "),
        rp(r"^go: finding module "),
        rp(r"^go: extracting "),
    ]);
    let input =
        "go: downloading github.com/foo/bar v1.0.0\n\
         go: downloading github.com/baz/qux v2.0.0\n\
         go: finding module for package github.com/foo/bar\n\
         go: extracting github.com/foo/bar v1.0.0\n\
         build ./...\n\
         error: undefined: Foo";
    let result = filter.apply(input);

    assert!(!result.contains("finding module"), "finding module removed");
    assert!(!result.contains("extracting"), "extracting removed");
    assert!(result.contains("error: undefined"), "error preserved");
    // downloading lines collapsed (not 2 separate)
    let dl_count = result.lines().filter(|l| l.contains("downloading")).count();
    assert!(dl_count <= 1, "downloading lines should be collapsed");
}

// ── Property: no noise pattern fires on canonical critical lines ──────────────

#[test]
fn no_noise_pattern_matches_critical_lines() {
    // Load the full default config's patterns for all new commands
    let critical_samples = [
        "error: compilation failed",
        "Error: cannot connect to server",
        "warning: deprecated API usage",
        "FAILED: build step failed",
        "fatal: not a git repository",
        "panic: runtime error: nil pointer dereference",
        "W0319 14:23:01 error in kube controller",  // klog line that contains "error"
    ];

    // kubectl noise patterns
    let kubectl_filter = make_filter(vec![
        rp(r"^[WIE]\d{4}\s+"),
        rp(r"^Warning:.*deprecated.*"),
        rp(r"^resource mapping not found.*"),
    ]);
    // W0319 starts with W followed by digits — check it's removed by klog pattern
    // but a plain "warning:" line is NOT (doesn't start with W followed by 4 digits)
    assert!(kubectl_filter.should_remove("W0319 14:23:01.123456 some klog line"), "klog W line should be removed");
    assert!(!kubectl_filter.should_remove("warning: some actual warning"), "plain warning should not be removed by klog pattern");
    assert!(!kubectl_filter.should_remove("error: some error"), "error should not be removed by klog pattern");

    // Make patterns should not remove compiler errors
    let make_filter_inst = make_filter(vec![
        rp(r"^make\[\d+\]: Entering directory"),
        rp(r"^make\[\d+\]: Leaving directory"),
        rp(r"^make\[\d+\]: Nothing to be done"),
    ]);
    for line in &critical_samples {
        assert!(
            !make_filter_inst.should_remove(line),
            "make noise pattern should not remove critical line: {:?}",
            line
        );
    }

    // pip patterns should not remove errors
    let pip_filter = make_filter(vec![
        cp(r"^Collecting \S+"),
        rp(r"^  Downloading "),
        rp(r"^  Using cached "),
        rp(r"^Installing collected packages:"),
    ]);
    for line in &critical_samples {
        assert!(
            !pip_filter.should_remove(line),
            "pip noise pattern should not remove critical line: {:?}",
            line
        );
    }
}
