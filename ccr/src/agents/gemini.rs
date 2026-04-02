//! Gemini CLI agent installer.
//!
//! Gemini CLI reads hooks from `~/.gemini/settings.json`.
//!
//! Hook registration format:
//!   ```json
//!   { "hooks": { "BeforeTool": [
//!     { "matcher": "run_shell_command",
//!       "hooks": [{ "type": "command", "command": "<script_path>" }] }
//!   ]}}
//!   ```
//!
//! Hook input:  `{"tool_name": "run_shell_command", "tool_input": {"command": "..."}}`
//! Hook output (rewrite): `{"decision": "allow", "hookSpecificOutput": {"tool_input": {"command": "rewritten"}}}`
//! Hook output (no-op):   `{"decision": "allow"}`
//!
//! Exit 0 on ALL error paths — Gemini CLI terminates on non-zero hook exit.

use super::AgentInstaller;
use std::path::PathBuf;

pub struct GeminiInstaller;

fn gemini_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".gemini"))
}

fn settings_path() -> Option<PathBuf> {
    Some(gemini_dir()?.join("settings.json"))
}

impl AgentInstaller for GeminiInstaller {
    fn name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn install(&self, ccr_bin: &str) -> anyhow::Result<()> {
        let Some(gemini_dir) = gemini_dir() else {
            anyhow::bail!("Cannot determine Gemini config directory");
        };
        std::fs::create_dir_all(&gemini_dir)?;

        // Write hook script
        let script_path = gemini_dir.join("ccr-rewrite.sh");
        let script = generate_gemini_script(ccr_bin);
        std::fs::write(&script_path, &script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms)?;
        }

        // Update ~/.gemini/settings.json
        let Some(settings_path) = settings_path() else {
            anyhow::bail!("Cannot determine Gemini settings path");
        };

        let mut root: serde_json::Value = if settings_path.exists() {
            let content = std::fs::read_to_string(&settings_path)?;
            serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
        } else {
            serde_json::json!({})
        };

        // Remove existing CCR entries from BeforeTool
        if let Some(arr) = root
            .get_mut("hooks")
            .and_then(|h| h.get_mut("BeforeTool"))
            .and_then(|e| e.as_array_mut())
        {
            arr.retain(|entry| {
                !entry["hooks"]
                    .as_array()
                    .and_then(|hooks| hooks.first())
                    .and_then(|h| h["command"].as_str())
                    .unwrap_or("")
                    .contains("ccr")
            });
        }

        // Insert BeforeTool rewrite hook entry
        let script_str = script_path.to_string_lossy().to_string();
        let entry = serde_json::json!({
            "matcher": "run_shell_command",
            "hooks": [{ "type": "command", "command": script_str }]
        });

        let root_obj = root.as_object_mut().unwrap();
        let hooks = root_obj
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
            .unwrap();
        let before_tool = hooks
            .entry("BeforeTool")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .unwrap();

        let already = before_tool.iter().any(|e| {
            e["hooks"]
                .as_array()
                .and_then(|h| h.first())
                .and_then(|h| h["command"].as_str())
                .unwrap_or("")
                == script_path.to_string_lossy()
        });
        if !already {
            before_tool.push(entry);
        }

        std::fs::write(&settings_path, serde_json::to_string_pretty(&root)?)?;

        println!("CCR hooks installed (Gemini CLI):");
        println!("  Script:   {}", script_path.display());
        println!("  Settings: {}", settings_path.display());

        Ok(())
    }

    fn uninstall(&self) -> anyhow::Result<()> {
        let Some(gemini_dir) = gemini_dir() else {
            return Ok(());
        };

        let script_path = gemini_dir.join("ccr-rewrite.sh");
        if script_path.exists() {
            std::fs::remove_file(&script_path)?;
            println!("Removed {}", script_path.display());
        }

        let Some(settings_path) = settings_path() else {
            return Ok(());
        };
        if settings_path.exists() {
            let content = std::fs::read_to_string(&settings_path)?;
            let mut root: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));

            if let Some(arr) = root
                .get_mut("hooks")
                .and_then(|h| h.get_mut("BeforeTool"))
                .and_then(|e| e.as_array_mut())
            {
                arr.retain(|entry| {
                    !entry["hooks"]
                        .as_array()
                        .and_then(|hooks| hooks.first())
                        .and_then(|h| h["command"].as_str())
                        .unwrap_or("")
                        .contains("ccr")
                });
            }

            std::fs::write(&settings_path, serde_json::to_string_pretty(&root)?)?;
            println!("Removed CCR entries from {}", settings_path.display());
        }

        Ok(())
    }
}

/// Generate the Gemini CLI PreToolUse hook script.
/// Always exits 0 — Gemini CLI terminates on non-zero hook exit.
fn generate_gemini_script(ccr_bin: &str) -> String {
    format!(
        r#"#!/usr/bin/env bash
# CCR Gemini CLI BeforeTool hook
# Rewrites run_shell_command invocations for token savings.
# ALWAYS exits 0 — Gemini CLI terminates on non-zero hook exit.
INPUT=$(cat)
CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty')
if [ -z "$CMD" ]; then
  echo '{{"decision": "allow"}}'
  exit 0
fi
REWRITTEN=$(CCR_SESSION_ID=$PPID "{ccr_bin}" rewrite "$CMD" 2>/dev/null) || {{
  echo '{{"decision": "allow"}}'
  exit 0
}}
if [ "$CMD" = "$REWRITTEN" ]; then
  echo '{{"decision": "allow"}}'
  exit 0
fi
jq -n --arg cmd "$REWRITTEN" '{{
  "decision": "allow",
  "hookSpecificOutput": {{
    "tool_input": {{"command": $cmd}}
  }}
}}'
"#,
        ccr_bin = ccr_bin
    )
}
