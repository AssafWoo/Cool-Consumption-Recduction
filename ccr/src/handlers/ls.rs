use super::Handler;

pub struct LsHandler;

const NOISE_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "__pycache__",
    ".next",
    "dist",
    "build",
    ".cache",
    ".venv",
    "venv",
    ".DS_Store",
];

impl Handler for LsHandler {
    fn filter(&self, output: &str, _args: &[String]) -> String {
        let lines: Vec<&str> = output
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();

        if lines.is_empty() {
            return output.to_string();
        }

        // Detect if output is `ls -l` format (starts with "total" or permissions like "drwx")
        let is_long_format = lines.first().map(|l| l.starts_with("total ")).unwrap_or(false)
            || lines
                .first()
                .map(|l| {
                    l.starts_with("dr")
                        || l.starts_with("-r")
                        || l.starts_with("lr")
                        || l.starts_with("-w")
                        || l.starts_with("d-")
                })
                .unwrap_or(false);

        let raw_entries: Vec<LsEntry> = if is_long_format {
            parse_long_format(&lines)
        } else {
            parse_short_format(&lines)
        };

        // Filter out noise directories/files, but remember what was hidden
        // so Claude knows they exist.
        let mut hidden: Vec<String> = raw_entries
            .iter()
            .filter(|e| NOISE_DIRS.contains(&e.name.as_str()))
            .map(|e| e.name.clone())
            .collect();
        hidden.sort();

        let entries: Vec<LsEntry> = raw_entries
            .into_iter()
            .filter(|e| !NOISE_DIRS.contains(&e.name.as_str()))
            .collect();

        // Sort: dirs first, then files
        let mut dirs: Vec<&LsEntry> = entries.iter().filter(|e| e.is_dir).collect();
        let mut files: Vec<&LsEntry> = entries.iter().filter(|e| !e.is_dir).collect();

        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        files.sort_by(|a, b| a.name.cmp(&b.name));

        let total = dirs.len() + files.len();
        let limit = 40;
        let mut shown = 0;
        let mut out: Vec<String> = Vec::new();

        for entry in dirs.iter().chain(files.iter()) {
            if shown >= limit {
                break;
            }
            if entry.is_dir {
                out.push(format!("{}/", entry.name));
            } else {
                out.push(entry.name.clone());
            }
            shown += 1;
        }

        if total > limit {
            out.push(format!("[+{} more]", total - limit));
        }
        out.push(format!(
            "[{} dirs, {} files]",
            dirs.len(),
            files.len()
        ));

        // Extension summary: top-3 extensions, only if >= 3 distinct
        let ext_summary = build_ext_summary(&files);
        if let Some(s) = ext_summary {
            out.push(s);
        }

        if !hidden.is_empty() {
            out.push(format!("[hidden: {}]", hidden.join(", ")));
        }

        out.join("\n")
    }
}

fn build_ext_summary(files: &[&LsEntry]) -> Option<String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for f in files {
        if let Some(dot_pos) = f.name.rfind('.') {
            let ext = &f.name[dot_pos..];
            if !ext.is_empty() && ext.len() > 1 {
                *counts.entry(ext.to_string()).or_insert(0) += 1;
            }
        }
    }
    if counts.len() < 3 {
        return None;
    }
    let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let top3: Vec<String> = sorted
        .iter()
        .take(3)
        .map(|(ext, count)| format!("{} \u{d7}{}", ext, count))
        .collect();
    Some(format!("top: {}", top3.join(", ")))
}

struct LsEntry {
    name: String,
    is_dir: bool,
}

fn parse_long_format(lines: &[&str]) -> Vec<LsEntry> {
    lines
        .iter()
        .filter(|l| !l.starts_with("total "))
        .filter_map(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            if parts.is_empty() {
                return None;
            }
            let is_dir = parts[0].starts_with('d');
            let name = parts.last()?.to_string();
            Some(LsEntry { name, is_dir })
        })
        .collect()
}

fn parse_short_format(lines: &[&str]) -> Vec<LsEntry> {
    lines
        .iter()
        .flat_map(|l| {
            // Space-separated or one-per-line
            l.split_whitespace().map(|name| LsEntry {
                is_dir: name.ends_with('/'),
                name: name.trim_end_matches('/').to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_filter(output: &str) -> String {
        let handler = LsHandler;
        handler.filter(output, &[])
    }

    #[test]
    fn test_node_modules_filtered_out() {
        let output = "src/\nnode_modules/\npackage.json\nREADME.md\n";
        let result = run_filter(output);
        // node_modules must not appear as a regular entry, but may appear in [hidden: ...] note
        let lines: Vec<&str> = result.lines().collect();
        assert!(!lines.iter().any(|l| *l == "node_modules/"), "node_modules/ must not appear as entry, got: {}", result);
        assert!(result.contains("src/"), "src/ should be present, got: {}", result);
        assert!(result.contains("package.json"), "package.json should be present, got: {}", result);
    }

    #[test]
    fn test_git_filtered_out() {
        let output = ".git/\nsrc/\nmain.rs\n";
        let result = run_filter(output);
        // .git must not appear as a regular entry line
        let lines: Vec<&str> = result.lines().collect();
        assert!(!lines.iter().any(|l| *l == ".git/"), "got: {}", result);
    }

    #[test]
    fn test_target_filtered_out() {
        let output = "target/\nsrc/\nCargo.toml\n";
        let result = run_filter(output);
        // target must not appear as a regular entry line
        let lines: Vec<&str> = result.lines().collect();
        assert!(!lines.iter().any(|l| *l == "target/"), "got: {}", result);
    }

    #[test]
    fn test_extension_summary_shows_correct_counts() {
        // 3 .rs, 2 .toml, 1 .md, 1 .txt → 4 distinct extensions → top 3 shown
        let output = "a.rs\nb.rs\nc.rs\nCargo.toml\nWorkspace.toml\nREADME.md\nnotes.txt\n";
        let result = run_filter(output);
        assert!(result.contains("top:"), "should have extension summary, got: {}", result);
        assert!(result.contains(".rs"), "got: {}", result);
        assert!(result.contains(".toml"), "got: {}", result);
    }

    #[test]
    fn test_extension_summary_not_shown_when_less_than_3_distinct() {
        let output = "a.rs\nb.rs\nc.toml\n";
        let result = run_filter(output);
        // Only 2 distinct extensions → no summary
        assert!(!result.contains("top:"), "should not have extension summary, got: {}", result);
    }

    #[test]
    fn test_short_listing_under_40_works() {
        let output = "main.rs\nlib.rs\nCargo.toml\n";
        let result = run_filter(output);
        assert!(result.contains("main.rs"), "got: {}", result);
        assert!(result.contains("[0 dirs, 3 files]"), "got: {}", result);
    }

    #[test]
    fn test_hidden_dirs_note_appended() {
        let output = "src/\nnode_modules/\ntarget/\npackage.json\n";
        let result = run_filter(output);
        assert!(result.contains("[hidden:"), "should have hidden note, got: {}", result);
        assert!(result.contains("node_modules"), "node_modules should appear in hidden note, got: {}", result);
        assert!(result.contains("target"), "target should appear in hidden note, got: {}", result);
        // They must NOT appear as regular entries
        let lines: Vec<&str> = result.lines().collect();
        assert!(!lines.iter().any(|l| *l == "node_modules/" || *l == "target/"), "noise dirs must not appear as entries");
    }
}
