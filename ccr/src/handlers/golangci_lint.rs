use super::Handler;
use serde_json;

/// Handler for golangci-lint — Go's meta-linter.
/// Groups diagnostics by file; collapses INFO/WARN metadata; shows error count.
pub struct GolangCiLintHandler;

impl Handler for GolangCiLintHandler {
    fn filter(&self, output: &str, _args: &[String]) -> String {
        filter_lint(output)
    }
}

pub fn filter_lint(output: &str) -> String {
    let first = output.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if first.trim_start().starts_with('{') {
        filter_lint_json(output)
    } else {
        filter_lint_text(output)
    }
}

fn filter_lint_json(output: &str) -> String {
    // golangci-lint v2 JSON format: {"Issues":[...],"Report":{}}
    // Try to find and parse the first JSON object in the output
    for line in output.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            let issues = match v.get("Issues").and_then(|i| i.as_array()) {
                Some(arr) => arr,
                None => return "No issues found.".to_string(),
            };

            if issues.is_empty() {
                return "No issues found.".to_string();
            }

            let total = issues.len();
            let shown = total.min(40);

            let mut diagnostics: Vec<String> = Vec::new();
            for issue in &issues[..shown] {
                let text = issue.get("Text").and_then(|t| t.as_str()).unwrap_or("");
                let linter = issue
                    .get("FromLinter")
                    .and_then(|l| l.as_str())
                    .unwrap_or("");
                let (file, line_n, col) = issue
                    .get("Pos")
                    .map(|p| {
                        let f = p
                            .get("Filename")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let ln = p
                            .get("Line")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let c = p
                            .get("Column")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        (f, ln, c)
                    })
                    .unwrap_or(("", 0, 0));
                diagnostics.push(format!(
                    "{}:{}:{}: {} ({})",
                    file, line_n, col, text, linter
                ));
            }

            let grouped = group_by_file(&diagnostics);
            let mut out: Vec<String> = Vec::new();
            for (file, issues) in &grouped {
                out.push(file.clone());
                for issue in issues {
                    out.push(format!("  {}", issue));
                }
            }

            if total > 40 {
                out.push(format!("[+{} more issues]", total - 40));
            }
            out.push(format!("[{} issue(s) found]", total));
            return out.join("\n");
        }
    }

    // Could not parse JSON — fall back to text
    filter_lint_text(output)
}

fn filter_lint_text(output: &str) -> String {
    // golangci-lint output format:
    //   src/handler.go:42:9: ineffectual assignment (ineffassign)
    //   src/main.go:15:2: S1000: use plain channel (gosimple)
    // Also has INFO/WARN/ERR prefix lines from the runner itself.

    let mut diagnostics: Vec<String> = Vec::new();
    let mut linter_errors: Vec<String> = Vec::new();
    let mut total = 0usize;
    let clean;

    for line in output.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // Skip INFO/DEBUG runner lines
        if t.starts_with("INFO") || t.starts_with("DEBU") {
            continue;
        }
        // WARN lines from golangci-lint (configuration warnings etc.)
        if t.starts_with("WARN") {
            if linter_errors.len() < 3 {
                linter_errors.push(t.trim_start_matches("WARN").trim().to_string());
            }
            continue;
        }
        // ERR lines
        if t.starts_with("ERR") || t.starts_with("level=error") {
            linter_errors.push(t.to_string());
            continue;
        }
        // Diagnostic lines: "path/file.go:line:col: message (linter)"
        // Must contain at least one colon and not be a header line
        if looks_like_diagnostic(t) {
            total += 1;
            if total <= 40 {
                diagnostics.push(t.to_string());
            }
            continue;
        }
        // "Run 'golangci-lint ..." hint lines — drop
        if t.starts_with("Run `") || t.starts_with("Run '") {
            continue;
        }
    }

    clean = diagnostics.is_empty() && linter_errors.is_empty();

    if clean {
        return "No issues found.".to_string();
    }

    let mut out: Vec<String> = Vec::new();

    // Group by file for readability
    let grouped = group_by_file(&diagnostics);
    for (file, issues) in &grouped {
        out.push(file.clone());
        for issue in issues {
            out.push(format!("  {}", issue));
        }
    }

    if total > 40 {
        out.push(format!("[+{} more issues]", total - 40));
    }

    out.push(format!("[{} issue(s) found]", total));

    for e in &linter_errors {
        out.push(format!("warn: {}", e));
    }

    out.join("\n")
}

