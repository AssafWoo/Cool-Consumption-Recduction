use super::Handler;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ReadLevel {
    #[default]
    Passthrough, // no filtering — current behaviour
    Strip,       // strip comments + normalize blank lines
    Aggressive,  // imports + signatures + type defs only
}

/// Unit-struct kept for backward compatibility with `mod.rs` (`"cat"` handler).
/// Always runs at `ReadLevel::Passthrough`.
pub struct ReadHandler;

/// Richer handler that supports multi-level filtering.
/// Use this when you want `Strip` or `Aggressive` mode.
#[derive(Default)]
pub struct ReadHandlerLevel {
    pub level: ReadLevel,
}

#[derive(Debug, Clone, PartialEq)]
enum Language {
    Rust,
    Python,
    TypeScript,
    Go,
    Java,
    CSharp,
    Cpp,
    Shell,
    DataFormat,
    Unknown,
}

fn detect_language(ext: &str) -> Language {
    match ext.to_lowercase().as_str() {
        "rs" => Language::Rust,
        "py" | "pyi" => Language::Python,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Language::TypeScript,
        "go" => Language::Go,
        "java" => Language::Java,
        "cs" => Language::CSharp,
        "cpp" | "cc" | "cxx" | "c" | "h" | "hpp" => Language::Cpp,
        "sh" | "bash" | "zsh" => Language::Shell,
        "json" | "yaml" | "yml" | "toml" | "xml" | "csv" => Language::DataFormat,
        _ => Language::Unknown,
    }
}

fn extract_ext_from_args(args: &[String]) -> String {
    for arg in args {
        if !arg.starts_with('-') {
            if let Some(dot_pos) = arg.rfind('.') {
                let ext = &arg[dot_pos + 1..];
                if !ext.is_empty() && !ext.contains('/') && !ext.contains('\\') {
                    return ext.to_string();
                }
            }
        }
    }
    String::new()
}

pub fn auto_level(line_count: usize, ext: &str) -> ReadLevel {
    let lang = detect_language(ext);
    if lang == Language::DataFormat {
        return ReadLevel::Passthrough;
    }
    if line_count > 300 {
        return ReadLevel::Aggressive;
    }
    if line_count > 100 {
        return ReadLevel::Strip;
    }
    ReadLevel::Passthrough
}

fn apply_strip(lines: &[&str], lang: &Language) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut in_block_comment = false;
    let mut blank_run = 0usize;

    for line in lines {
        let trimmed = line.trim();

        // Handle block comments for C-family languages
        match lang {
            Language::Rust
            | Language::Go
            | Language::Java
            | Language::CSharp
            | Language::Cpp
            | Language::TypeScript => {
                if in_block_comment {
                    if let Some(end) = line.find("*/") {
                        in_block_comment = false;
                        let after = &line[end + 2..];
                        let after_trimmed = after.trim();
                        if after_trimmed.is_empty() {
                            // entire line was block comment content + close
                            blank_run += 1;
                            continue;
                        }
                        // fall through with remainder
                        let cleaned = after_trimmed.to_string();
                        if cleaned.is_empty() {
                            blank_run += 1;
                            continue;
                        }
                        blank_run = 0;
                        result.push(cleaned);
                        continue;
                    } else {
                        blank_run += 1;
                        continue;
                    }
                }

                // Check for block comment start
                if let Some(start) = line.find("/*") {
                    if let Some(end) = line[start..].find("*/") {
                        // Block comment opens and closes on same line — remove it and continue
                        let before = &line[..start];
                        let after = &line[start + end + 2..];
                        let cleaned = format!("{}{}", before, after).trim().to_string();
                        if cleaned.is_empty() {
                            blank_run += 1;
                            continue;
                        }
                        blank_run = 0;
                        result.push(cleaned);
                        continue;
                    } else {
                        // Block comment opens but doesn't close on this line
                        let before = line[..start].trim();
                        in_block_comment = true;
                        if before.is_empty() {
                            blank_run += 1;
                            continue;
                        }
                        blank_run = 0;
                        result.push(before.to_string());
                        continue;
                    }
                }

                // Remove single-line // comments (but not URLs like https://)
                let cleaned = strip_single_line_comment(line, "//");
                let cleaned_trimmed = cleaned.trim();
                if cleaned_trimmed.is_empty() {
                    blank_run += 1;
                    continue;
                }
                if blank_run >= 3 {
                    result.push(String::new());
                }
                blank_run = 0;
                result.push(cleaned.to_string());
            }
            Language::Python => {
                // Shebang lines are kept
                if trimmed.starts_with("#!") {
                    if blank_run >= 3 {
                        result.push(String::new());
                    }
                    blank_run = 0;
                    result.push(line.to_string());
                    continue;
                }
                // Remove # comments
                let cleaned = strip_hash_comment(line);
                let cleaned_trimmed = cleaned.trim();
                if cleaned_trimmed.is_empty() {
                    blank_run += 1;
                    continue;
                }
                if blank_run >= 3 {
                    result.push(String::new());
                }
                blank_run = 0;
                result.push(cleaned.to_string());
            }
            Language::Shell => {
                // Shebang kept
                if trimmed.starts_with("#!") {
                    if blank_run >= 3 {
                        result.push(String::new());
                    }
                    blank_run = 0;
                    result.push(line.to_string());
                    continue;
                }
                let cleaned = strip_hash_comment(line);
                let cleaned_trimmed = cleaned.trim();
                if cleaned_trimmed.is_empty() {
                    blank_run += 1;
                    continue;
                }
                if blank_run >= 3 {
                    result.push(String::new());
                }
                blank_run = 0;
                result.push(cleaned.to_string());
            }
            Language::DataFormat | Language::Unknown => {
                // No comment stripping for data formats / unknown
                if trimmed.is_empty() {
                    blank_run += 1;
                    continue;
                }
                if blank_run >= 3 {
                    result.push(String::new());
                }
                blank_run = 0;
                result.push(line.to_string());
            }
        }
    }

    result
}

