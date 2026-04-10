use super::Handler;

pub struct KubectlHandler;

impl Handler for KubectlHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        if subcmd == "logs" && !args.iter().any(|a| a.starts_with("--tail")) {
            let mut out = args.to_vec();
            out.push("--tail=200".to_string());
            return out;
        }
        args.to_vec()
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "get" => {
                let resource = args.get(2).map(|s| s.as_str()).unwrap_or("");
                let is_pods = resource == "pods" || resource == "pod";
                let has_all_ns = args.iter().any(|a| a == "--all-namespaces" || a == "-A");
                // pod/name syntax: contains a slash
                let is_specific = resource.contains('/');
                if is_pods && !has_all_ns && !is_specific {
                    filter_get_pods(output)
                } else {
                    filter_get(output)
                }
            }
            "logs" => filter_logs(output),
            "describe" => filter_describe(output),
            "apply" | "delete" | "rollout" => filter_changes(output),
            _ => output.to_string(),
        }
    }
}

const INTERESTING_COLUMNS: &[&str] = &[
    "NAME", "STATUS", "READY", "STATE", "PHASE", "TYPE", "CLUSTER-IP", "EXTERNAL-IP",
];

const MAX_GET_ROWS: usize = 30;

fn filter_get(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return output.to_string();
    }

    // First non-empty line is the header
    let header_idx = match lines.iter().position(|l| !l.trim().is_empty()) {
        Some(i) => i,
        None => return output.to_string(),
    };

    let header = lines[header_idx];

    // Parse column positions from the header by finding where each word starts.
    // kubectl output uses fixed-width columns separated by two or more spaces.
    let col_starts: Vec<usize> = {
        let mut starts = Vec::new();
        let mut in_word = false;
        for (i, c) in header.char_indices() {
            if c != ' ' && !in_word {
                starts.push(i);
                in_word = true;
            } else if c == ' ' {
                in_word = false;
            }
        }
        starts
    };

    if col_starts.is_empty() {
        return output.to_string();
    }

    // Extract column names from header
    let col_names: Vec<String> = col_starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = if i + 1 < col_starts.len() {
                col_starts[i + 1]
            } else {
                header.len()
            };
            // trim trailing spaces from the slice
            let end = end.min(header.len());
            header[start..end].trim().to_uppercase()
        })
        .collect();

    // Determine which column indices to keep.
    // Always keep NAME (index 0).  Keep others that are "interesting".
    let keep_indices: Vec<usize> = {
        let interesting: Vec<usize> = col_names
            .iter()
            .enumerate()
            .filter(|(_, name)| INTERESTING_COLUMNS.contains(&name.as_str()))
            .map(|(i, _)| i)
            .collect();

        if interesting.is_empty() {
            // Fallback: keep first 3 columns
            (0..col_names.len().min(3)).collect()
        } else {
            interesting
        }
    };

    // Helper: extract a cell value for a given column index from a raw line
    let extract_cell = |line: &str, col_idx: usize| -> String {
        let start = col_starts[col_idx];
        if start >= line.len() {
            return String::new();
        }
        let end = if col_idx + 1 < col_starts.len() {
            col_starts[col_idx + 1].min(line.len())
        } else {
            line.len()
        };
        line[start..end].trim().to_string()
    };

    // Build output rows (header + data)
    let mut out: Vec<String> = Vec::new();

    // Header row
    let header_cells: Vec<String> = keep_indices
        .iter()
        .map(|&i| col_names[i].clone())
        .collect();
    out.push(header_cells.join("\t"));

    // Data rows (skip the header line itself)
    let data_lines: Vec<&str> = lines
        .iter()
        .skip(header_idx + 1)
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();

    let total_data = data_lines.len();
    let capped = data_lines.iter().take(MAX_GET_ROWS);

    for line in capped {
        let cells: Vec<String> = keep_indices.iter().map(|&i| extract_cell(line, i)).collect();
        out.push(cells.join("\t"));
    }

    if total_data > MAX_GET_ROWS {
        out.push(format!("[+{} more]", total_data - MAX_GET_ROWS));
    }

    out.join("\n")
}

const HEALTHY_STATUSES: &[&str] = &["Running", "Completed"];

