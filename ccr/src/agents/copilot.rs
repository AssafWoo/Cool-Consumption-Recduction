//! VS Code Copilot agent installer.
//!
//! Copilot reads hooks from `.github/hooks/<name>.json` in the project root.
//! This is project-scoped — `ccr init --agent copilot` writes to the current directory.
//!
//! Hook config format (`.github/hooks/ccr-rewrite.json`):
//!   ```json
//!   { "hooks": { "PreToolUse": [
//!     { "type": "command", "command": "<script>", "cwd": ".", "timeout": 5 }
//!   ]}}
//!   ```
//!
//! Hook input (VS Code Copilot Chat, snake_case):
//!   `{"tool_name": "Bash", "tool_input": {"command": "..."}}`
//!
//! Hook output (rewrite):
//!   `{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": {"command": "..."}}}`
//!
//! Hook output (no-op): exit 0 with empty output.

use super::AgentInstaller;
use std::path::PathBuf;

pub struct CopilotInstaller;

fn github_hooks_dir() -> PathBuf {
    PathBuf::from(".github").join("hooks")
}

impl AgentInstaller for CopilotInstaller {
    fn name(&self) -> &'static str {
        "VS Code Copilot"
    }

    fn install(&self, ccr_bin: &str) -> anyhow::Result<()> {
        let hooks_dir = github_hooks_dir();
        std::fs::create_dir_all(&hooks_dir)?;

        // Hook shell script
        let script_path = hooks_dir.join("ccr-rewrite.sh");
        let script = generate_copilot_script(ccr_bin);
        std::fs::write(&script_path, &script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms)?;
        }

        // Hook config JSON
        let config_path = hooks_dir.join("ccr-rewrite.json");
        let config = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "type": "command",
                    "command": "./hooks/ccr-rewrite.sh",
                    "cwd": ".",
                    "timeout": 5
                }]
            }
        });
        std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;

        // Copilot instructions file
        let instructions_path = PathBuf::from(".github").join("copilot-instructions.md");
        let instructions = generate_copilot_instructions();
        // Append CCR block if not already present; don't overwrite existing instructions
        let existing = if instructions_path.exists() {
            std::fs::read_to_string(&instructions_path).unwrap_or_default()
        } else {
            String::new()
        };
        if !existing.contains("ccr-instructions-start") {
            let new_content = if existing.is_empty() {
                instructions
            } else {
                format!("{}\n\n{}", existing.trim_end(), instructions)
            };
            std::fs::write(&instructions_path, new_content)?;
        }

        println!("CCR hooks installed (VS Code Copilot) — project-scoped:");
        println!("  Script:       {}", script_path.display());
        println!("  Hook config:  {}", config_path.display());
        println!("  Instructions: {}", instructions_path.display());
        println!();
        println!("Commit .github/hooks/ and .github/copilot-instructions.md to activate.");

        Ok(())
    }

    fn uninstall(&self) -> anyhow::Result<()> {
        let hooks_dir = github_hooks_dir();

        for name in &["ccr-rewrite.sh", "ccr-rewrite.json"] {
            let path = hooks_dir.join(name);
            if path.exists() {
                std::fs::remove_file(&path)?;
                println!("Removed {}", path.display());
            }
        }

        // Remove CCR block from copilot-instructions.md, preserve the rest
        let instructions_path = PathBuf::from(".github").join("copilot-instructions.md");
        if instructions_path.exists() {
            let content = std::fs::read_to_string(&instructions_path)?;
            if content.contains("ccr-instructions-start") {
                let cleaned = remove_ccr_block(&content);
                if cleaned.trim().is_empty() {
                    std::fs::remove_file(&instructions_path)?;
                    println!("Removed {}", instructions_path.display());
                } else {
                    std::fs::write(&instructions_path, cleaned)?;
                    println!("Removed CCR block from {}", instructions_path.display());
                }
            }
        }

        Ok(())
    }
}

fn generate_copilot_script(ccr_bin: &str) -> String {
    format!(
        r#"#!/usr/bin/env bash
# CCR VS Code Copilot PreToolUse hook
# Rewrites shell commands for token savings.
INPUT=$(cat)
CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty')
[ -z "$CMD" ] && exit 0
REWRITTEN=$(CCR_SESSION_ID=$PPID "{ccr_bin}" rewrite "$CMD" 2>/dev/null) || exit 0
[ "$CMD" = "$REWRITTEN" ] && exit 0
jq -n --arg cmd "$REWRITTEN" '{{
  "hookSpecificOutput": {{
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "CCR auto-rewrite",
    "updatedInput": {{"command": $cmd}}
  }}
}}'
"#,
        ccr_bin = ccr_bin
    )
}

fn generate_copilot_instructions() -> String {
    r#"<!-- ccr-instructions-start -->
## CCR — Token Savings

Always prefix shell commands with `ccr run` to minimize token consumption:
- `ccr run git status` instead of `git status`
- `ccr run cargo build` instead of `cargo build`
- `ccr run kubectl get pods` instead of `kubectl get pods`

Use `ccr gain` to view token savings analytics.
<!-- ccr-instructions-end -->"#
        .to_string()
}

fn remove_ccr_block(content: &str) -> String {
    let start = "<!-- ccr-instructions-start -->";
    let end = "<!-- ccr-instructions-end -->";
    if let (Some(s), Some(e)) = (content.find(start), content.find(end)) {
        let before = content[..s].trim_end();
        let after = content[e + end.len()..].trim_start();
        if before.is_empty() {
            after.to_string()
        } else if after.is_empty() {
            before.to_string()
        } else {
            format!("{}\n\n{}", before, after)
        }
    } else {
        content.to_string()
    }
}
