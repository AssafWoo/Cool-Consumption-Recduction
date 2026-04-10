use super::Handler;
use std::collections::BTreeMap;

pub struct GrepHandler;

/// Extract the search pattern from grep/rg args.
/// Handles `-e pattern` explicitly; otherwise returns the first non-flag argument.
fn find_pattern_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().peekable();
    // Skip the command name (first arg)
    iter.next();
    while let Some(a) = iter.next() {
        if a == "-e" || a == "--regexp" {
            if let Some(pat) = iter.next() {
                return Some(pat.clone());
            }
        } else if a.starts_with("-e") {
            return Some(a[2..].to_string());
        } else if a.starts_with("--regexp=") {
            return Some(a["--regexp=".len()..].to_string());
        } else if a.starts_with('-') {
            // Flag that takes a value — skip value for known flags
            let flag_with_value = ["-A", "-B", "-C", "-m", "--include", "--exclude",
                                    "--after-context", "--before-context", "--context",
                                    "--max-count", "-f", "--file", "--color", "--colours"];
            if flag_with_value.iter().any(|f| a == f) {
                iter.next(); // skip value
            }
            // Otherwise just a boolean flag, skip it
        } else {
            // First non-flag arg is the pattern
            return Some(a.clone());
        }
    }
    None
}

/// Truncate a long line to `max` chars, centering the window around the first
/// occurrence of `pattern`. Falls back to left-truncation if no match.
fn truncate_centered(line: &str, pattern: Option<&str>, max: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= max {
        return line.to_string();
    }

    // Try to find match position (char index)
    let match_char_idx = pattern.and_then(|pat| {
        let lower_line: String = chars.iter().collect::<String>().to_lowercase();
        let lower_pat = pat.to_lowercase();
        lower_line.find(&lower_pat).map(|byte_pos| {
            // Convert byte offset to char index
            line[..byte_pos].chars().count()
        })
    });

    let match_idx = match match_char_idx {
        Some(idx) => idx,
        None => {
            // Fallback: left truncation
            return format!("{}…", chars[..max - 1].iter().collect::<String>());
        }
    };

    // Center a window of `max` chars around the match midpoint
    let half = max / 2;
    let start = if match_idx > half { match_idx - half } else { 0 };
    let end = (start + max).min(chars.len());
    // Adjust start if end was clamped
    let start = if end == chars.len() && end > max {
        end - max
    } else {
        start
    };

    let window: String = chars[start..end].iter().collect();
    let prefix = if start > 0 { "…" } else { "" };
    let suffix = if end < chars.len() { "…" } else { "" };
    format!("{}{}{}", prefix, window, suffix)
}

impl Handler for GrepHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let mut out = args.to_vec();
        // Ensure consistent "filename:lineno:match" format for the filter's parser
        if !out.iter().any(|a| a == "--no-heading" || a == "--heading") {
            out.push("--no-heading".to_string());
        }
        if !out.iter().any(|a| a == "--with-filename" || a == "-H" || a == "--no-filename" || a == "-h") {
            out.push("--with-filename".to_string());
        }
        out
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        // Detect if output uses "filename:lineno:match" format (grep -n or rg default)
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();

        if lines.is_empty() {
            return output.to_string();
        }

        let pattern = find_pattern_arg(args);

        // Try to group by filename
        let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut ungrouped: Vec<String> = Vec::new();
        let mut total_matches = 0;

        for line in &lines {
            if let Some((file, rest)) = split_grep_line(line) {
                let entry = by_file.entry(file).or_default();
                let truncated = truncate_centered(rest, pattern.as_deref(), 120);
                entry.push(truncated);
                total_matches += 1;
            } else {
                // Could be a filename header (rg --heading) or match without file
                ungrouped.push(truncate_centered(line, pattern.as_deref(), 120));
                total_matches += 1;
            }
        }

        if by_file.is_empty() {
            // No file grouping possible
            let shown = 50.min(ungrouped.len());
            let extra = ungrouped.len().saturating_sub(50);
            let mut out: Vec<String> = ungrouped[..shown].to_vec();
            if extra > 0 {
                out.push(format!("[+{} more matches]", extra));
            }
            return out.join("\n");
        }

        let file_count = by_file.len();
        let mut out: Vec<String> = Vec::new();
        let mut shown = 0;
        const LIMIT: usize = 200;
        const PER_FILE_LIMIT: usize = 100;

        'outer: for (file, matches) in &by_file {
            out.push(format!("{}:", compact_path(&file)));
            let file_shown = PER_FILE_LIMIT.min(matches.len());
            let file_extra = matches.len().saturating_sub(PER_FILE_LIMIT);
            for m in &matches[..file_shown] {
                if shown >= LIMIT {
                    break 'outer;
                }
                out.push(format!("  {}", m));
                shown += 1;
            }
            if file_extra > 0 {
                out.push(format!("  [+{} more in this file]", file_extra));
            }
        }

        if total_matches > LIMIT {
            out.push(format!(
                "[+{} more in {} files]",
                total_matches - shown,
                file_count
            ));
        }

        out.join("\n")
    }
}