fn filter_get_pods(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return output.to_string();
    }

    // Find header line
    let header_idx = match lines.iter().position(|l| !l.trim().is_empty()) {
        Some(i) => i,
        None => return output.to_string(),
    };

    let header = lines[header_idx];

    // Find STATUS column start by scanning header words
    let status_col_start: Option<usize> = {
        let mut found = None;
        let mut in_word = false;
        let mut word_start = 0;
        let mut buf = String::new();
        for (i, c) in header.char_indices() {
            if c != ' ' && !in_word {
                word_start = i;
                buf.clear();
                in_word = true;
            }
            if in_word {
                if c == ' ' {
                    if buf.eq_ignore_ascii_case("STATUS") {
                        found = Some(word_start);
                        break;
                    }
                    in_word = false;
                } else {
                    buf.push(c);
                }
            }
        }
        // Check final word
        if found.is_none() && in_word && buf.eq_ignore_ascii_case("STATUS") {
            found = Some(word_start);
        }
        found
    };

    let status_col = match status_col_start {
        Some(c) => c,
        None => return filter_get(output),
    };

    let data_lines: Vec<&str> = lines
        .iter()
        .skip(header_idx + 1)
        .filter(|l| !l.trim().is_empty())
        .copied()
        .collect();

    if data_lines.is_empty() {
        return output.to_string();
    }

    // Extract status from a pod line
    let pod_status = |line: &str| -> String {
        if status_col >= line.len() {
            return String::new();
        }
        line[status_col..].split_whitespace().next().unwrap_or("").to_string()
    };

    let mut running = 0usize;
    let mut problem_pods: Vec<&str> = Vec::new();
    let mut status_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for line in &data_lines {
        let status = pod_status(line);
        let healthy = HEALTHY_STATUSES.iter().any(|s| status.starts_with(*s));
        if healthy {
            running += 1;
        } else {
            problem_pods.push(line);
            *status_counts.entry(status).or_insert(0) += 1;
        }
    }

    if problem_pods.is_empty() {
        return format!("[{} pods, all running]", data_lines.len());
    }

    // Emit header + problem pods + summary
    let mut out: Vec<String> = Vec::new();
    out.push(header.to_string());
    for line in &problem_pods {
        out.push(line.to_string());
    }

    let mut counts_parts: Vec<String> = Vec::new();
    if running > 0 {
        counts_parts.push(format!("{} running", running));
    }
    for (status, count) in &status_counts {
        counts_parts.push(format!("{} {}", count, status));
    }
    out.push(format!("[{}]", counts_parts.join(", ")));

    out.join("\n")
}

fn filter_logs(output: &str) -> String {
    let lines_in = output.lines().count();
    if lines_in == 0 {
        return output.to_string();
    }
    let budget = (lines_in / 3).max(20).min(200);
    ccr_core::summarizer::summarize(output, budget).output
}

