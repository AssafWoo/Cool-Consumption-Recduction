use super::Handler;

pub struct GitHandler;

const PUSH_PULL_ERROR_TERMS: &[&str] = &["error:", "rejected", "conflict", "denied", "fatal:"];

impl Handler for GitHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "log" => {
                if !args.iter().any(|a| a == "--oneline") {
                    let mut out = args.to_vec();
                    out.insert(2, "--oneline".to_string());
                    return out;
                }
            }
            "status" => {
                if !args.iter().any(|a| a == "--porcelain" || a == "--short" || a == "-s") {
                    let mut out = args.to_vec();
                    out.insert(2, "--porcelain".to_string());
                    return out;
                }
            }
            _ => {}
        }
        args.to_vec()
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "status" => filter_status(output),
            "log" => filter_log(output),
            "diff" => filter_diff(output),
            "push" | "pull" | "fetch" => filter_push_pull(output),
            "commit" | "add" => filter_commit(output),
            "branch" | "stash" => filter_list(output),
            _ => output.to_string(),
        }
    }
}

// ─── status ──────────────────────────────────────────────────────────────────

fn filter_status(output: &str) -> String {
    if output.contains("nothing to commit") || output.trim().is_empty() {
        return "nothing to commit, working tree clean".to_string();
    }

    let mut staged: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    let mut untracked: Vec<String> = Vec::new();

    for line in output.lines() {
        if line.trim().is_empty()
            || line.trim().starts_with("(use \"git")
            || line.trim().starts_with("no changes added")
        {
            continue;
        }
        if line.len() < 2 {
            continue;
        }

        let x = line.chars().next().unwrap_or(' ');
        let y = line.chars().nth(1).unwrap_or(' ');

        if x == '?' && y == '?' {
            let name = line.get(3..).unwrap_or("").trim().to_string();
            if !name.is_empty() {
                untracked.push(name);
            }
            continue;
        }

        let rest = line.get(3..).unwrap_or("").trim().to_string();
        if rest.is_empty() {
            continue;
        }
        if x != ' ' && x != '#' {
            staged.push(rest.clone());
        }
        if y != ' ' && y != '#' {
            modified.push(rest);
        }
    }

    if staged.is_empty() && modified.is_empty() && untracked.is_empty() {
        return "nothing to commit, working tree clean".to_string();
    }

    let mut out: Vec<String> = Vec::new();

    out.push(format!(
        "Staged: {} · Modified: {} · Untracked: {}",
        staged.len(),
        modified.len(),
        untracked.len()
    ));

    const MAX_STAGED_MODIFIED: usize = 15;
    let sm_combined: Vec<&String> = staged.iter().chain(modified.iter()).collect();
    let sm_shown = MAX_STAGED_MODIFIED.min(sm_combined.len());
    for entry in &sm_combined[..sm_shown] {
        out.push(format!("  {}", entry));
    }
    let sm_extra = sm_combined.len().saturating_sub(sm_shown);
    if sm_extra > 0 {
        out.push(format!("[+{} more staged/modified]", sm_extra));
    }

    const MAX_UNTRACKED: usize = 10;
    let ut_shown = MAX_UNTRACKED.min(untracked.len());
    for entry in &untracked[..ut_shown] {
        out.push(format!("  {}", entry));
    }
    let ut_extra = untracked.len().saturating_sub(ut_shown);
    if ut_extra > 0 {
        out.push(format!("[+{} more untracked]", ut_extra));
    }

    out.join("\n")
}

// ─── log ─────────────────────────────────────────────────────────────────────

/// Trailer prefixes stripped from one-line commit subjects.
const TRAILERS: &[&str] = &[
    "Signed-off-by:", "Co-authored-by:", "Change-Id:", "Reviewed-by:",
    "Acked-by:", "Tested-by:", "Reported-by:", "Cc:",
];

fn filter_log(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let msg = l.splitn(2, ' ').nth(1).unwrap_or("");
            !TRAILERS.iter().any(|t| msg.trim_start().starts_with(t))
        })
        .take(50)
        .collect();

    let total = output.lines().count();
    let mut result: Vec<String> = lines
        .iter()
        .map(|l| {
            let chars: Vec<char> = l.chars().collect();
            if chars.len() > 100 {
                format!("{}…", chars[..99].iter().collect::<String>())
            } else {
                l.to_string()
            }
        })
        .collect();

    if total > 50 {
        result.push(format!("[+{} more commits]", total - 50));
    }
    result.join("\n")
}

// ─── diff ────────────────────────────────────────────────────────────────────

