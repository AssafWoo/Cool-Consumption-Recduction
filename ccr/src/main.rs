use clap::{Parser, Subcommand, ValueEnum};

#[derive(ValueEnum, Clone, PartialEq, Debug, Default)]
enum AgentTarget {
    #[default]
    Claude,
    Cursor,
    /// VS Code GitHub Copilot
    Copilot,
    /// Gemini CLI
    Gemini,
    /// Cline (.clinerules integration)
    Cline,
    /// Install for all detected agents
    All,
}

mod agents;
mod analytics_db;
mod cmd;
mod config_loader;
mod handlers;
mod hook;
mod integrity;
mod intent;
mod noise_learner;
mod pre_cache;
mod result_cache;
mod session;
mod user_filters;
mod util;
mod zoom_store;

#[derive(Parser)]
#[command(name = "ccr", about = "Cool Cost Reduction — LLM token optimizer", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Filter stdin to reduce token count
    Filter {
        /// Command hint for selecting filter rules (e.g. cargo, git, npm)
        #[arg(long)]
        command: Option<String>,
    },
    /// Show token savings analytics
    Gain {
        /// Show per-day history instead of overall summary
        #[arg(long)]
        history: bool,
        /// Number of days to include in the history view
        #[arg(long, default_value = "14")]
        days: u32,
        /// Show per-command breakdown table
        #[arg(long)]
        breakdown: bool,
        /// Show insight view: categorized savings and top individual wins
        #[arg(long)]
        insight: bool,
    },
    /// Diagnose CCR installation: hook scripts, settings, analytics DB
    Doctor,
    /// PostToolUse hook mode for Claude Code (hidden)
    #[command(hide = true)]
    Hook,
    /// Install CCR hooks into Claude Code or Cursor
    Init {
        /// Remove CCR hooks and scripts instead of installing them
        #[arg(long)]
        uninstall: bool,
        /// Target agent to install hooks for
        #[arg(long, value_enum, default_value = "claude")]
        agent: AgentTarget,
    },
    /// Execute a command through CCR's specialized handlers
    Run {
        /// The command and its arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Rewrite a command string for PreToolUse injection (hidden)
    #[command(hide = true)]
    Rewrite {
        /// Full command string to rewrite
        command: String,
    },
    /// Execute a command raw (no filtering) and record analytics
    Proxy {
        /// The command and its arguments
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Scan Claude Code history and report missed optimization opportunities
    Discover,
    /// Print the original lines from a collapsed or omitted block
    Expand {
        /// Zoom block ID shown in compressed output (e.g. ZI_1)
        id: Option<String>,
        /// List all available block IDs
        #[arg(long)]
        list: bool,
    },
    /// Show or reset learned noise patterns for the current project
    Noise {
        /// Clear all learned patterns for this project
        #[arg(long)]
        reset: bool,
    },
    /// Apply read filtering to a file (diagnostic — shows token savings)
    ReadFile {
        /// File path, or - for stdin
        file: String,
        /// Filter level: passthrough, auto, strip, aggressive
        #[arg(long, default_value = "auto")]
        level: String,
    },
    /// Check integrity of installed PandaFilter hook scripts
    Verify,
    /// Update PandaFilter (use `brew upgrade assafwoo/pandafilter/ccr` instead)
    Update,
    /// Compress a conversation JSON to reduce token count
    Compress {
        /// Path to conversation JSON file (use - for stdin)
        #[arg(default_value = "-")]
        input: String,
        /// Write compressed output to file (default: stdout)
        #[arg(long, short = 'o')]
        output: Option<String>,
        /// Number of most-recent turns to preserve verbatim
        #[arg(long, default_value = "3")]
        recent_turns: usize,
        /// Number of tier-1 turns (moderate compression) after recent turns
        #[arg(long, default_value = "5")]
        tier1_turns: usize,
        /// Ollama base URL for generative summarization (optional)
        #[arg(long)]
        ollama: Option<String>,
        /// Ollama model to use
        #[arg(long, default_value = "mistral:instruct")]
        ollama_model: String,
        /// Target token budget (compress until under this limit)
        #[arg(long)]
        max_tokens: Option<usize>,
        /// Only print savings estimate without writing output
        #[arg(long)]
        dry_run: bool,
        /// Find and compress the most recently modified conversation in ~/.claude/projects/
        #[arg(long)]
        scan_session: bool,
    },
}

fn main() {
    // Apply config-driven model selection and extra keep patterns before any BERT use.
    // set_model_name is no-op after first call, so this must run before any summarization.
    if let Ok(config) = config_loader::load_config() {
        ccr_core::summarizer::set_model_name(&config.global.bert_model);
        ccr_core::summarizer::set_extra_keep_patterns(config.global.hard_keep_patterns.clone());
    }

    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Filter { command } => cmd::filter::run(command),
        Commands::Gain { history, days, breakdown, insight } => cmd::gain::run(history, days, breakdown, insight),
        Commands::Doctor => cmd::doctor::run(),
        Commands::Hook => hook::run(),
        Commands::Init { uninstall, agent } => match (uninstall, agent) {
            (true,  AgentTarget::Claude)  => uninstall_ccr(),
            (true,  AgentTarget::Cursor)  => uninstall_cursor(),
            (false, AgentTarget::Claude)  => init(),
            (false, AgentTarget::Cursor)  => init_cursor(),
            (false, AgentTarget::Copilot) => init_agent("copilot"),
            (false, AgentTarget::Gemini)  => init_agent("gemini"),
            (false, AgentTarget::Cline)   => init_agent("cline"),
            (false, AgentTarget::All)     => init_all_agents(),
            (true,  AgentTarget::Copilot) => uninstall_agent("copilot"),
            (true,  AgentTarget::Gemini)  => uninstall_agent("gemini"),
            (true,  AgentTarget::Cline)   => uninstall_agent("cline"),
            (true,  AgentTarget::All)     => uninstall_all_agents(),
        },
        Commands::Run { args } => cmd::run::run(args),
        Commands::Rewrite { command } => cmd::rewrite::run(command),
        Commands::Proxy { args } => cmd::proxy::run(args),
        Commands::Discover => cmd::discover::run(),
        Commands::Expand { id, list } => cmd::expand::run(id.as_deref().unwrap_or(""), list),
        Commands::Noise { reset } => cmd::noise::run(reset),
        Commands::ReadFile { file, level } => cmd::read_cmd::run(&file, &level),
        Commands::Verify => cmd::verify::run(),
        Commands::Update => {
            // Detect the bad-keg migration case: older installs stored the keg
            // as version "64" (inferred from "arm64" in the asset URL). brew upgrade
            // then skips the update because it thinks 64 > 0.5.x.
            let has_bad_keg = std::process::Command::new("brew")
                .args(["--cellar", "assafwoo/pandafilter/ccr"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|cellar| {
                    let path = cellar.trim().to_string();
                    std::path::Path::new(&path).join("64").exists()
                })
                .unwrap_or(false);

            if has_bad_keg {
                println!("ccr update is deprecated — and your install has a known version mismatch.");
                println!();
                println!("Your brew keg is stored as version \"64\" (a one-time bug from an older");
                println!("formula). brew upgrade won't fix it because 64 > 0.5.x.");
                println!();
                println!("Fix it with a one-time reinstall:");
                println!("  brew reinstall assafwoo/pandafilter/ccr");
                println!();
                println!("After that, future updates work normally with:");
                println!("  brew upgrade assafwoo/pandafilter/ccr");
            } else {
                println!("ccr update is deprecated.");
                println!();
                println!("Update with Homebrew:");
                println!("  brew update && brew upgrade assafwoo/pandafilter/ccr");
            }
            Ok(())
        }
        Commands::Compress { input, output, recent_turns, tier1_turns, ollama, ollama_model, max_tokens, dry_run, scan_session } =>
            cmd::compress::run(&input, output.as_deref(), recent_turns, tier1_turns, ollama.as_deref(), &ollama_model, max_tokens, dry_run, scan_session),
    };
    if let Err(e) = result {
        eprintln!("ccr error: {}", e);
        std::process::exit(1);
    }
}

fn init() -> anyhow::Result<()> {
    use serde_json::Value;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

    let settings_path = home.join(".claude").join("settings.json");
    let hooks_dir = home.join(".claude").join("hooks");

    // Write ccr-rewrite.sh
    std::fs::create_dir_all(&hooks_dir)?;
    let rewrite_script_path = hooks_dir.join("ccr-rewrite.sh");
    // Resolve the binary path for use inside the hook script and settings.json.
    // Prefer the same binary that is currently running; fall back to PATH lookup.
    let ccr_bin = std::env::current_exe()
        .ok()
        .unwrap_or_else(|| std::path::PathBuf::from("ccr"));
    let ccr_bin_str = ccr_bin.to_string_lossy();

    let rewrite_script = format!(r#"#!/usr/bin/env bash
INPUT=$(cat)
CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty')
[ -z "$CMD" ] && exit 0
REWRITTEN=$(CCR_SESSION_ID=$PPID "{ccr_bin_str}" rewrite "$CMD" 2>/dev/null) || exit 0
[ "$CMD" = "$REWRITTEN" ] && exit 0
ORIGINAL_INPUT=$(echo "$INPUT" | jq -c '.tool_input')
UPDATED_INPUT=$(echo "$ORIGINAL_INPUT" | jq --arg cmd "$REWRITTEN" '.command = $cmd')
jq -n --argjson updated "$UPDATED_INPUT" \
  '{{"hookSpecificOutput":{{"hookEventName":"PreToolUse","permissionDecision":"allow",
    "permissionDecisionReason":"CCR auto-rewrite","updatedInput":$updated}}}}'