fn looks_like_diagnostic(line: &str) -> bool {
    // "src/foo.go:12:5: some message (linter-name)"
    // Must have at least two colons and the first part should look like a file path
    let parts: Vec<&str> = line.splitn(3, ':').collect();
    if parts.len() < 3 {
        return false;
    }
    let file_part = parts[0];
    // File path: must contain .go or look like a path
    (file_part.ends_with(".go") || file_part.contains('/') || file_part.contains('\\'))
        && parts[1].trim().parse::<u32>().is_ok()
}

fn group_by_file(lines: &[String]) -> Vec<(String, Vec<String>)> {
    let mut map: Vec<(String, Vec<String>)> = Vec::new();

    for line in lines {
        let file = extract_file(line);
        // Remove the file prefix from the issue line for display
        let issue = line
            .splitn(2, ':')
            .nth(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| line.clone());

        if let Some(entry) = map.iter_mut().find(|(f, _)| f == &file) {
            entry.1.push(issue);
        } else {
            map.push((file, vec![issue]));
        }
    }

    map
}

fn extract_file(line: &str) -> String {
    line.splitn(2, ':').next().unwrap_or(line).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::Handler;

    fn args() -> Vec<String> { vec![] }

    #[test]
    fn clean_run_returns_no_issues() {
        let output = "\
INFO [config] Config search paths: [/home/user]
INFO [loader] Go packages loading in PACKAGES mode with GOFLAGS=
";
        let result = GolangCiLintHandler.filter(output, &args());
        assert!(result.contains("No issues") || result == "No issues found.");
    }

    #[test]
    fn diagnostics_grouped_by_file() {
        let output = "\
src/handler.go:42:9: ineffectual assignment to err (ineffassign)
src/handler.go:55:3: error return value not checked (errcheck)
src/main.go:15:2: S1000: use plain channel send or receive (gosimple)
";
        let result = GolangCiLintHandler.filter(output, &args());
        assert!(result.contains("src/handler.go"));
        assert!(result.contains("src/main.go"));
        assert!(result.contains("3 issue(s)") || result.contains("issue(s)"));
    }

    #[test]
    fn info_lines_dropped() {
        let output = "\
INFO [runner] Starting linters...
INFO [runner] Running 10 linters
src/foo.go:1:1: unused variable (deadcode)
";
        let result = GolangCiLintHandler.filter(output, &args());
        assert!(!result.contains("INFO"));
        assert!(result.contains("foo.go") || result.contains("issue"));
    }

    #[test]
    fn looks_like_diagnostic_works() {
        assert!(looks_like_diagnostic("src/handler.go:42:9: some message (linter)"));
        assert!(!looks_like_diagnostic("INFO [runner] some info"));
        assert!(!looks_like_diagnostic("WARN deprecated config"));
    }

    #[test]
    fn json_format_parsed() {
        let output = r#"{"Issues":[{"Text":"ineffectual assignment","FromLinter":"ineffassign","Pos":{"Filename":"src/handler.go","Line":42,"Column":9}}],"Report":{}}"#;
        let result = GolangCiLintHandler.filter(output, &args());
        assert!(result.contains("src/handler.go"), "should contain filename");
        assert!(result.contains("ineffassign"), "should contain linter name");
        assert!(result.contains("1 issue(s)"), "should show issue count");
    }

    #[test]
    fn json_empty_issues_returns_no_issues() {
        let output = r#"{"Issues":[],"Report":{}}"#;
        let result = GolangCiLintHandler.filter(output, &args());
        assert_eq!(result, "No issues found.");
    }

    #[test]
    fn text_format_unchanged_by_dispatcher() {
        let output = "\
src/handler.go:42:9: ineffectual assignment to err (ineffassign)
";
        let result = GolangCiLintHandler.filter(output, &args());
        assert!(result.contains("src/handler.go"));
        assert!(result.contains("ineffassign"));
    }

    #[test]
    fn json_50_issues_shows_overflow() {
        let issues: Vec<String> = (0..50)
            .map(|i| {
                format!(
                    r#"{{"Text":"msg {}","FromLinter":"linter","Pos":{{"Filename":"src/foo.go","Line":{},"Column":1}}}}"#,
                    i, i + 1
                )
            })
            .collect();
        let output = format!(r#"{{"Issues":[{}],"Report":{{}}}}"#, issues.join(","));
        let result = GolangCiLintHandler.filter(&output, &args());
        assert!(result.contains("[+10 more issues]"), "should show overflow, got:\n{}", result);
    }
}
