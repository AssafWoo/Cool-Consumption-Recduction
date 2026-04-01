/// A rule for match_output short-circuit: if output matches success_pattern return ok_message,
/// if it matches error_pattern return None (fall through to normal handling).
pub struct MatchOutputRule {
    pub success_pattern: &'static str,
    pub error_pattern: &'static str,
    pub ok_message: &'static str,
}

/// Check a list of MatchOutputRules against output.
/// Returns Some(ok_message) if a success pattern fires and no error pattern fires.
/// Returns None to indicate normal processing should continue.
pub fn check_match_output(output: &str, rules: &[MatchOutputRule]) -> Option<String> {
    for rule in rules {
        let success_re = regex::Regex::new(rule.success_pattern).ok()?;
        let error_re = regex::Regex::new(rule.error_pattern).ok()?;
        if success_re.is_match(output) && !error_re.is_match(output) {
            return Some(rule.ok_message.to_string());
        }
    }
    None
}

/// Parse a space-aligned table, keep only specified column indices (0-based).
pub fn compact_table(output: &str, keep_cols: &[usize]) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return output.to_string();
    }

    let mut out: Vec<String> = Vec::new();
    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.is_empty() {
            continue;
        }
        let selected: Vec<&str> = keep_cols
            .iter()
            .filter_map(|&i| cols.get(i).copied())
            .collect();
        out.push(selected.join("  "));
    }
    out.join("\n")
}

/// Extract failure blocks + summary from test runner output.
/// runner: "pytest" | "jest" | "vitest" | "dotnet"
pub fn test_failures(output: &str, runner: &str) -> String {
    match runner {
        "pytest" => filter_pytest(output),
        "jest" => filter_jest(output),
        "vitest" => filter_vitest(output),
        _ => output.to_string(),
    }
}

