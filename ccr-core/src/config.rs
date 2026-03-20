use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CcrConfig {
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub commands: HashMap<String, CommandConfig>,
    #[serde(default)]
    pub tee: TeeConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TeeConfig {
    pub enabled: bool,
    pub mode: TeeMode,
    pub max_files: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TeeMode {
    Aggressive,
    Always,
    Never,
}

impl Default for TeeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: TeeMode::Aggressive,
            max_files: 20,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GlobalConfig {
    pub summarize_threshold_lines: usize,
    pub head_lines: usize,
    pub tail_lines: usize,
    pub strip_ansi: bool,
    pub normalize_whitespace: bool,
    pub deduplicate_lines: bool,
    /// Additional regex patterns for lines that must never be dropped.
    /// Each entry is ORed with the built-in critical pattern
    /// (error|warning|failed|fatal|panic|exception|critical).
    /// Example: ["OOMKilled", "timeout", "deadline exceeded"]
    #[serde(default)]
    pub hard_keep_patterns: Vec<String>,
    /// BERT embedding model to use for semantic summarization.
    /// Options: "AllMiniLML6V2" (default, ~90MB), "AllMiniLML12V2" (~120MB).
    /// First call wins — changing this requires restarting the process.
    #[serde(default = "default_bert_model")]
    pub bert_model: String,
    /// Commands whose output represents persistent system state.
    /// These get full-content storage in SessionEntry (no 4000-char cap),
    /// enabling accurate line-level delta across long state outputs.
    #[serde(default = "default_state_commands")]
    pub state_commands: Vec<String>,
    /// Override the cost per million input tokens used in `ccr gain`.
    /// If unset, CCR auto-detects from the ANTHROPIC_MODEL env var,
    /// falling back to $3.00/1M (Claude Sonnet 4.6).
    /// Example: cost_per_million_tokens = 15.0  # for Opus
    #[serde(default)]
    pub cost_per_million_tokens: Option<f64>,
}

fn default_bert_model() -> String {
    "AllMiniLML6V2".to_string()
}

fn default_state_commands() -> Vec<String> {
    ["git", "kubectl", "ps", "ls", "df", "docker"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            summarize_threshold_lines: 200,
            head_lines: 30,
            tail_lines: 30,
            strip_ansi: true,
            normalize_whitespace: true,
            deduplicate_lines: true,
            hard_keep_patterns: Vec::new(),
            bert_model: default_bert_model(),
            state_commands: default_state_commands(),
            cost_per_million_tokens: None,
        }
    }
}

impl CcrConfig {
    /// Return a copy of this config adjusted for the given context pressure.
    /// pressure: 0.0 = no change, 1.0 = maximum tightening.
    ///
    /// At p=1.0:
    ///   - summarize_threshold_lines shrinks to 25% of configured value (min 30)
    ///   - head_lines / tail_lines shrink to 40% of configured values (min 4 each)
    pub fn with_pressure(mut self, pressure: f32) -> Self {
        if pressure < 0.01 {
            return self;
        }
        let p = pressure.clamp(0.0, 1.0);
        let threshold_factor = 1.0 - 0.75 * p;
        self.global.summarize_threshold_lines = ((self.global.summarize_threshold_lines as f32
            * threshold_factor) as usize)
            .max(30);
        let budget_factor = 1.0 - 0.60 * p;
        self.global.head_lines =
            ((self.global.head_lines as f32 * budget_factor) as usize).max(4);
        self.global.tail_lines =
            ((self.global.tail_lines as f32 * budget_factor) as usize).max(4);
        self
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CommandConfig {
    #[serde(default)]
    pub patterns: Vec<FilterPattern>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FilterPattern {
    pub regex: String,
    pub action: FilterAction,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum FilterAction {
    Simple(SimpleAction),
    #[allow(non_snake_case)]
    ReplaceWith { ReplaceWith: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub enum SimpleAction {
    Remove,
    Collapse,
}