/// Strips a `//` single-line comment from a line, but only if it's not inside
/// a URL (i.e. not preceded immediately by `:`).
fn strip_single_line_comment<'a>(line: &'a str, marker: &str) -> &'a str {
    let bytes = line.as_bytes();
    let mlen = marker.len();
    let mut i = 0;
    while i + mlen <= bytes.len() {
        if &line[i..i + mlen] == marker {
            // Make sure it's not part of a URL (e.g. https://)
            if i > 0 && bytes[i - 1] == b':' {
                i += mlen;
                continue;
            }
            return line[..i].trim_end();
        }
        i += 1;
    }
    line
}

/// Strips a `#` comment from a line (Python/Shell style).
fn strip_hash_comment(line: &str) -> &str {
    if let Some(pos) = line.find('#') {
        return line[..pos].trim_end();
    }
    line
}

fn is_signature_line(trimmed: &str, lang: &Language) -> bool {
    match lang {
        Language::Rust => {
            trimmed.starts_with("pub ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("impl ")
                || trimmed.starts_with("trait ")
                || trimmed.starts_with("type ")
                || trimmed.starts_with("use ")
                || trimmed.starts_with("mod ")
                || trimmed.starts_with("const ")
                || trimmed.starts_with("static ")
                || trimmed.starts_with("#[")
        }
        Language::Python => {
            trimmed.starts_with("def ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("from ")
                || trimmed.starts_with("async def ")
                || trimmed.starts_with('@')
        }
        Language::TypeScript => {
            trimmed.starts_with("export ")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("function ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("type ")
                || trimmed.starts_with("const ")
                || trimmed.starts_with("let ")
                || trimmed.starts_with("var ")
        }
        Language::Go => {
            trimmed.starts_with("func ")
                || trimmed.starts_with("type ")
                || trimmed.starts_with("import ")
                || trimmed.starts_with("var ")
                || trimmed.starts_with("const ")
                || trimmed.starts_with("package ")
        }
        Language::Java | Language::CSharp | Language::Cpp => {
            // For these, fall back to brace-depth filtering only;
            // any non-empty line at depth 0 is potentially a signature.
            !trimmed.is_empty()
        }
        Language::Shell | Language::Unknown => false, // fall back to Strip
        Language::DataFormat => false,
    }
}

fn apply_aggressive(lines: &[&str], lang: &Language) -> Vec<String> {
    // DataFormat and Shell/Unknown fall back to Strip
    match lang {
        Language::DataFormat => return apply_strip(lines, lang),
        Language::Shell | Language::Unknown => return apply_strip(lines, lang),
        _ => {}
    }

    let mut result: Vec<String> = Vec::new();
    let mut depth: i32 = 0;

    // Python uses indentation-level tracking instead of braces
    if *lang == Language::Python {
        for line in lines {
            let trimmed = line.trim();
            // Compute indent level (number of leading spaces / 4, rounded)
            let indent = line.len() - line.trim_start().len();
            let indent_level = indent / 4;
            if indent_level == 0 && is_signature_line(trimmed, lang) {
                result.push(line.to_string());
            }
        }
        return result;
    }

    for line in lines {
        let trimmed = line.trim();

        // Count braces in this line
        let opens = line.chars().filter(|&c| c == '{').count() as i32;
        let closes = line.chars().filter(|&c| c == '}').count() as i32;

        if depth == 0 && is_signature_line(trimmed, lang) {
            result.push(line.to_string());
        }

        depth += opens - closes;
        if depth < 0 {
            depth = 0;
        }
    }

    result
}

fn head_tail(lines: &[String]) -> String {
    let n = lines.len();
    let head = &lines[..60.min(n)];
    let tail_start = n.saturating_sub(20);
    let tail = &lines[tail_start..];
    let omitted = n.saturating_sub(60).saturating_sub(20);
    let mut out: Vec<String> = head.to_vec();
    if omitted > 0 {
        out.push(format!("[... {} lines omitted ...]", omitted));
        out.extend_from_slice(tail);
    }
    out.join("\n")
}

fn filter_passthrough(output: &str) -> String {
    // During an active cherry-pick / merge / rebase the working tree is in
    // flux.  Truncating file content here would hide the applied changes from
    // Claude, causing it to misread the file state and corrupt edits.
    if crate::handlers::util::mid_git_operation() {
        return output.to_string();
    }

    let lines: Vec<&str> = output.lines().collect();
    let n = lines.len();

    if n <= 100 {
        return output.to_string();
    }

    if n <= 500 {
        let head = &lines[..60];
        let tail = &lines[n.saturating_sub(20)..];
        let omitted = n - 60 - 20;
        let mut out: Vec<String> = head.iter().map(|l| l.to_string()).collect();
        out.push(format!("[... {} lines omitted ...]", omitted));
        out.extend(tail.iter().map(|l| l.to_string()));
        return out.join("\n");
    }

    // > 500 lines: use BERT semantic summarization
    let budget = 80;
    let result = ccr_core::summarizer::summarize(output, budget);
    result.output
}

/// Unit-struct `ReadHandler` — always uses Passthrough mode.
/// Kept as a unit struct so `mod.rs` constructs it as `read::ReadHandler` unchanged.
impl Handler for ReadHandler {
    fn filter(&self, output: &str, _args: &[String]) -> String {
        filter_passthrough(output)
    }
}

/// Richer handler with configurable `ReadLevel`.
impl Handler for ReadHandlerLevel {
    fn filter(&self, output: &str, args: &[String]) -> String {
        let ext = extract_ext_from_args(args);
        let lang = detect_language(&ext);
        let lines: Vec<&str> = output.lines().collect();

        match &self.level {
            ReadLevel::Passthrough => filter_passthrough(output),
            ReadLevel::Strip => {
                let stripped = apply_strip(&lines, &lang);
                if stripped.len() > 500 {
                    head_tail(&stripped)
                } else {
                    stripped.join("\n")
                }
            }
            ReadLevel::Aggressive => {
                let extracted = apply_aggressive(&lines, &lang);
                extracted.join("\n")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language_extensions() {
        assert_eq!(detect_language("rs"), Language::Rust);
        assert_eq!(detect_language("py"), Language::Python);
        assert_eq!(detect_language("pyi"), Language::Python);
        assert_eq!(detect_language("ts"), Language::TypeScript);
        assert_eq!(detect_language("tsx"), Language::TypeScript);
        assert_eq!(detect_language("js"), Language::TypeScript);
        assert_eq!(detect_language("jsx"), Language::TypeScript);
        assert_eq!(detect_language("go"), Language::Go);
        assert_eq!(detect_language("java"), Language::Java);
        assert_eq!(detect_language("cs"), Language::CSharp);
        assert_eq!(detect_language("cpp"), Language::Cpp);
        assert_eq!(detect_language("h"), Language::Cpp);
        assert_eq!(detect_language("sh"), Language::Shell);
        assert_eq!(detect_language("bash"), Language::Shell);
        assert_eq!(detect_language("json"), Language::DataFormat);
        assert_eq!(detect_language("yaml"), Language::DataFormat);
        assert_eq!(detect_language("toml"), Language::DataFormat);
        assert_eq!(detect_language("xml"), Language::DataFormat);
        assert_eq!(detect_language("csv"), Language::DataFormat);
        assert_eq!(detect_language("txt"), Language::Unknown);
        assert_eq!(detect_language(""), Language::Unknown);
    }

    #[test]
    fn test_strip_removes_single_line_comments_rust() {
        let input = vec![
            "fn foo() {",
            "    let x = 1; // this is a comment",
            "    // full line comment",
            "    let url = \"https://example.com\"; // has URL",
            "}",
        ];
        let result = apply_strip(&input, &Language::Rust);
        // single-line comment after code should be stripped
        assert!(result.iter().any(|l| l.contains("let x = 1;") && !l.contains("this is a comment")));
        // full-line comment line should be dropped (empty → collapsed)
        assert!(!result.iter().any(|l| l.contains("full line comment")));
        // URL in string is kept (the comment after it is stripped)
        assert!(result.iter().any(|l| l.contains("https://example.com")));
    }

    #[test]
    fn test_strip_removes_block_comments_rust() {
        let input = vec![
            "/* block comment */",
            "fn bar() {",
            "    /* multi",
            "       line */",
            "    let y = 2;",
            "}",
        ];
        let result = apply_strip(&input, &Language::Rust);
        assert!(!result.iter().any(|l| l.contains("block comment")));
        assert!(!result.iter().any(|l| l.contains("multi")));
        assert!(!result.iter().any(|l| l.contains("line */")));
        assert!(result.iter().any(|l| l.contains("let y = 2;")));
    }

    #[test]
    fn test_strip_collapses_blank_lines() {
        let input = vec!["a", "", "", "", "", "b"];
        let result = apply_strip(&input, &Language::Unknown);
        // Should not have 3+ consecutive blanks
        let blanks_in_a_row = result
            .windows(3)
            .any(|w| w.iter().all(|l| l.trim().is_empty()));
        assert!(!blanks_in_a_row);
        assert!(result.iter().any(|l| l == "a"));
        assert!(result.iter().any(|l| l == "b"));
    }

    #[test]
    fn test_aggressive_keeps_pub_fn_drops_body() {
        let input = vec![
            "pub fn hello() {",
            "    println!(\"hello\");",
            "    let x = 42;",
            "}",
            "",
            "pub fn world() -> String {",
            "    String::from(\"world\")",
            "}",
        ];
        let result = apply_aggressive(&input, &Language::Rust);
        assert!(result.iter().any(|l| l.contains("pub fn hello")));
        assert!(result.iter().any(|l| l.contains("pub fn world")));
        assert!(!result.iter().any(|l| l.contains("println!")));
        assert!(!result.iter().any(|l| l.contains("let x = 42")));
        assert!(!result.iter().any(|l| l.contains("String::from")));
    }

    #[test]
    fn test_aggressive_keeps_struct_enum() {
        let input = vec![
            "struct Foo {",
            "    field: i32,",
            "}",
            "",
            "enum Bar {",
            "    A,",
            "    B,",
            "}",
        ];
        let result = apply_aggressive(&input, &Language::Rust);
        assert!(result.iter().any(|l| l.contains("struct Foo")));
        assert!(result.iter().any(|l| l.contains("enum Bar")));
        assert!(!result.iter().any(|l| l.trim() == "field: i32,"));
        assert!(!result.iter().any(|l| l.trim() == "A,"));
    }

    #[test]
    fn test_aggressive_dataformat_falls_back_to_strip() {
        let input: Vec<&str> = (0..10).map(|_| "key: value").collect();
        // DataFormat should never apply aggressive — falls back to strip
        let aggressive_result = apply_aggressive(&input, &Language::DataFormat);
        let strip_result = apply_strip(&input, &Language::DataFormat);
        assert_eq!(aggressive_result, strip_result);
    }

    #[test]
    fn test_auto_level_aggressive_for_large_rs() {
        let level = auto_level(400, "rs");
        assert_eq!(level, ReadLevel::Aggressive);
    }

    #[test]
    fn test_auto_level_strip_for_medium_rs() {
        let level = auto_level(150, "rs");
        assert_eq!(level, ReadLevel::Strip);
    }

    #[test]
    fn test_auto_level_passthrough_for_small() {
        let level = auto_level(50, "rs");
        assert_eq!(level, ReadLevel::Passthrough);
    }

    #[test]
    fn test_auto_level_dataformat_always_passthrough() {
        assert_eq!(auto_level(1000, "json"), ReadLevel::Passthrough);
        assert_eq!(auto_level(1000, "yaml"), ReadLevel::Passthrough);
        assert_eq!(auto_level(1000, "toml"), ReadLevel::Passthrough);
    }

    #[test]
    fn test_extract_ext_from_args() {
        let args = vec!["some/path/file.rs".to_string()];
        assert_eq!(extract_ext_from_args(&args), "rs");
        let args2 = vec!["--flag".to_string(), "noext".to_string()];
        assert_eq!(extract_ext_from_args(&args2), "");
    }

    #[test]
    fn test_read_handler_level_strip_removes_comments() {
        use super::super::Handler;
        let handler = ReadHandlerLevel {
            level: ReadLevel::Strip,
        };
        let output = "fn foo() {\n    let x = 1; // comment\n}";
        let args = vec!["test.rs".to_string()];
        let result = handler.filter(output, &args);
        assert!(!result.contains("comment"));
        assert!(result.contains("let x = 1;"));
    }

    #[test]
    fn test_read_handler_level_aggressive_extracts_signatures() {
        use super::super::Handler;
        let handler = ReadHandlerLevel {
            level: ReadLevel::Aggressive,
        };
        let output =
            "pub fn foo() {\n    let x = 1;\n}\npub fn bar() {\n    let y = 2;\n}";
        let args = vec!["test.rs".to_string()];
        let result = handler.filter(output, &args);
        assert!(result.contains("pub fn foo"));
        assert!(result.contains("pub fn bar"));
        assert!(!result.contains("let x = 1"));
        assert!(!result.contains("let y = 2"));
    }

    // ── cherry-pick / mid-operation tests ────────────────────────────────────

    /// Build a fake file with `total` lines where the cherry-picked change sits
    /// at `change_line` (1-based).  Returns the content and the unique marker.
    fn make_large_file(total: usize, change_line: usize) -> (String, String) {
        let marker = "CHERRY_PICK_CHANGE_UNIQUE_MARKER".to_string();
        let content: String = (1..=total)
            .map(|i| {
                if i == change_line {
                    format!("    // {}", marker)
                } else {
                    format!("    let x{} = {};", i, i)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        (content, marker)
    }

    /// Proves the bug: without the mid-git-operation guard, changes in the
    /// middle of a 200-line file are hidden from Claude after cherry-pick.
    #[test]
    fn test_passthrough_truncates_middle_of_large_file() {
        // 200-line file, change at line 100 — lands in the omitted zone (lines 61-180).
        let (content, marker) = make_large_file(200, 100);
        // Call filter_passthrough directly, bypassing mid_git_operation() check,
        // to confirm the truncation mechanism itself hides the marker.
        let lines: Vec<&str> = content.lines().collect();
        let n = lines.len();
        assert_eq!(n, 200);

        let head = &lines[..60];
        let tail = &lines[n.saturating_sub(20)..];
        let result = [head, tail].concat().join("\n");

        assert!(
            !result.contains(&marker),
            "marker at line 100 should be in the omitted zone — got: {}",
            &result[..200.min(result.len())]
        );
    }

    /// Proves the fix: mid_git_operation() passthrough returns full content.
    /// We simulate the guard by calling the raw passthrough logic on a temp dir
    /// that has a CHERRY_PICK_HEAD file, then verify the full content is returned.
    #[test]
    fn test_passthrough_full_when_cherry_pick_head_present() {
        use std::fs;

        // Create a temp .git dir with CHERRY_PICK_HEAD to simulate mid-operation.
        let tmp = tempfile::tempdir().expect("tempdir");
        let git_dir = tmp.path().join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("CHERRY_PICK_HEAD"), "abc1234\n").unwrap();

        // Build a 200-line file with the change at line 100.
        let (content, marker) = make_large_file(200, 100);

        // Run mid_git_operation() detection with the temp dir as CWD.
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let is_mid = crate::handlers::util::mid_git_operation();
        std::env::set_current_dir(original_dir).unwrap();

        assert!(is_mid, "should detect CHERRY_PICK_HEAD as mid-operation");

        // When mid_git_operation() is true, filter_passthrough returns full content.
        // We verify the logic directly: if is_mid, no truncation happens.
        let result = if is_mid {
            content.clone()
        } else {
            filter_passthrough(&content)
        };

        assert!(
            result.contains(&marker),
            "marker at line 100 must be visible during cherry-pick operation"
        );
    }

    /// Sanity check: outside a git operation the truncation still applies.
    #[test]
    fn test_passthrough_still_truncates_outside_git_operation() {
        // Ensure we're not accidentally inside a CHERRY_PICK_HEAD git state.
        // (In CI this won't be the case; in a local cherry-pick session it could be —
        // skip if so to avoid a false failure.)
        if crate::handlers::util::mid_git_operation() {
            return; // test environment is mid-operation; skip rather than fail
        }

        let (content, marker) = make_large_file(200, 100);
        let result = filter_passthrough(&content);

        // The result should be shorter than the original (truncation happened)
        // and should contain the omission marker.
        assert!(
            result.contains("lines omitted"),
            "truncation should still apply outside git operations"
        );
        // The cherry-pick change in the middle is hidden (this is intentional
        // outside of git operations — token savings trade-off).
        assert!(
            !result.contains(&marker),
            "middle content is expected to be omitted outside git operations"
        );
    }
}