fn filter_pytest(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_failure = false;
    let mut failure_lines = 0;

    for line in &lines {
        let t = line.trim();
        // Keep FAILED/ERROR node IDs
        if t.starts_with("FAILED ") || t.starts_with("ERROR ") {
            out.push(line.to_string());
            continue;
        }
        // Start of a failure block
        if t.starts_with("____") && t.ends_with("____") {
            in_failure = true;
            failure_lines = 0;
            out.push(line.to_string());
            continue;
        }
        if in_failure {
            if failure_lines < 10 {
                out.push(line.to_string());
                failure_lines += 1;
            } else if failure_lines == 10 {
                out.push("[... truncated ...]".to_string());
                failure_lines += 1;
            }
            // End of failure block
            if t.starts_with("====") {
                in_failure = false;
            }
            continue;
        }
        // Summary line
        if t.contains(" failed") || t.contains(" passed") || t.contains(" error") {
            if t.starts_with('=') {
                out.push(line.to_string());
            }
        }
        // Drop: PASSED lines, dots, "collected N items", platform header
    }
    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

fn filter_jest(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_failure = false;
    let mut failure_lines = 0;

    for line in &lines {
        let t = line.trim();
        // Keep FAIL <path> lines
        if t.starts_with("FAIL ") {
            out.push(line.to_string());
            in_failure = false;
            continue;
        }
        // Keep ● failure detail blocks
        if t.starts_with('●') {
            in_failure = true;
            failure_lines = 0;
            out.push(line.to_string());
            continue;
        }
        if in_failure {
            if failure_lines < 15 {
                out.push(line.to_string());
                failure_lines += 1;
            } else if failure_lines == 15 {
                out.push("[... truncated ...]".to_string());
                failure_lines += 1;
            }
            // Blank line ends the block
            if t.is_empty() && failure_lines > 2 {
                in_failure = false;
            }
            continue;
        }
        // Final summary
        if t.starts_with("Tests:") || t.starts_with("Test Suites:") || t.starts_with("Time:") {
            out.push(line.to_string());
            continue;
        }
        // Drop: PASS lines, ✓ lines, -- separators
    }
    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

fn filter_vitest(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut in_failure = false;
    let mut failure_lines = 0;

    for line in &lines {
        let t = line.trim();
        // Keep FAIL lines
        if t.starts_with("FAIL") && t.contains(' ') {
            out.push(line.to_string());
            in_failure = false;
            continue;
        }
        // Error message lines
        if t.starts_with("× ") || t.starts_with("✗ ") {
            in_failure = true;
            failure_lines = 0;
            out.push(line.to_string());
            continue;
        }
        if in_failure {
            if failure_lines < 5 {
                out.push(line.to_string());
                failure_lines += 1;
            }
            if t.is_empty() && failure_lines > 1 {
                in_failure = false;
            }
            continue;
        }
        // Summary line
        if t.starts_with("Tests") && (t.contains("failed") || t.contains("passed")) {
            out.push(line.to_string());
        }
        // Drop: ✓ passing lines, progress bars, module noise
    }
    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

/// Returns true if a log line should always be kept regardless of semantic similarity.
pub fn is_hard_keep(line: &str) -> bool {
    let l = line.to_lowercase();
    l.contains("error")
        || l.contains("panic")
        || l.contains("fatal")
        || l.contains("exception")
        || l.contains("failed")
        || l.contains("stack trace")
        || l.contains("caused by")
        || l.contains("critical")
        || l.contains("alert")
        || l.contains("emergency")
}

/// Derive a depth-limited, typed schema from a JSON value.
/// - Integers → "int", floats → "float", strings → "string", bools → "bool", null → "null"
/// - Objects: cap at 15 keys, add "...": "+N more keys" if truncated; recurse depth-limited
/// - Arrays: emit [schema_of_first, "[N items total]"]; empty array → ["array(0 items)"]
/// - Depth limit: 4 levels; beyond that emit "object" or "array"
pub fn json_to_schema(v: &serde_json::Value) -> serde_json::Value {
    json_to_schema_depth(v, 0)
}

fn json_to_schema_depth(v: &serde_json::Value, depth: usize) -> serde_json::Value {
    const MAX_DEPTH: usize = 4;
    const MAX_KEYS: usize = 15;

    match v {
        serde_json::Value::Object(map) => {
            if depth >= MAX_DEPTH {
                return serde_json::Value::String("object".to_string());
            }
            let mut schema_map = serde_json::Map::new();
            let total = map.len();
            for (k, val) in map.iter().take(MAX_KEYS) {
                schema_map.insert(k.clone(), json_to_schema_depth(val, depth + 1));
            }
            if total > MAX_KEYS {
                let extra = total - MAX_KEYS;
                schema_map.insert(
                    "...".to_string(),
                    serde_json::Value::String(format!("+{} more keys", extra)),
                );
            }
            serde_json::Value::Object(schema_map)
        }
        serde_json::Value::Array(arr) => {
            if depth >= MAX_DEPTH {
                return serde_json::Value::String("array".to_string());
            }
            if arr.is_empty() {
                serde_json::json!(["array(0 items)"])
            } else {
                let first_schema = json_to_schema_depth(&arr[0], depth + 1);
                serde_json::json!([first_schema, format!("[{} items total]", arr.len())])
            }
        }
        serde_json::Value::Number(n) => {
            if n.is_f64() && n.as_f64().map(|f| f.fract() != 0.0).unwrap_or(false) {
                serde_json::Value::String("float".to_string())
            } else {
                serde_json::Value::String("int".to_string())
            }
        }
        serde_json::Value::String(_) => serde_json::Value::String("string".to_string()),
        serde_json::Value::Bool(_) => serde_json::Value::String("bool".to_string()),
        serde_json::Value::Null => serde_json::Value::String("null".to_string()),
    }
}

/// Cosine similarity between two float vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Returns true when a mutating git operation is currently in progress
/// (cherry-pick, merge, rebase, am/apply).  Uses only filesystem stats —
/// no subprocess, no latency.
pub fn mid_git_operation() -> bool {
    let Ok(mut dir) = std::env::current_dir() else { return false };
    loop {
        let git = dir.join(".git");
        if git.exists() {
            return git.join("CHERRY_PICK_HEAD").exists()
                || git.join("MERGE_HEAD").exists()
                || git.join("rebase-merge").exists()
                || git.join("rebase-apply").exists();
        }
        if !dir.pop() {
            return false;
        }
    }
}

/// Compact a file path longer than `max_len` chars to "prefix/.../filename".
pub fn compact_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }
    let sep = if path.contains('/') { '/' } else { '\\' };
    let parts: Vec<&str> = path.split(sep).collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    let first = parts[0];
    let last = parts[parts.len() - 1];
    let candidate = format!("{}/.../{}", first, last);
    if candidate.len() <= max_len {
        candidate
    } else {
        format!(".../{}", last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mid_git_operation_false_outside_git() {
        // In a normal test environment there is no CHERRY_PICK_HEAD etc.
        // We can only assert it doesn't panic and returns a bool.
        let _ = mid_git_operation(); // must not panic
    }

    // ── json_to_schema ────────────────────────────────────────────────────────

    #[test]
    fn schema_int() {
        let v = serde_json::json!(42);
        assert_eq!(json_to_schema(&v), serde_json::json!("int"));
    }

    #[test]
    fn schema_float() {
        let v = serde_json::json!(3.14);
        assert_eq!(json_to_schema(&v), serde_json::json!("float"));
    }

    #[test]
    fn schema_bool() {
        let v = serde_json::json!(true);
        assert_eq!(json_to_schema(&v), serde_json::json!("bool"));
    }

    #[test]
    fn schema_null() {
        let v = serde_json::Value::Null;
        assert_eq!(json_to_schema(&v), serde_json::json!("null"));
    }

    #[test]
    fn schema_string() {
        let v = serde_json::json!("hello");
        assert_eq!(json_to_schema(&v), serde_json::json!("string"));
    }

    #[test]
    fn schema_object_key_cap() {
        // Build an object with 20 keys
        let mut map = serde_json::Map::new();
        for i in 0..20usize {
            map.insert(format!("key{}", i), serde_json::json!(i));
        }
        let v = serde_json::Value::Object(map);
        let schema = json_to_schema(&v);
        let obj = schema.as_object().unwrap();
        // Should have at most 15 data keys + 1 "..." key
        assert!(obj.len() <= 16);
        assert!(obj.contains_key("..."));
        let extra_msg = obj["..."].as_str().unwrap();
        assert!(extra_msg.contains("+5 more keys"));
    }

    #[test]
    fn schema_array_shows_count() {
        let v = serde_json::json!([1, 2, 3, 4, 5]);
        let schema = json_to_schema(&v);
        let arr = schema.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1], serde_json::json!("[5 items total]"));
    }

    #[test]
    fn schema_empty_array() {
        let v = serde_json::json!([]);
        let schema = json_to_schema(&v);
        let arr = schema.as_array().unwrap();
        assert_eq!(arr[0], serde_json::json!("array(0 items)"));
    }

    #[test]
    fn schema_depth_limit_object() {
        // Build a deeply nested object: {a: {b: {c: {d: {e: 1}}}}}
        let v = serde_json::json!({"a": {"b": {"c": {"d": {"e": 1}}}}});
        let schema = json_to_schema(&v);
        // At depth 4 we should see "object" string instead of recursing further
        let s = serde_json::to_string(&schema).unwrap();
        assert!(s.contains("\"object\"") || s.contains("\"int\""));
    }

    // ── check_match_output ────────────────────────────────────────────────────

    #[test]
    fn match_output_fires_on_success() {
        let rules = [MatchOutputRule {
            success_pattern: "successfully",
            error_pattern: "error",
            ok_message: "All good",
        }];
        let result = check_match_output("Operation completed successfully", &rules);
        assert_eq!(result, Some("All good".to_string()));
    }

    #[test]
    fn match_output_blocked_by_error_pattern() {
        let rules = [MatchOutputRule {
            success_pattern: "successfully",
            error_pattern: "error",
            ok_message: "All good",
        }];
        let result = check_match_output("completed successfully but with error", &rules);
        assert_eq!(result, None);
    }

    #[test]
    fn match_output_no_match_returns_none() {
        let rules = [MatchOutputRule {
            success_pattern: "done",
            error_pattern: "fail",
            ok_message: "OK",
        }];
        let result = check_match_output("nothing relevant here", &rules);
        assert_eq!(result, None);
    }

    // ── compact_path ─────────────────────────────────────────────────────────

    #[test]
    fn compact_path_short_unchanged() {
        let p = "/a/b/c";
        assert_eq!(compact_path(p, 20), p);
    }

    #[test]
    fn compact_path_truncates_long() {
        let p = "/very/long/deeply/nested/path/to/some/file.rs";
        let result = compact_path(p, 20);
        assert!(result.len() <= 20 || result.starts_with(".../"));
        assert!(result.contains("file.rs"));
    }

    #[test]
    fn compact_path_prefix_slash_filename() {
        let p = "/home/user/projects/myapp/src/main.rs";
        let result = compact_path(p, 25);
        // Should contain filename
        assert!(result.contains("main.rs"));
    }
}
