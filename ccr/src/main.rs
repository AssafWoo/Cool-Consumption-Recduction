use clap::{Parser, Subcommand};

mod cmd;
mod config_loader;
mod handlers;
mod hook;
mod intent;
mod noise_learner;
mod pre_cache;
mod session;
mod util;
mod zoom_store;

#[derive(Parser)]
#[command(name = "ccr", about = "Cool Cost Reduction — LLM token optimizer")]
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
    /// Show token savings analytics (per-command breakdown)
    Gain {
        /// Show per-day history instead of overall summary
        #[arg(long)]
        history: bool,
        /// Number of days to include in the history view
        #[arg(long, default_value = "14")]
        days: u32,
    },
    /// PostToolUse hook mode for Claude Code (hidden)
    #[command(hide = true)]
    Hook,
    /// Install CCR hooks into Claude Code settings.json
    Init,
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
        Commands::Gain { history, days } => cmd::gain::run(history, days),
        Commands::Hook => hook::run(),
        Commands::Init => init(),
        Commands::Run { args } => cmd::run::run(args),
        Commands::Rewrite { command } => cmd::rewrite::run(command),
        Commands::Proxy { args } => cmd::proxy::run(args),
        Commands::Discover => cmd::discover::run(),
        Commands::Expand { id, list } => cmd::expand::run(id.as_deref().unwrap_or(""), list),
        Commands::Noise { reset } => cmd::noise::run(reset),
        Commands::Compress { input, output, recent_turns, tier1_turns, ollama, ollama_model, max_tokens } =>
            cmd::compress::run(&input, output.as_deref(), recent_turns, tier1_turns, ollama.as_deref(), &ollama_model, max_tokens),
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