/// Hard cap per hunk and across the whole diff.
const HUNK_LINE_CAP: usize = 20;
/// Total line budget for the whole diff output.
/// Kept below BERT_MIN_LINES (≈15 tokens) for typical small diffs so BERT is skipped.
/// Large diffs still trigger BERT but with a much smaller input.
const DIFF_TOTAL_CAP: usize = 60;
/// Maximum context lines kept on each side of a changed block.
const MAX_CONTEXT_PER_SIDE: usize = 2;

fn filter_diff(output: &str) -> String {
    if crate::handlers::util::mid_git_operation() {
        return output.to_string();
    }
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<String> = Vec::new();

    // Per-file change tally (flushed when a new file starts or at end)
    let mut file_header_idx: Option<usize> = None;
    let mut file_added: usize = 0;
    let mut file_removed: usize = 0;

    let mut hunk_lines: usize = 0;
    let mut hunk_truncated = false;
    let mut total_lines: usize = 0;
    let mut global_truncated = false;

    // Context trimming state: buffer context lines; flush only the last
    // MAX_CONTEXT_PER_SIDE when the next changed line arrives.
    let mut ctx_after: usize = 0;   // context lines already emitted after last change
    let mut ctx_pending: Vec<String> = Vec::new(); // suppressed context awaiting next change

    // Flush the per-file tally into the file header line.
    macro_rules! flush_file_tally {
        () => {
            if let Some(idx) = file_header_idx {
                if file_added > 0 || file_removed > 0 {
                    out[idx] = format!("{} [+{} -{}]", out[idx], file_added, file_removed);
                }
                file_header_idx = None;
                file_added = 0;
                file_removed = 0;
            }
        };
    }

    for line in &lines {
        if global_truncated {
            continue;
        }

        if line.starts_with("diff --git ") {
            // Flush pending context and file tally before starting new file
            ctx_pending.clear();
            ctx_after = 0;
            flush_file_tally!();

            let fname = line
                .split_whitespace()
                .last()
                .and_then(|s| s.strip_prefix("b/"))
                .unwrap_or(line);
            file_header_idx = Some(out.len());
            out.push(fname.to_string());
            total_lines += 1;
            hunk_lines = 0;
            hunk_truncated = false;
            continue;
        }

        // Drop noisy headers
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("index ")
            || line.starts_with("\\ No newline")
        {
            continue;
        }

        // Hunk header: reset per-hunk state but keep context trimming state clean
        if line.starts_with("@@") {
            ctx_pending.clear();
            ctx_after = 0;
            hunk_lines = 0;
            hunk_truncated = false;
            out.push(hunk_context(line));
            total_lines += 1;
            continue;
        }

        // Context lines (' '): buffer after MAX_CONTEXT_PER_SIDE trailing lines
        if line.starts_with(' ') {
            if hunk_truncated {
                continue;
            }
            if ctx_after < MAX_CONTEXT_PER_SIDE {
                out.push(line.to_string());
                hunk_lines += 1;
                total_lines += 1;
                ctx_after += 1;
                if total_lines >= DIFF_TOTAL_CAP {
                    global_truncated = true;
                }
            } else {
                // Suppress but keep the most recent lines for leading context
                ctx_pending.push(line.to_string());
            }
            continue;
        }

        // Changed lines ('+'/'-')
        if line.starts_with('+') || line.starts_with('-') {
            if hunk_truncated {
                if line.starts_with('+') {
                    file_added += 1;
                } else {
                    file_removed += 1;
                }
                continue;
            }

            // Flush up to MAX_CONTEXT_PER_SIDE leading context from pending buffer
            if !ctx_pending.is_empty() {
                let skip = ctx_pending.len().saturating_sub(MAX_CONTEXT_PER_SIDE);
                for ctx_line in ctx_pending.drain(skip..) {
                    if !global_truncated {
                        out.push(ctx_line);
                        hunk_lines += 1;
                        total_lines += 1;
                        if total_lines >= DIFF_TOTAL_CAP {
                            global_truncated = true;
                        }
                    }
                }
                ctx_pending.clear();
            }
            ctx_after = 0;

            if hunk_lines >= HUNK_LINE_CAP {
                hunk_truncated = true;
                out.push("  [...truncated...]".to_string());
                total_lines += 1;
            } else if !global_truncated {
                if line.starts_with('+') {
                    file_added += 1;
                } else {
                    file_removed += 1;
                }
                out.push(line.to_string());
                hunk_lines += 1;
                total_lines += 1;
                if total_lines >= DIFF_TOTAL_CAP {
                    global_truncated = true;
                }
            }
        }
    }

    // Flush final file tally
    flush_file_tally!();

    if global_truncated {
        out.push("[... diff truncated — run `git diff` for full output]".to_string());
    }

    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

/// Extract the human-readable function/class context from a `@@ ... @@ context` line.
fn hunk_context(header: &str) -> String {
    // "@@ -L,N +L,N @@ fn foo() {" → "@@ fn foo() {"
    let parts: Vec<&str> = header.splitn(4, "@@").collect();
    if parts.len() >= 3 {
        let ctx = parts[2].trim();
        if !ctx.is_empty() {
            return format!("@@ {}", ctx);
        }
    }
    "@@".to_string()
}

// ─── push / pull / fetch ─────────────────────────────────────────────────────

fn filter_push_pull(output: &str) -> String {
    let has_error = output.lines().any(|l| {
        let t = l.trim().to_lowercase();
        PUSH_PULL_ERROR_TERMS.iter().any(|e| t.contains(e))
    });

    // Already up to date (only if no errors)
    if !has_error && (output.contains("Everything up-to-date") || output.contains("Already up to date")) {
        return "ok (up to date)".to_string();
    }

    if has_error {
        let lines: Vec<&str> = output.lines().collect();
        let n = lines.len();
        let mut keep = vec![false; n];
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim().to_lowercase();
            if PUSH_PULL_ERROR_TERMS.iter().any(|e| t.contains(e)) {
                let start = i.saturating_sub(2);
                let end = (i + 2).min(n.saturating_sub(1));
                for j in start..=end {
                    keep[j] = true;
                }
            }
        }
        let mut result: Vec<String> = Vec::new();
        let mut last_kept: Option<usize> = None;
        for (i, &k) in keep.iter().enumerate() {
            if k {
                if let Some(last) = last_kept {
                    if last + 1 < i {
                        result.push("...".to_string());
                    }
                }
                result.push(lines[i].to_string());
                last_kept = Some(i);
            }
        }
        return result.join("\n");
    }

    // Success — find the branch ref line: "main -> origin/main" or "branch 'main' set up to track..."
    for line in output.lines() {
        let t = line.trim();
        if t.contains(" -> ") && !t.starts_with("remote:") {
            return format!("ok {}", t);
        }
    }

    // Pull / fetch with file stats
    for line in output.lines() {
        let t = line.trim();
        if t.contains("file") && (t.contains("changed") || t.contains("insertion") || t.contains("deletion")) {
            return format!("ok ({})", t);
        }
    }

    // Fallback: last meaningful line
    output
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| format!("ok {}", l.trim()))
        .unwrap_or_else(|| "ok".to_string())
}

