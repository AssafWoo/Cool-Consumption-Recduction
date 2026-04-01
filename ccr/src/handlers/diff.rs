use super::Handler;

const MAX_HUNKS: usize = 5;
const MAX_CONTEXT_PER_HUNK: usize = 2;

pub struct DiffHandler;

impl Handler for DiffHandler {
    fn filter(&self, output: &str, _args: &[String]) -> String {
        if crate::handlers::util::mid_git_operation() {
            return output.to_string();
        }
        let mut out: Vec<String> = Vec::new();
        let mut hunk_count: usize = 0;
        let mut in_hunk = false;
        let mut context_in_hunk: usize = 0;
        let mut remaining_hunks: usize = 0;

        for line in output.lines() {
            // Skip index lines (git object hashes — pure noise)
            if line.starts_with("index ") {
                continue;
            }

            // Skip "\ No newline at end of file" noise
            if line.starts_with("\\ No newline") {
                continue;
            }

            // Hunk header
            if line.starts_with("@@") {
                if hunk_count >= MAX_HUNKS {
                    // Count remaining hunks for the summary message
                    remaining_hunks += 1;
                    continue;
                }
                hunk_count += 1;
                in_hunk = true;
                context_in_hunk = 0;
                out.push(line.to_string());
                continue;
            }

            // If we've already hit the hunk limit, keep counting but don't emit
            if hunk_count >= MAX_HUNKS {
                // Count additional @@ lines handled above; just skip everything else
                continue;
            }

            // File header lines
            if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") {
                in_hunk = false;
                context_in_hunk = 0;
                out.push(line.to_string());
                continue;
            }

            if in_hunk {
                if line.starts_with('+') || line.starts_with('-') {
                    out.push(line.to_string());
                } else {
                    // Context line (space-prefixed or blank inside hunk)
                    if context_in_hunk < MAX_CONTEXT_PER_HUNK {
                        out.push(line.to_string());
                        context_in_hunk += 1;
                    }
                    // else: drop excess context lines
                }
            } else {
                // Outside a hunk: keep file-level metadata lines
                out.push(line.to_string());
            }
        }

        if remaining_hunks > 0 {
            out.push(format!("[+{} more hunks]", remaining_hunks));
        }

        if out.is_empty() {
            output.to_string()
        } else {
            out.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::Handler;

    fn handler() -> DiffHandler {
        DiffHandler
    }

    fn no_args() -> Vec<String> {
        vec![]
    }

    #[test]
    fn passthrough_when_empty() {
        let h = handler();
        let input = "";
        let result = h.filter(input, &no_args());
        assert_eq!(result, input);
    }

    #[test]
    fn passthrough_when_no_diff_lines() {
        // Output with no recognisable diff markers → returned as-is
        let h = handler();
        let input = "some random text\nanother line";
        let result = h.filter(input, &no_args());
        assert_eq!(result, input);
    }

    #[test]
    fn strips_index_lines() {
        let h = handler();
        let input = "diff --git a/foo.rs b/foo.rs\nindex abc123..def456 100644\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,3 +1,3 @@\n-old\n+new";
        let result = h.filter(input, &no_args());
        assert!(!result.contains("index abc123"), "index line should be stripped");
        assert!(result.contains("diff --git"));
        assert!(result.contains("-old"));
        assert!(result.contains("+new"));
    }

    #[test]
    fn strips_no_newline_at_end_of_file() {
        let h = handler();
        let input = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n\\ No newline at end of file";
        let result = h.filter(input, &no_args());
        assert!(!result.contains("\\ No newline"), "no-newline notice should be stripped");
    }

    #[test]
    fn limits_to_five_hunks_and_emits_summary() {
        let h = handler();
        // Build a diff with 8 hunks
        let mut input = String::from("diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n");
        for i in 1..=8usize {
            input.push_str(&format!("@@ -{i},1 +{i},1 @@\n-old{i}\n+new{i}\n"));
        }
        let result = h.filter(input.as_str(), &no_args());
        // Exactly 5 @@ lines should appear
        let hunk_headers = result.lines().filter(|l| l.starts_with("@@")).count();
        assert_eq!(hunk_headers, 5, "should keep exactly 5 hunks");
        // Summary line for the remaining 3 hunks
        assert!(result.contains("[+3 more hunks]"), "should emit remaining hunk count; got:\n{result}");
    }

    #[test]
    fn keeps_up_to_two_context_lines_per_hunk() {
        let h = handler();
        // Hunk with 4 context lines
        let input = "diff --git a/foo.rs b/foo.rs\n--- a/foo.rs\n+++ b/foo.rs\n@@ -1,6 +1,6 @@\n context1\n context2\n context3\n context4\n-old\n+new";
        let result = h.filter(input, &no_args());
        let context_lines: Vec<&str> = result.lines().filter(|l| l.starts_with(' ')).collect();
        assert!(context_lines.len() <= 2, "should keep at most 2 context lines per hunk; got {}", context_lines.len());
        assert!(result.contains("-old"));
        assert!(result.contains("+new"));
    }
}