"#, ccr_bin_str = ccr_bin_str);
    std::fs::write(&rewrite_script_path, rewrite_script)?;
    // chmod +x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&rewrite_script_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&rewrite_script_path, perms)?;
    }

    // Write integrity baseline (SHA-256 of the hook script)
    if let Err(e) = crate::integrity::write_baseline(&rewrite_script_path, &hooks_dir) {
        eprintln!("warning: could not write integrity baseline: {e}");
    }

    // Load or create settings.json
    let mut settings: Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // CCR_SESSION_ID=$PPID passes Claude Code's PID so all hook invocations
    // within one session share the same state file.
    let ccr_hook_cmd = format!("CCR_SESSION_ID=$PPID {} hook", ccr_bin_str);
    let ccr_rewrite_cmd = rewrite_script_path.to_string_lossy().to_string();

    // Merge CCR entries into existing hook arrays rather than overwriting them.
    // This preserves hooks from other tools (e.g. RTK).
    merge_hook(&mut settings, "PostToolUse", "Bash", &ccr_hook_cmd);
    merge_hook(&mut settings, "PostToolUse", "Read", &ccr_hook_cmd);
    merge_hook(&mut settings, "PostToolUse", "Glob", &ccr_hook_cmd);
    merge_hook(&mut settings, "PreToolUse",  "Bash", &ccr_rewrite_cmd);

    let parent = settings_path.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;

    println!("CCR hooks installed:");
    println!("  PostToolUse: {} → {}", ccr_hook_cmd, settings_path.display());
    println!("  PreToolUse:  {} → {}", ccr_rewrite_cmd, settings_path.display());

    // Pre-download the BERT model now so it's ready before the first Claude session.
    println!();
    if let Err(e) = ccr_core::summarizer::preload_model() {
        eprintln!("warning: could not pre-load BERT model: {e}");
        eprintln!("         it will download automatically on first use.");
    }

    Ok(())
}