fn filter_describe(output: &str) -> String {
    let keep_sections = ["Name:", "Status:", "Conditions:", "Events:"];
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut annotation_count = 0usize;
    let mut in_annotations = false;

    for line in output.lines() {
        let t = line.trim();

        if t.starts_with("Annotations:") || t.starts_with("Labels:") {
            in_annotations = true;
            annotation_count = 0;
            out.push(line.to_string());
            continue;
        }

        if in_annotations {
            if line.starts_with(' ') || line.starts_with('\t') {
                annotation_count += 1;
                if annotation_count <= 5 {
                    out.push(line.to_string());
                } else if annotation_count == 6 {
                    out.push(format!("[{} annotations]", annotation_count));
                }
                continue;
            } else {
                in_annotations = false;
            }
        }

        let is_section = keep_sections.iter().any(|s| t.starts_with(s));
        if is_section {
            in_section = true;
        } else if !t.is_empty() && !line.starts_with(' ') && !line.starts_with('\t') {
            in_section = false;
        }

        if in_section || is_section {
            out.push(line.to_string());
        }
    }

    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

fn filter_changes(output: &str) -> String {
    let out: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && (t.contains("created")
                    || t.contains("deleted")
                    || t.contains("configured")
                    || t.contains("unchanged")
                    || t.contains("error")
                    || t.contains("Error")
                    || t.starts_with("deployment.")
                    || t.starts_with("service.")
                    || t.starts_with("pod/"))
        })
        .collect();
    if out.is_empty() {
        output.to_string()
    } else {
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic `kubectl get pods` snippet
    const PODS_OUTPUT: &str = "\
NAME                          READY   STATUS    RESTARTS   AGE
my-app-6d4f8b9c7-xk2pq        1/1     Running   3          5d
another-pod-5b7c9d8f4-qr3mn   2/2     Running   0          2h
";

    #[test]
    fn filter_get_keeps_name_status_ready_drops_age_restarts() {
        let result = filter_get(PODS_OUTPUT);
        // Should contain NAME, READY, STATUS columns
        assert!(result.contains("NAME"), "missing NAME header");
        assert!(result.contains("STATUS"), "missing STATUS header");
        assert!(result.contains("READY"), "missing READY header");
        // AGE and RESTARTS should NOT appear as column headers
        let header_line = result.lines().next().unwrap();
        assert!(!header_line.contains("AGE"), "AGE should be dropped");
        assert!(!header_line.contains("RESTARTS"), "RESTARTS should be dropped");
        // Data should contain pod name
        assert!(result.contains("my-app-6d4f8b9c7-xk2pq"));
        assert!(result.contains("Running"));
    }

    #[test]
    fn filter_get_fallback_keeps_first_three_columns_when_no_interesting() {
        // Columns that are not in INTERESTING_COLUMNS
        let weird = "\
FOO   BAR   BAZ   QUUX
aaa   bbb   ccc   ddd
";
        let result = filter_get(weird);
        let header = result.lines().next().unwrap();
        // Should keep the first 3: FOO, BAR, BAZ
        assert!(header.contains("FOO"));
        assert!(header.contains("BAR"));
        assert!(header.contains("BAZ"));
        // QUUX should be dropped
        assert!(!header.contains("QUUX"), "QUUX should be dropped in fallback");
    }

    #[test]
    fn filter_get_caps_at_30_rows() {
        // Build a table with 35 data rows
        let mut lines = vec![
            "NAME                    STATUS    READY".to_string(),
        ];
        for i in 0..35 {
            lines.push(format!("pod-{:<20} Running   1/1", i));
        }
        let input = lines.join("\n");
        let result = filter_get(&input);
        let result_lines: Vec<&str> = result.lines().collect();
        // header + 30 data + 1 "[+N more]" line
        assert_eq!(result_lines.len(), 32, "expected header + 30 rows + tail line");
        assert!(result_lines.last().unwrap().contains("[+5 more]"));
    }

    #[test]
    fn filter_changes_keeps_configured_and_created_lines() {
        let input = "\
deployment.apps/my-app configured
some noise line with no keywords
service/my-svc created
another irrelevant line
pod/debug-pod-xyz unchanged
";
        let result = filter_changes(input);
        assert!(result.contains("configured"));
        assert!(result.contains("created"));
        assert!(result.contains("unchanged"));
        assert!(!result.contains("noise"), "noise lines should be dropped");
        assert!(!result.contains("irrelevant"), "irrelevant lines should be dropped");
    }

    #[test]
    fn filter_changes_returns_original_when_nothing_matches() {
        let input = "no relevant lines here\njust random text\n";
        let result = filter_changes(input);
        // passthrough preserves the original (including trailing newline)
        assert_eq!(result, input);
    }

    // --- filter_get_pods ---

    const ALL_RUNNING_PODS: &str = "\
NAME                          READY   STATUS    RESTARTS   AGE
my-app-6d4f8b9c7-xk2pq        1/1     Running   3          5d
another-pod-5b7c9d8f4-qr3mn   2/2     Running   0          2h
";

    const MIXED_PODS: &str = "\
NAME                          READY   STATUS             RESTARTS   AGE
my-app-6d4f8b9c7-xk2pq        1/1     Running            3          5d
bad-pod-5b7c9d8f4-qr3mn       0/1     CrashLoopBackOff   5          1h
pending-pod-abc123             0/1     Pending            0          10m
";

    #[test]
    fn all_running_returns_single_summary_line() {
        let handler = KubectlHandler;
        let args: Vec<String> = vec!["kubectl".into(), "get".into(), "pods".into()];
        let result = handler.filter(ALL_RUNNING_PODS, &args);
        assert_eq!(result, "[2 pods, all running]");
    }

    #[test]
    fn mixed_pods_shows_only_problem_pods_with_counts() {
        let handler = KubectlHandler;
        let args: Vec<String> = vec!["kubectl".into(), "get".into(), "pods".into()];
        let result = handler.filter(MIXED_PODS, &args);
        assert!(result.contains("bad-pod"), "should show problem pod");
        assert!(result.contains("pending-pod"), "should show pending pod");
        assert!(!result.contains("my-app-6d4f8b9c7"), "should not show running pods");
        assert!(result.contains("1 running"), "should show running count");
    }

    #[test]
    fn all_namespaces_falls_back_to_filter_get() {
        let handler = KubectlHandler;
        let args: Vec<String> = vec!["kubectl".into(), "get".into(), "pods".into(), "-A".into()];
        let result = handler.filter(ALL_RUNNING_PODS, &args);
        // filter_get output should have column headers like NAME STATUS READY
        assert!(result.contains("NAME"), "should fall back to filter_get output");
    }
}
