use std::sync::OnceLock;

use regex::Regex;

use super::Handler;

pub struct DockerHandler;

impl Handler for DockerHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "logs" => {
                if !args.iter().any(|a| a == "--tail") {
                    let mut out = args.to_vec();
                    out.insert(2, "200".to_string());
                    out.insert(2, "--tail".to_string());
                    return out;
                }
            }
            "ps" => {
                if !args.iter().any(|a| a == "--format") {
                    let mut out = args.to_vec();
                    out.push("--format".to_string());
                    out.push("table {{.ID}}\t{{.Image}}\t{{.Status}}\t{{.Names}}".to_string());
                    return out;
                }
            }
            "images" => {
                if !args.iter().any(|a| a == "--format") {
                    let mut out = args.to_vec();
                    out.push("--format".to_string());
                    out.push("table {{.Repository}}\t{{.Tag}}\t{{.Size}}".to_string());
                    return out;
                }
            }
            _ => {}
        }
        args.to_vec()
    }

    fn filter(&self, output: &str, args: &[String]) -> String {
        let subcmd = args.get(1).map(|s| s.as_str()).unwrap_or("");
        // Handle compose subcommands
        let effective_subcmd = if subcmd == "compose" || subcmd == "stack" {
            args.get(2).map(|s| s.as_str()).unwrap_or("")
        } else {
            subcmd
        };

        match effective_subcmd {
            "logs" => filter_logs(output),
            "ps" => filter_ps(output),
            "images" => filter_images(output),
            _ => output.to_string(),
        }
    }
}

/// Remove ANSI escape codes from a string.
fn strip_ansi(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Matches ESC[ followed by parameter bytes and a final byte
        Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("invalid ANSI regex")
    });
    re.replace_all(s, "").into_owned()
}

/// Remove common docker/container timestamp prefixes from a log line.
/// Matches ISO-8601 timestamps like `2024-01-15T12:34:56.123456789Z ` at the
/// start of the line, which repeat on every log line and are pure noise for
/// BERT summarization.
fn strip_timestamp(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s+")
            .expect("invalid timestamp regex")
    });
    re.replace(s, "").into_owned()
}

fn filter_logs(output: &str) -> String {
    let lines_in = output.lines().count();
    if lines_in == 0 {
        return output.to_string();
    }

    // Strip ANSI codes and timestamps before summarization so BERT sees clean text.
    let cleaned: Vec<String> = output
        .lines()
        .map(|line| strip_timestamp(&strip_ansi(line)))
        .collect();
    let cleaned_output = cleaned.join("\n");

    // Anomaly-based scoring: outlier lines (errors, unique events) score highest.
    // Budget: ~30% of lines, min 20, max 200.
    let budget = (lines_in / 3).max(20).min(200);
    ccr_core::summarizer::summarize(&cleaned_output, budget).output
}

fn filter_ps(output: &str) -> String {
    // Keep only name/container ID, status, and ports columns
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return output.to_string();
    }

    // Header + data rows
    let mut out: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            // Header: keep it but truncate
            out.push(line.to_string());
        } else if !line.trim().is_empty() {
            // Data row: extract name, status, ports
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 5 {
                // Typical docker ps columns: CONTAINER ID, IMAGE, COMMAND, CREATED, STATUS, PORTS, NAMES
                let name = parts.last().unwrap_or(&"");
                let status = parts[4];
                // Try to get ports (may span multiple columns)
                let ports_start = line.rfind("  ").unwrap_or(0);
                let ports = &line[ports_start..].trim();
                out.push(format!("{} [{}] {}", name, status, ports));
            } else {
                out.push(line.to_string());
            }
        }
    }
    out.join("\n")
}

/// Parse a Docker image size string (e.g. "1.23GB", "456MB", "789kB", "512B") into MB.
fn parse_docker_size_mb(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("GB") {
        n.trim().parse::<f64>().ok().map(|v| v * 1024.0)
    } else if let Some(n) = s.strip_suffix("MB") {
        n.trim().parse::<f64>().ok()
    } else if let Some(n) = s.strip_suffix("kB") {
        n.trim().parse::<f64>().ok().map(|v| v / 1024.0)
    } else if let Some(n) = s.strip_suffix("B") {
        n.trim().parse::<f64>().ok().map(|v| v / (1024.0 * 1024.0))
    } else {
        None
    }
}