fn uninstall_ccr() -> anyhow::Result<()> {
    use serde_json::Value;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

    let settings_path = home.join(".claude").join("settings.json");
    let hooks_dir = home.join(".claude").join("hooks");
    let rewrite_script_path = hooks_dir.join("ccr-rewrite.sh");
    let hash_file_path = hooks_dir.join(".ccr-hook.sha256");

    // Remove hook script
    if rewrite_script_path.exists() {
        std::fs::remove_file(&rewrite_script_path)?;
        println!("Removed {}", rewrite_script_path.display());
    }

    // Remove integrity hash file
    if hash_file_path.exists() {
        // Make writable first in case write_baseline set it to 0o444
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&hash_file_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o644);
                let _ = std::fs::set_permissions(&hash_file_path, perms);
            }
        }
        std::fs::remove_file(&hash_file_path)?;
        println!("Removed {}", hash_file_path.display());
    }

    // Strip CCR entries from settings.json
    if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        let mut settings: Value = serde_json::from_str(&content).unwrap_or(serde_json::json!({}));

        let events = ["PostToolUse", "PreToolUse"];
        for event in &events {
            if let Some(arr) = settings["hooks"][event].as_array_mut() {
                arr.retain(|entry| {
                    // Remove entries whose hooks list contains a ccr command,
                    // or whose command field references ccr.
                    let cmd = entry["command"].as_str().unwrap_or("");
                    if cmd.contains("ccr") {
                        return false;
                    }
                    if let Some(hooks) = entry["hooks"].as_array() {
                        let has_ccr = hooks.iter().any(|h| {
                            h["command"].as_str().unwrap_or("").contains("ccr")
                        });
                        if has_ccr {
                            return false;
                        }
                    }
                    true
                });
            }
        }

        std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
        println!("Removed CCR hooks from {}", settings_path.display());
    }

    println!();
    println!("CCR hooks removed. The binary itself can be uninstalled with:");
    println!("  brew uninstall ccr          # if installed via Homebrew");
    println!("  cargo uninstall ccr         # if installed via cargo");

    Ok(())
}