// ─── commit / add ────────────────────────────────────────────────────────────

fn filter_commit(output: &str) -> String {
    let mut bracket_line: Option<String> = None;
    let mut stats_line: Option<String> = None;

    for line in output.lines() {
        let t = line.trim();
        if t.starts_with('[') && bracket_line.is_none() {
            bracket_line = Some(t.to_string());
        }
        if t.contains("file") && (t.contains("changed") || t.contains("insertion") || t.contains("deletion")) {
            stats_line = Some(t.to_string());
        }
    }

    match (bracket_line, stats_line) {
        (Some(b), Some(s)) => format!("ok — {}\n{}", b, s),
        (Some(b), None) => format!("ok — {}", b),
        _ => output.to_string(),
    }
}

// ─── branch / stash ──────────────────────────────────────────────────────────

fn filter_list(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() > 30 {
        let extra = lines.len() - 30;
        let mut out: Vec<String> = lines[..30].iter().map(|l| l.to_string()).collect();
        out.push(format!("[+{} more]", extra));
        out.join("\n")
    } else {
        lines.join("\n")
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_injects_porcelain() {
        let handler = GitHandler;
        let args: Vec<String> = vec!["git".into(), "status".into()];
        let rewritten = handler.rewrite_args(&args);
        assert!(rewritten.contains(&"--porcelain".to_string()), "should inject --porcelain");
    }

    #[test]
    fn test_rewrite_no_double_porcelain() {
        let handler = GitHandler;
        let args: Vec<String> = vec!["git".into(), "status".into(), "--porcelain".into()];
        let rewritten = handler.rewrite_args(&args);
        assert_eq!(rewritten.iter().filter(|a| *a == "--porcelain").count(), 1);
    }

    #[test]
    fn test_status_clean() {
        let output = "On branch main\nnothing to commit, working tree clean\n";
        assert_eq!(filter_status(output), "nothing to commit, working tree clean");
    }

    #[test]
    fn test_status_empty() {
        assert_eq!(filter_status(""), "nothing to commit, working tree clean");
    }

    #[test]
    fn test_status_staged_and_untracked() {
        let output = "M  src/main.rs\nA  src/new.rs\n?? untracked.txt\n?? other.txt\n";
        let result = filter_status(output);
        assert!(result.contains("Staged: 2"), "expected Staged: 2, got: {}", result);
        assert!(result.contains("Untracked: 2"), "expected Untracked: 2, got: {}", result);
        assert!(result.contains("src/main.rs"));
        assert!(result.contains("untracked.txt"));
    }

    #[test]
    fn test_status_modified_unstaged() {
        let output = " M src/lib.rs\n?? foo.txt\n";
        let result = filter_status(output);
        assert!(result.contains("Modified: 1"), "got: {}", result);
        assert!(result.contains("Untracked: 1"), "got: {}", result);
    }

    #[test]
    fn test_status_caps_overflow() {
        let mut output = String::new();
        for i in 0..20 {
            output.push_str(&format!(" M src/file{}.rs\n", i));
        }
        let result = filter_status(&output);
        assert!(result.contains("[+5 more staged/modified]"), "got: {}", result);
    }

    #[test]
    fn test_diff_hunk_cap() {
        let mut input = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,40 +1,40 @@ fn main() {\n".to_string();
        for i in 0..35 {
            input.push_str(&format!("+    line {};\n", i));
        }
        let result = filter_diff(&input);
        assert!(result.contains("[...truncated...]"), "should truncate at 30 lines, got: {}", result);
    }

    #[test]
    fn test_diff_strips_headers() {
        let output = "diff --git a/foo.rs b/foo.rs\nindex abc..def 100644\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,3 @@ fn main() {\n-    old();\n+    new();\n";
        let result = filter_diff(output);
        assert!(!result.contains("index abc"), "index line should be stripped");
        assert!(!result.contains("--- a/"), "--- line should be stripped");
        assert!(!result.contains("+++ b/"), "+++ line should be stripped");
        assert!(result.contains("-    old();"));
        assert!(result.contains("+    new();"));
    }

    #[test]
    fn test_diff_hunk_context_extracted() {
        let output = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -10,5 +10,5 @@ fn main() {\n-    old();\n+    new();\n";
        let result = filter_diff(output);
        assert!(result.contains("@@ fn main()"), "hunk context should be kept, got: {}", result);
    }

    #[test]
    fn test_diff_per_file_tally() {
        let output = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,4 @@\n-old\n+new\n+extra\n context\n";
        let result = filter_diff(output);
        assert!(result.contains("foo.rs"), "filename should appear, got: {}", result);
        assert!(result.contains("+new"), "added line should appear, got: {}", result);
        // Per-file tally is now included in the filename header
        assert!(result.contains("[+2 -1]"), "tally should appear in header, got: {}", result);
    }

    #[test]
    fn test_push_up_to_date() {
        let output = "Everything up-to-date\n";
        assert_eq!(filter_push_pull(output), "ok (up to date)");
    }

    #[test]
    fn test_push_success_one_liner() {
        let output = "remote: Counting objects: 3\nremote: Compressing objects: 100%\n   abc1234..def5678  main -> origin/main\n";
        let result = filter_push_pull(output);
        assert_eq!(result, "ok abc1234..def5678  main -> origin/main");
    }

    #[test]
    fn test_push_error_kept() {
        let output = "Everything up-to-date\nerror: failed to push some refs\n";
        let result = filter_push_pull(output);
        assert_ne!(result, "ok (up to date)");
        assert!(result.contains("error:"));
        // context lines within 2 of the error should also be kept
        assert!(result.contains("Everything up-to-date"));
    }

    #[test]
    fn test_push_error_includes_context_lines() {
        let output = "remote: some preamble\nremote: branch protection rule\nerror: failed to push some refs\nremote: see https://example.com for info\nremote: trailing noise\n";
        let result = filter_push_pull(output);
        assert!(result.contains("error:"), "error line kept");
        // The 2 lines before the error should be included
        assert!(result.contains("branch protection rule"), "context before error kept");
        // The 2 lines after the error should be included
        assert!(result.contains("see https://example.com"), "context after error kept");
    }

    #[test]
    fn test_log_strips_trailers() {
        let output = "abc1234 fix: real commit\ndef5678 Signed-off-by: Bot <bot@ci.com>\n5678abc Co-authored-by: Alice <a@b.com>\n";
        let result = filter_log(output);
        assert!(result.contains("fix: real commit"), "real commit should remain");
        assert!(!result.contains("Signed-off-by"), "trailer commits should be stripped");
        assert!(!result.contains("Co-authored-by"), "trailer commits should be stripped");
    }

    #[test]
    fn test_commit_format() {
        let output = "[main abc1234] Add feature\n 2 files changed, 10 insertions(+), 3 deletions(-)\n";
        let result = filter_commit(output);
        assert!(result.starts_with("ok — [main abc1234]"), "got: {}", result);
        assert!(result.contains("2 files changed"), "got: {}", result);
    }
}