fn compact_path(path: &str) -> String {
    if path.len() <= 50 {
        return path.to_string();
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    format!("{}/.../{}", parts[0], parts[parts.len() - 1])
}

/// Attempt to split "file:linenum:content" or "file:content"
fn split_grep_line(line: &str) -> Option<(String, &str)> {
    // Try "filename:N:content" (grep -n) or "filename:content"
    let mut colon_positions = line.match_indices(':');
    if let Some((pos1, _)) = colon_positions.next() {
        let candidate_file = &line[..pos1];
        // If it looks like a path (contains / or . or no spaces)
        if !candidate_file.contains(' ') && !candidate_file.is_empty() {
            let rest = &line[pos1 + 1..];
            // Skip line number if present
            if let Some((pos2, _)) = rest.match_indices(':').next() {
                let maybe_num = &rest[..pos2];
                if maybe_num.chars().all(|c| c.is_ascii_digit()) {
                    return Some((candidate_file.to_string(), &rest[pos2 + 1..]));
                }
            }
            return Some((candidate_file.to_string(), rest));
        }
    }
    None
}

fn truncate_line(line: &str, max: usize) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= max {
        line.to_string()
    } else {
        format!("{}…", chars[..max - 1].iter().collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_path_short_unchanged() {
        let path = "src/main.rs";
        assert_eq!(compact_path(path), path);
    }

    #[test]
    fn test_compact_path_long_compacted() {
        let path = "very/long/deeply/nested/directory/structure/that/exceeds/fifty/characters/file.rs";
        let result = compact_path(path);
        assert!(result.len() <= path.len(), "should be shorter, got: {}", result);
        assert!(result.contains("very/"), "should start with first segment, got: {}", result);
        assert!(result.contains("file.rs"), "should end with last segment, got: {}", result);
        assert!(result.contains("..."), "should contain ellipsis, got: {}", result);
    }

    #[test]
    fn test_compact_path_exactly_50_unchanged() {
        // 50 chars exactly
        let path = "a".repeat(25) + "/" + &"b".repeat(24);
        assert_eq!(path.len(), 50);
        assert_eq!(compact_path(&path), path);
    }

    #[test]
    fn test_per_file_limit_of_25() {
        let handler = GrepHandler;
        // Build output with 110 matches in one file (new limit is 100)
        let lines: Vec<String> = (0..110)
            .map(|i| format!("src/main.rs:{}:match here", i + 1))
            .collect();
        let output = lines.join("\n");
        let result = handler.filter(&output, &[]);
        assert!(
            result.contains("[+10 more in this file]"),
            "expected per-file overflow message, got: {}",
            result
        );
    }

    #[test]
    fn test_per_file_limit_not_triggered_for_small_file() {
        let handler = GrepHandler;
        let lines: Vec<String> = (0..10)
            .map(|i| format!("src/lib.rs:{}:some match", i + 1))
            .collect();
        let output = lines.join("\n");
        let result = handler.filter(&output, &[]);
        assert!(
            !result.contains("more in this file"),
            "should not have overflow message, got: {}",
            result
        );
    }

    #[test]
    fn truncate_centered_match_in_middle_stays_in_window() {
        // Line of 200 chars with "TARGET" at position 150
        let prefix = "a".repeat(150);
        let suffix = "b".repeat(44);
        let line = format!("{}TARGET{}", prefix, suffix);
        assert!(line.len() > 120);
        let result = truncate_centered(&line, Some("TARGET"), 120);
        assert!(result.contains("TARGET"), "match should be visible in window");
        // Result should be at most 120 chars + 2 ellipsis chars
        let result_chars: Vec<char> = result.chars().collect();
        assert!(result_chars.len() <= 122, "truncated line too long: {}", result_chars.len());
    }

    #[test]
    fn truncate_centered_no_match_falls_back_to_left() {
        let long = "x".repeat(200);
        let result = truncate_centered(&long, Some("notfound"), 120);
        // Left truncation: first char should be 'x', ends with '…'
        assert!(result.ends_with('…'), "should end with ellipsis");
        assert!(!result.starts_with('…'), "should not start with ellipsis (left truncation)");
    }

    #[test]
    fn find_pattern_arg_extracts_e_flag() {
        let args: Vec<String> = vec!["grep".into(), "-e".into(), "mypattern".into(), "file.txt".into()];
        let result = find_pattern_arg(&args);
        assert_eq!(result.as_deref(), Some("mypattern"));
    }

    #[test]
    fn find_pattern_arg_first_non_flag() {
        let args: Vec<String> = vec!["grep".into(), "-r".into(), "-n".into(), "searchterm".into()];
        let result = find_pattern_arg(&args);
        assert_eq!(result.as_deref(), Some("searchterm"));
    }

    #[test]
    fn test_long_path_in_output_compacted() {
        let handler = GrepHandler;
        let long_path =
            "very/long/deeply/nested/directory/structure/that/exceeds/fifty/characters/file.rs";
        let output = format!("{}:1:fn main()", long_path);
        let result = handler.filter(&output, &[]);
        // The file header line should have the compacted path
        assert!(result.contains(".../file.rs:"), "expected compacted path in output, got: {}", result);
    }
}