fn init_cursor() -> anyhow::Result<()> {
    use serde_json::Value;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

    let cursor_dir = home.join(".cursor");

    // Only install if Cursor is already present on this machine.
    // Avoids creating a stray ~/.cursor/ on machines that don't use Cursor.
    if !cursor_dir.exists() {
        println!("Cursor not found (no ~/.cursor directory) — skipping Cursor install.");
        println!("If you install Cursor later, run: ccr init --agent cursor");
        return Ok(());
    }

    let hooks_dir = cursor_dir.join("hooks");
    let hooks_json_path = cursor_dir.join("hooks.json");

    std::fs::create_dir_all(&hooks_dir)?;

    let ccr_bin = std::env::current_exe()
        .ok()
        .unwrap_or_else(|| std::path::PathBuf::from("ccr"));
    let ccr_bin_str = ccr_bin.to_string_lossy();

    // Cursor preToolUse hook: rewrites commands using Cursor's JSON format.
    // Must return valid JSON on ALL code paths (Cursor rejects empty output).
    let rewrite_script = format!(r#"#!/usr/bin/env bash
# ccr-hook-version: 1
# CCR Cursor hook — rewrites commands for token savings.
# Cursor requires JSON on ALL code paths — returns {{}} when no rewrite applies.
INPUT=$(cat)
CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty')
if [ -z "$CMD" ]; then echo '{{}}'; exit 0; fi
REWRITTEN=$(CCR_SESSION_ID=$PPID "{ccr_bin_str}" rewrite "$CMD" 2>/dev/null) || {{ echo '{{}}'; exit 0; }}
if [ "$CMD" = "$REWRITTEN" ]; then echo '{{}}'; exit 0; fi
ORIGINAL=$(echo "$INPUT" | jq -c '.tool_input')
UPDATED=$(echo "$ORIGINAL" | jq --arg cmd "$REWRITTEN" '.command = $cmd')
jq -n --argjson updated "$UPDATED" '{{"permission":"allow","updated_input":$updated}}'
"#, ccr_bin_str = ccr_bin_str);

    let script_path = hooks_dir.join("ccr-rewrite.sh");
    std::fs::write(&script_path, &rewrite_script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms)?;
    }

    // Make hash file writable before (re-)writing the baseline, in case a previous
    // init set it to 0o444 read-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let hash_file = hooks_dir.join(".ccr-hook.sha256");
        if hash_file.exists() {
            if let Ok(meta) = std::fs::metadata(&hash_file) {
                let mut perms = meta.permissions();
                perms.set_mode(0o644);
                let _ = std::fs::set_permissions(&hash_file, perms);
            }
        }
    }
    if let Err(e) = crate::integrity::write_baseline(&script_path, &hooks_dir) {
        eprintln!("warning: could not write integrity baseline: {e}");
    }

    // Load or create hooks.json
    let mut root: Value = if hooks_json_path.exists() {
        let content = std::fs::read_to_string(&hooks_json_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({"version": 1}))
    } else {
        serde_json::json!({"version": 1})
    };

    // Strip any existing CCR entries first so re-running init with a new binary
    // path replaces rather than accumulates stale entries.
    // Use get_mut (not IndexMut) to avoid inserting phantom null keys.
    for event in &["preToolUse", "postToolUse"] {
        if let Some(arr) = root
            .get_mut("hooks")
            .and_then(|h| h.get_mut(*event))
            .and_then(|e| e.as_array_mut())
        {
            arr.retain(|e| !e["command"].as_str().unwrap_or("").contains("ccr"));
        }
    }

    // PreToolUse: command rewriter
    cursor_insert_hook_entry(
        &mut root,
        "preToolUse",
        serde_json::json!({"command": "./hooks/ccr-rewrite.sh", "matcher": "Shell"}),
    );

    // PostToolUse: output compressor (CCR_AGENT=cursor so hook.rs checks ~/.cursor integrity)
    let hook_cmd = format!("CCR_SESSION_ID=$PPID CCR_AGENT=cursor {} hook", ccr_bin_str);
    cursor_insert_hook_entry(
        &mut root,
        "postToolUse",
        serde_json::json!({"command": hook_cmd, "matcher": "Bash"}),
    );
    cursor_insert_hook_entry(
        &mut root,
        "postToolUse",
        serde_json::json!({"command": hook_cmd, "matcher": "Read"}),
    );
    cursor_insert_hook_entry(
        &mut root,
        "postToolUse",
        serde_json::json!({"command": hook_cmd, "matcher": "Glob"}),
    );

    std::fs::write(&hooks_json_path, serde_json::to_string_pretty(&root)?)?;

    println!("CCR hooks installed (Cursor):");
    println!("  PreToolUse:  {} → {}", script_path.display(), hooks_json_path.display());
    println!("  PostToolUse: {} → {}", hook_cmd, hooks_json_path.display());

    println!();
    if let Err(e) = ccr_core::summarizer::preload_model() {
        eprintln!("warning: could not pre-load BERT model: {e}");
        eprintln!("         it will download automatically on first use.");
    }

    Ok(())
}

