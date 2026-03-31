use std::sync::OnceLock;

use super::Handler;

pub struct CargoHandler;

fn re_clippy_rule() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\[(\w+)\]").expect("cargo clippy rule regex"))
}

impl Handler for CargoHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "build" | "check" | "clippy" => {
                // Inject --message-format json unless already present
                if args.iter().any(|a| a.starts_with("--message-format")) {
                    args.to_vec()
                } else {
                    let mut out = args.to_vec();
                    out.push("--message-format".to_string());
                    out.push("json".to_string());
                    out
                }
            }
            _ => args.to_vec(),
        }
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "build" | "check" | "clippy" => filter_build(output),
            "test" | "nextest" => filter_test(output),
            _ => output.to_string(),
        }
    }
}

/// Group clippy warnings by lint rule name (e.g. `[unused_variables]`).
/// Returns formatted lines: `[rule_name ×N]` plus up to 3 example location lines.
/// Only applied when there are 3 or more warnings.
fn group_clippy_warnings(warnings: &[String]) -> Vec<String> {
    if warnings.len() < 3 {
        return warnings.iter().map(|w| format!("  {}", w)).collect();
    }

    // Collect (rule_name, original_warning_line) pairs; ungrouped warnings kept as-is.
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut ungrouped: Vec<String> = Vec::new();

    for w in warnings {
        if let Some(cap) = re_clippy_rule().captures(w) {
            let rule = cap[1].to_string();
            grouped.entry(rule).or_default().push(w.clone());
        } else {
            ungrouped.push(w.clone());
        }
    }

    let mut out: Vec<String> = Vec::new();

    for (rule, lines) in &grouped {
        out.push(format!("[{} \u{d7}{}]", rule, lines.len()));
        for loc in lines.iter().take(3) {
            // Extract location part: text after last `]` or the full line
            let location = re_clippy_rule()
                .find(loc)
                .map(|m: regex::Match| loc[m.end()..].trim())
                .unwrap_or(loc.trim());
            if !location.is_empty() {
                out.push(format!("    {}", location));
            }
        }
    }

    for w in &ungrouped {
        out.push(format!("  {}", w));
    }

    out
}

/// Filter `cargo build/check/clippy --message-format json` output.
/// Keeps only compiler-message (errors + warnings); discards compiler-artifact noise.
fn filter_build(output: &str) -> String {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut success: Option<bool> = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Try JSON parse first
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            match v.get("reason").and_then(|r| r.as_str()) {
                Some("compiler-message") => {
                    if let Some(msg) = v.get("message") {
                        let level = msg.get("level").and_then(|l| l.as_str()).unwrap_or("");
                        let text = msg.get("message").and_then(|m| m.as_str()).unwrap_or("");
                        let location = msg
                            .get("spans")
                            .and_then(|s| s.as_array())
                            .and_then(|s| s.first())
                            .map(|span| {
                                let file =
                                    span.get("file_name").and_then(|f| f.as_str()).unwrap_or("");
                                let line_n =
                                    span.get("line_start").and_then(|l| l.as_u64()).unwrap_or(0);
                                format!(" [{}:{}]", file, line_n)
                            })
                            .unwrap_or_default();

                        match level {
                            "error" | "error[E]" => {
                                errors.push(format!("error: {}{}", text, location));
                            }
                            "warning" => {
                                warnings.push(format!("warning: {}{}", text, location));
                            }
                            _ => {}
                        }
                    }
                }
                Some("build-finished") => {
                    success = v.get("success").and_then(|s| s.as_bool());
                }
                _ => {}
            }
        } else {
            // Non-JSON line (e.g. cargo stderr without JSON flag, or mixed output)
            // Keep error/warning lines
            if trimmed.starts_with("error") || trimmed.starts_with("warning") {
                if trimmed.starts_with("error") {
                    errors.push(trimmed.to_string());
                } else {
                    warnings.push(trimmed.to_string());
                }
            }
        }
    }

    let mut out: Vec<String> = Vec::new();
    out.extend(errors.iter().cloned());
    if !warnings.is_empty() {
        out.push(format!("[{} warnings]", warnings.len()));
        let grouped = group_clippy_warnings(&warnings);
        out.extend(grouped);
    }
    match success {
        Some(true) => {
            if out.is_empty() {
                out.push("Build OK".to_string());
            }
        }
        Some(false) => {
            if errors.is_empty() {
                out.push("Build FAILED".to_string());
            }
        }
        None => {}
    }

    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

