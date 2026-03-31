use std::sync::OnceLock;

use super::Handler;

pub struct TscHandler;

fn re_ts_error() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^(.+\.tsx?)\((\d+),\d+\):\s+(error|warning)\s+(TS\d+:.+)$")
            .expect("tsc error regex")
    })
}

/// Maximum length for a TypeScript error message before truncation.
/// TypeScript emits very verbose type mismatch descriptions; trim them to keep context.
const MAX_MSG_LEN: usize = 80;

impl Handler for TscHandler {
    fn filter(&self, output: &str, _args: &[String]) -> String {
        // Clean build
        if output.contains("Found 0 errors") {
            return "Build OK".to_string();
        }

        let lines: Vec<&str> = output.lines().collect();
        let mut error_count = 0usize;
        let mut warning_count = 0usize;

        // Group errors/warnings by file
        // Lines like: src/foo.ts(42,5): error TS2345: ...
        let mut grouped: Vec<(String, Vec<(String, String, String)>)> = Vec::new(); // (file, [(lineno, kind, msg)])
        for line in &lines {
            if let Some(caps) = re_ts_error().captures(line) {
                let file = caps[1].to_string();
                let lineno = caps[2].to_string();
                let kind = caps[3].to_string();
                let raw_msg = caps[4].to_string();

                // Truncate verbose type error messages
                let msg = if raw_msg.len() > MAX_MSG_LEN {
                    format!("{}…", &raw_msg[..MAX_MSG_LEN])
                } else {
                    raw_msg
                };

                if kind == "error" {
                    error_count += 1;
                } else {
                    warning_count += 1;
                }

                if let Some(last) = grouped.last_mut() {
                    if last.0 == file {
                        last.1.push((lineno, kind, msg));
                        continue;
                    }
                }
                grouped.push((file, vec![(lineno, kind, msg)]));
            }
        }

        if grouped.is_empty() {
            return output.to_string();
        }

        let mut out: Vec<String> = Vec::new();
        for (file, messages) in &grouped {
            out.push(file.clone());

            // Within each file, collapse runs of the same TS error code.
            // e.g. TS2339 appearing 4 times → "  TS2339 (×4): L12, L45, L78, L92 — msg"
            let mut i = 0;
            while i < messages.len() {
                let (lineno, kind, msg) = &messages[i];
                // Extract the TS code prefix (e.g. "TS2339")
                let ts_code = msg.split(':').next().unwrap_or("").trim();
                // Collect consecutive entries with the same code
                let mut j = i + 1;
                while j < messages.len() {
                    let (_, k2, m2) = &messages[j];
                    let code2 = m2.split(':').next().unwrap_or("").trim();
                    if code2 == ts_code && k2 == kind {
                        j += 1;
                    } else {
                        break;
                    }
                }
                let count = j - i;
                if count == 1 {
                    out.push(format!("  L{}: {} {}", lineno, kind, msg));
                } else {
                    let line_nums: Vec<String> = messages[i..j]
                        .iter()
                        .map(|(ln, _, _)| format!("L{}", ln))
                        .collect();
                    // Keep the message from the first occurrence (already truncated)
                    let msg_after_code = msg.splitn(2, ':').nth(1).unwrap_or(msg).trim();
                    let msg_preview = if msg_after_code.len() > 60 {
                        format!("{}…", &msg_after_code[..60])
                    } else {
                        msg_after_code.to_string()
                    };
                    out.push(format!(
                        "  {} (×{}): {} — {}",
                        ts_code, count,
                        line_nums.join(", "),
                        msg_preview
                    ));
                }
                i = j;
            }
        }
        out.push(format!("[{} errors, {} warnings]", error_count, warning_count));
        out.join("\n")
    }
}