fn uninstall_cursor() -> anyhow::Result<()> {
    use serde_json::Value;

    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;

    let cursor_dir = home.join(".cursor");
    let hooks_dir = cursor_dir.join("hooks");
    let script_path = hooks_dir.join("ccr-rewrite.sh");
    let hash_file_path = hooks_dir.join(".ccr-hook.sha256");
    let hooks_json_path = cursor_dir.join("hooks.json");

    if script_path.exists() {
        std::fs::remove_file(&script_path)?;
        println!("Removed {}", script_path.display());
    }

    if hash_file_path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&hash_file_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o644);
                let _ = std::fs::set_permissions(&hash_file_path, perms);
            }
        }
        std::fs::remove_file(&hash_file_path)?;
        println!("Removed {}", hash_file_path.display());
    }

    if hooks_json_path.exists() {
        let content = std::fs::read_to_string(&hooks_json_path)?;
        let mut root: Value = serde_json::from_str(&content).unwrap_or(serde_json::json!({}));

        for event in &["preToolUse", "postToolUse"] {
            if let Some(arr) = root["hooks"][event].as_array_mut() {
                arr.retain(|entry| {
                    !entry["command"].as_str().unwrap_or("").contains("ccr")
                });
            }
        }

        std::fs::write(&hooks_json_path, serde_json::to_string_pretty(&root)?)?;
        println!("Removed CCR hooks from {}", hooks_json_path.display());
    }

    println!();
    println!("CCR Cursor hooks removed. The binary itself can be uninstalled with:");
    println!("  brew uninstall ccr          # if installed via Homebrew");
    println!("  cargo uninstall ccr         # if installed via cargo");

    Ok(())
}