fn filter_images(output: &str) -> String {
    // Keep only repo, tag, and size
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return output.to_string();
    }

    let mut out: Vec<String> = Vec::new();
    let mut total_mb = 0.0f64;
    let mut parseable_count = 0usize;

    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            out.push("REPOSITORY           TAG       SIZE".to_string());
        } else if !line.trim().is_empty() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 7 {
                // REPOSITORY TAG IMAGE_ID CREATED VIRTUAL_SIZE
                let repo = parts[0];
                let tag = parts[1];
                let size = parts.last().unwrap_or(&"");
                if let Some(mb) = parse_docker_size_mb(size) {
                    total_mb += mb;
                    parseable_count += 1;
                }
                out.push(format!("{:<20} {:<9} {}", repo, tag, size));
            } else {
                out.push(line.to_string());
            }
        }
    }

    if parseable_count > 0 {
        let total_gb = total_mb / 1024.0;
        out.push(format!("[total: {:.1} GB across {} images]", total_gb, parseable_count));
    }

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::Handler;

    fn handler() -> DockerHandler {
        DockerHandler
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // --- strip_ansi ---

    #[test]
    fn ansi_stripping_removes_color_codes() {
        let colored = "\x1b[32mHello\x1b[0m World\x1b[1;31m!\x1b[0m";
        assert_eq!(strip_ansi(colored), "Hello World!");
    }

    #[test]
    fn ansi_stripping_is_noop_on_plain_text() {
        let plain = "plain text with no escapes";
        assert_eq!(strip_ansi(plain), plain);
    }

    #[test]
    fn ansi_stripping_handles_cursor_movement_codes() {
        // e.g. \x1b[2J (clear screen), \x1b[1A (cursor up)
        let s = "\x1b[2Jclear\x1b[1Aup";
        assert_eq!(strip_ansi(s), "clearup");
    }

    // --- strip_timestamp ---

    #[test]
    fn timestamp_stripping_removes_iso8601_prefix() {
        let line = "2024-01-15T12:34:56.123456789Z   actual log message";
        assert_eq!(strip_timestamp(line), "actual log message");
    }

    #[test]
    fn timestamp_stripping_is_noop_when_no_timestamp() {
        let line = "just a plain log line";
        assert_eq!(strip_timestamp(line), line);
    }

    #[test]
    fn timestamp_stripping_only_strips_prefix() {
        // A timestamp mid-line should not be stripped
        let line = "ERROR occurred at 2024-01-15T12:34:56.000Z in module";
        assert_eq!(strip_timestamp(line), line);
    }

    // --- filter_ps ---

    #[test]
    fn ps_output_formats_name_status_ports() {
        // Simulate a typical `docker ps` line (7 whitespace-separated columns)
        let header = "CONTAINER ID   IMAGE     COMMAND   CREATED   STATUS    PORTS     NAMES";
        // CREATED must be a single token so STATUS lands at parts[4]
        let row    = "abc123def456   nginx     \"nginx\"   2h        Up        80/tcp    my_nginx";
        let input = format!("{}\n{}", header, row);
        let result = handler().filter(&input, &args(&["docker", "ps"]));
        let lines: Vec<&str> = result.lines().collect();
        // Header preserved
        assert_eq!(lines[0], header);
        // Data row: name [status] ports
        assert!(lines[1].contains("my_nginx"), "should contain container name");
        assert!(lines[1].contains("[Up"), "should contain status");
    }

    #[test]
    fn ps_returns_empty_as_is() {
        let result = handler().filter("", &args(&["docker", "ps"]));
        assert_eq!(result, "");
    }

    // --- rewrite_args ---

    #[test]
    fn rewrite_args_adds_tail_to_logs() {
        let result = handler().rewrite_args(&args(&["docker", "logs", "mycontainer"]));
        assert!(result.contains(&"--tail".to_string()));
        assert!(result.contains(&"200".to_string()));
    }

    #[test]
    fn rewrite_args_does_not_duplicate_tail() {
        let result = handler().rewrite_args(&args(&["docker", "logs", "--tail", "50", "mycontainer"]));
        let tail_count = result.iter().filter(|a| a.as_str() == "--tail").count();
        assert_eq!(tail_count, 1, "should not add a second --tail");
    }

    #[test]
    fn rewrite_args_injects_format_for_ps() {
        let result = handler().rewrite_args(&args(&["docker", "ps", "-a"]));
        assert!(result.contains(&"--format".to_string()), "should inject --format for docker ps");
        let fmt_idx = result.iter().position(|a| a == "--format").unwrap();
        assert!(result[fmt_idx + 1].contains("{{.ID}}"), "format should include ID");
    }

    #[test]
    fn rewrite_args_injects_format_for_images() {
        let result = handler().rewrite_args(&args(&["docker", "images"]));
        assert!(result.contains(&"--format".to_string()), "should inject --format for docker images");
    }

    #[test]
    fn rewrite_args_does_not_duplicate_format_for_ps() {
        let input = args(&["docker", "ps", "--format", "{{.Names}}"]);
        let result = handler().rewrite_args(&input);
        let count = result.iter().filter(|a| a.as_str() == "--format").count();
        assert_eq!(count, 1, "should not add a second --format");
    }

    #[test]
    fn rewrite_args_leaves_unknown_subcommands_unchanged() {
        let input = args(&["docker", "pull", "nginx"]);
        let result = handler().rewrite_args(&input);
        assert_eq!(result, input);
    }

    // --- parse_docker_size_mb ---

    #[test]
    fn parse_size_gb() {
        let mb = parse_docker_size_mb("1.5GB").unwrap();
        assert!((mb - 1536.0).abs() < 0.01);
    }

    #[test]
    fn parse_size_mb() {
        let mb = parse_docker_size_mb("256MB").unwrap();
        assert!((mb - 256.0).abs() < 0.01);
    }

    #[test]
    fn parse_size_kb() {
        let mb = parse_docker_size_mb("512kB").unwrap();
        assert!((mb - 0.5).abs() < 0.001);
    }

    #[test]
    fn parse_size_bytes() {
        let mb = parse_docker_size_mb("1048576B").unwrap();
        assert!((mb - 1.0).abs() < 0.001);
    }

    #[test]
    fn parse_size_unknown_returns_none() {
        assert!(parse_docker_size_mb("???").is_none());
    }

    // --- filter_images total line ---

    #[test]
    fn images_total_line_appended() {
        // 7-column format: REPOSITORY TAG IMAGE_ID CREATED_DATE CREATED_TIME CREATED_AGO SIZE
        let input = "\
REPOSITORY           TAG       IMAGE ID      CREATED       SIZE
nginx                latest    abc123def456  2 days ago    1 week ago    2 weeks ago    128MB
redis                alpine    def789abc012  1 week ago    1 week ago    1 month ago    64MB
";
        let result = filter_images(input);
        assert!(result.contains("[total:"), "should append total line, got:\n{}", result);
    }

    #[test]
    fn images_no_total_when_no_parseable_rows() {
        let input = "REPOSITORY           TAG       SIZE\n";
        let result = filter_images(input);
        assert!(!result.contains("[total:"), "should not show total with no data rows");
    }
}