/// Filter `cargo test` standard output.
/// Keeps failures, the final summary line, and failure detail sections.
fn filter_test(output: &str) -> String {
    let mut failures: Vec<String> = Vec::new();
    let mut summary: Option<String> = None;
    let mut in_failure_detail = false;
    let mut failure_detail: Vec<String> = Vec::new();
    let mut failure_names: Vec<String> = Vec::new();

    for line in output.lines() {
        // Detect failure test lines: "test some::path ... FAILED"
        if line.trim_start().starts_with("test ") && line.ends_with("FAILED") {
            let name = line
                .trim_start()
                .trim_start_matches("test ")
                .trim_end_matches(" ... FAILED")
                .to_string();
            failure_names.push(name);
        }

        // Final result line
        if line.starts_with("test result:") {
            summary = Some(line.to_string());
        }

        // Failure detail sections
        if line.starts_with("failures:") {
            in_failure_detail = true;
        }
        if in_failure_detail {
            failure_detail.push(line.to_string());
        }
    }

    // If all passed
    if failure_names.is_empty() {
        if let Some(s) = summary {
            // Count from summary line
            return s;
        }
        return output.to_string();
    }

    // Build compact output
    let mut out: Vec<String> = Vec::new();
    for name in &failure_names {
        failures.push(format!("FAILED: {}", name));
    }
    out.extend(failures);

    // Add failure details (truncated)
    if !failure_detail.is_empty() {
        let detail_lines: Vec<&str> = failure_detail
            .iter()
            .map(|s| s.as_str())
            .filter(|l| {
                !l.trim().is_empty()
                    && !l.starts_with("failures:")
                    && !l.starts_with("test result:")
            })
            .take(20)
            .collect();
        out.push(String::new());
        out.extend(detail_lines.iter().map(|l| l.to_string()));
    }

    if let Some(s) = summary {
        out.push(s);
    }

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── group_clippy_warnings ────────────────────────────────────────────────

    #[test]
    fn group_same_rule_five_warnings() {
        let warnings: Vec<String> = (1..=5)
            .map(|i| {
                format!(
                    "warning: unused variable [unused_variables] [src/main.rs:{}]",
                    i
                )
            })
            .collect();
        let result = group_clippy_warnings(&warnings);
        // First line should be the grouped header
        assert!(result[0].contains("unused_variables") && result[0].contains("×5"));
    }

    #[test]
    fn group_different_rules_grouped_separately() {
        let warnings = vec![
            "warning: unused variable `x` [unused_variables] [src/a.rs:1]".to_string(),
            "warning: unused variable `y` [unused_variables] [src/a.rs:2]".to_string(),
            "warning: function is never used: `foo` [dead_code] [src/b.rs:10]".to_string(),
            "warning: function is never used: `bar` [dead_code] [src/b.rs:20]".to_string(),
            "warning: function is never used: `baz` [dead_code] [src/b.rs:30]".to_string(),
        ];
        let result = group_clippy_warnings(&warnings);
        let output = result.join("\n");
        assert!(output.contains("dead_code") && output.contains("×3"));
        assert!(output.contains("unused_variables") && output.contains("×2"));
    }

    #[test]
    fn fewer_than_three_warnings_shown_as_is() {
        let warnings = vec![
            "warning: something [some_lint] [src/a.rs:1]".to_string(),
            "warning: something else [other_lint] [src/b.rs:2]".to_string(),
        ];
        let result = group_clippy_warnings(&warnings);
        // Should just be prefixed with "  " — no grouping header
        assert_eq!(result.len(), 2);
        assert!(result[0].starts_with("  "));
        assert!(result[1].starts_with("  "));
    }
}