/// Add an entry to root["hooks"][event] array (Cursor flat format: {{command, matcher}}).
/// Returns true if actually added (false = already present by command match).
fn cursor_insert_hook_entry(
    root: &mut serde_json::Value,
    event: &str,
    entry: serde_json::Value,
) -> bool {
    let command = entry["command"].as_str().unwrap_or("").to_string();

    let root_obj = match root.as_object_mut() {
        Some(obj) => obj,
        None => return false,
    };
    root_obj.entry("version").or_insert(serde_json::json!(1));
    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .expect("hooks must be an object");
    let arr = hooks
        .entry(event)
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .expect("event must be an array");

    let matcher = entry["matcher"].as_str().unwrap_or("").to_string();
    let already = arr.iter().any(|e| {
        e["command"].as_str().unwrap_or("") == command
            && e["matcher"].as_str().unwrap_or("") == matcher
    });
    if !already {
        arr.push(entry);
        true
    } else {
        false
    }
}

// ── New agent helpers ─────────────────────────────────────────────────────────

fn init_agent(agent: &str) -> anyhow::Result<()> {
    let ccr_bin = std::env::current_exe()
        .ok()
        .unwrap_or_else(|| std::path::PathBuf::from("ccr"));
    let ccr_bin_str = ccr_bin.to_string_lossy().to_string();
    match crate::agents::get_installer(agent) {
        Some(installer) => installer.install(&ccr_bin_str),
        None => {
            anyhow::bail!("Unknown agent '{}'. Valid agents: copilot, gemini, cline", agent)
        }
    }
}

fn uninstall_agent(agent: &str) -> anyhow::Result<()> {
    match crate::agents::get_installer(agent) {
        Some(installer) => installer.uninstall(),
        None => {
            anyhow::bail!("Unknown agent '{}'. Valid agents: copilot, gemini, cline", agent)
        }
    }
}

fn init_all_agents() -> anyhow::Result<()> {
    // Always install the Claude (default) agent first
    init()?;
    // Then attempt each new agent, printing warnings on failure
    for agent in &["copilot", "gemini", "cline"] {
        if let Err(e) = init_agent(agent) {
            eprintln!("warning: could not install {} agent: {}", agent, e);
        }
    }
    Ok(())
}

fn uninstall_all_agents() -> anyhow::Result<()> {
    let _ = uninstall_ccr();
    let _ = uninstall_cursor();
    for agent in &["copilot", "gemini", "cline"] {
        if let Err(e) = uninstall_agent(agent) {
            eprintln!("warning: could not uninstall {} agent: {}", agent, e);
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────

/// Add a hook command to an existing hook-event array without removing other entries.
/// If an entry for `matcher` already contains `command`, it is not duplicated.
fn merge_hook(settings: &mut serde_json::Value, event: &str, matcher: &str, command: &str) {
    let arr = settings["hooks"][event]
        .as_array_mut()
        .map(|a| std::mem::take(a))
        .unwrap_or_default();

    let new_hook = serde_json::json!({ "type": "command", "command": command });

    // Find an existing entry for this matcher and append to its hooks list,
    // or insert a new entry if none exists.
    let mut found = false;
    let mut updated: Vec<serde_json::Value> = arr
        .into_iter()
        .map(|mut entry| {
            if entry.get("matcher").and_then(|m| m.as_str()) == Some(matcher) {
                let hooks = entry["hooks"].as_array_mut();
                if let Some(hooks) = hooks {
                    let already = hooks.iter().any(|h| {
                        h.get("command").and_then(|c| c.as_str()) == Some(command)
                    });
                    if !already {
                        hooks.push(new_hook.clone());
                    }
                }
                found = true;
            }
            entry
        })
        .collect();

    if !found {
        updated.push(serde_json::json!({
            "matcher": matcher,
            "hooks": [new_hook]
        }));
    }

    settings["hooks"][event] = serde_json::Value::Array(updated);
}
