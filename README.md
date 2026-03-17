# CCR — Cool Cost Reduction

> **60–95% token savings on Claude Code tool outputs.** CCR intercepts shell commands before Claude reads their output, routes them through specialized handlers, and returns compact summaries.

---

## Contents

- [How It Works](#how-it-works)
- [Installation](#installation)
- [Commands](#commands)
- [Handlers](#handlers)
- [Pipeline (Unknown Commands)](#pipeline-unknown-commands)
- [Configuration](#configuration)
- [Analytics](#analytics)
- [Tee: Raw Output Recovery](#tee-raw-output-recovery)
- [CCR-SDK: Conversation Compression](#ccr-sdk-conversation-compression)
- [Hook Architecture](#hook-architecture)
- [CCR vs RTK](#ccr-vs-rtk)
- [Crate Overview](#crate-overview)

---

## How It Works

```
Claude issues: git status
    ↓ PreToolUse hook (ccr-rewrite.sh)
      git is a known handler → patches command to: ccr run git status
    ↓ ccr run executes git status, filters output, writes tee file
    ↓ Claude reads: compact changed-file list (80% fewer tokens)

Claude issues: some-unknown-tool
    ↓ PreToolUse: no handler → passes through unchanged
    ↓ PostToolUse hook (ccr hook)
      BERT semantic compression (~40% savings on anything)
    ↓ Claude reads: compressed output
```

**CCR's edge over rule-based proxies:**

- **BERT semantic compression** — unknown commands get ~40% savings instead of 0%
- **Docker semantic dedup** — "connection refused to 10.0.0.1" and "…10.0.0.2" collapse to one line; exact-match tools keep both
- **Smart `cat`** — files >500 lines use BERT importance scoring, not head+tail
- **Conversation compression** (ccr-sdk) — 10–20% savings per turn that compound across a long session

---

## Installation

```bash
git clone <repo> && cd ccr
cargo build --release
cp target/release/ccr ~/.local/bin/   # or any directory on PATH
ccr init                               # registers hooks in ~/.claude/settings.json
```

`ccr init` writes `~/.claude/hooks/ccr-rewrite.sh` (PreToolUse) and merges both hook entries into `settings.json` **without removing existing hooks** from other tools.

Verify:

```bash
ccr run git status    # should print compact output
ccr gain              # should show a run recorded
```

---

## Commands

### ccr run

Execute a command through CCR's handler pipeline.

```
ccr run <command> [args...]
```

1. Looks up handler for `argv[0]`
2. `handler.rewrite_args()` — optionally injects flags (e.g. `--message-format json` for cargo)
3. Executes, capturing stdout + stderr combined
4. Writes raw output to `~/.local/share/ccr/tee/<ts>_<cmd>.log`
5. `handler.filter()` → compact output; falls back to BERT pipeline if no handler
6. Appends `[full output: <path>]` when savings exceed 60%
7. Records `{ command, subcommand, input_tokens, output_tokens, duration_ms }` to analytics
8. Propagates original exit code

### ccr gain

```
ccr gain [--history] [--days N]
```

**Default view:**
```
CCR Token Savings
═════════════════════════════════════════════════
  Runs:           142
  Tokens saved:   182.2k  (77.7%)
  Cost saved:     ~$0.547  (at $3.00/1M input tokens)
  Today:          23 runs · 31.4k saved · 74.3%

Per-Command Breakdown
─────────────────────────────────────────────────────────────
COMMAND        RUNS       SAVED   SAVINGS   AVG ms  IMPACT
─────────────────────────────────────────────────────────────
cargo            45       89.2k     87.2%      420  ████████████████████
git              31       41.1k     79.1%       82  ████████████████
curl             12       31.2k     94.3%      210  ██████████████████
(pipeline)       18       12.4k     42.1%        —  ████████
```

**History view:**
```bash
ccr gain --history          # last 14 days (default)
ccr gain --history --days 7
```
```
CCR Daily History  (last 14 days)
────────────────────────────────────────────────────────────
DATE          RUNS        SAVED   SAVINGS   COST SAVED
2026-03-17      23        31.4k     74.3%       $0.094
2026-03-16      41        58.1k     78.1%       $0.174
```

### ccr discover

Scan `~/.claude/projects/*/` JSONL history for Bash calls not yet wrapped in `ccr run`. Reports estimated savings per command and suggests running `ccr init`.

### ccr init

Installs both hooks into `~/.claude/settings.json`. Safe to re-run — merges into existing arrays so other tools' hooks are preserved. Uses the absolute path of the running binary in all hook commands.

### ccr filter

```
ccr filter [--command <hint>]
```

Reads stdin, runs the four-stage pipeline, writes to stdout. Useful for piping arbitrary output: `cargo clippy 2>&1 | ccr filter --command cargo`.

### ccr proxy

Execute raw (no filtering), record analytics as a baseline. Writes a `_proxy.log` tee file.

---

## Handlers

9 handlers registered in `ccr/src/handlers/`. Each implements:

```rust
fn rewrite_args(&self, args: &[String]) -> Vec<String>  // inject flags before execution
fn filter(&self, output: &str, args: &[String]) -> String
```

| Handler | Savings | Key behavior |
|---------|---------|-------------|
| **cargo** | ~87% | `build`/`check`/`clippy`: injects `--message-format json`, keeps only errors + warning count. `test`: parses failure names + detail section + summary line. |
| **git** | ~80% | Per-subcommand: `status` drops help text, caps at 20 files. `log` injects `--oneline`, caps 20. `diff` keeps only `+`/`-`/`@@`/header lines. `push`/`pull` drops progress noise. |
| **curl** | ~96% | JSON responses: replaces values with type names (`"string"`, `"number"`, etc.); arrays show first-element schema + `[N items total]`. Size guard: passes through if schema > original. |
| **docker** | ~85% | `logs`: `--tail 200` + **BERT semantic dedup** (cosine > 0.90 threshold). Hard-keeps errors/panics/stack traces. Falls back to exact-match dedup. `ps`/`images`: compact table. |
| **npm/pnpm/yarn** | ~85% | `install`: `[install complete — N packages]`. `test`: failures + summary line only. `run`: errors + last 5 lines + line count. |
| **ls** | ~80% | Dirs first, alphabetical, limit 40, `[N dirs, M files]` summary. |
| **cat** | ~70% | ≤100 lines: pass through. 101–500: head 60 + tail 20. >500: BERT semantic summarization (budget: 80 lines). |
| **grep / rg** | ~80% | Groups matches by file, truncates lines to 120 chars, caps at 50 matches. |
| **find** | ~78% | Strips common prefix, groups by directory, shows 5 files/dir, caps at 50 entries. |

---

## Pipeline (Unknown Commands)

Any command without a handler goes through four stages:

1. **Strip ANSI** — removes color/cursor escape sequences
2. **Normalize whitespace** — trim trailing spaces, deduplicate consecutive identical lines, collapse multiple blanks
3. **Apply regex patterns** — per-command rules from config (`Remove` / `Collapse` / `ReplaceWith`)
4. **BERT semantic summarization** — triggered when line count > `summarize_threshold_lines` (default 200)

**BERT scoring:** Each line is scored as `1 - cosine_similarity(embedding, centroid)`. High score = outlier = informative. Lines matching error/warning patterns are hard-kept regardless of score. Falls back to head+tail if the model is unavailable.

---

## Configuration

Config is loaded from the first file found: `./ccr.toml` → `~/.config/ccr/config.toml` → embedded default.

```toml
[global]
summarize_threshold_lines = 200  # trigger BERT summarization
head_lines = 30                  # head+tail fallback budget
tail_lines = 30
strip_ansi = true
normalize_whitespace = true
deduplicate_lines = true

[tee]
enabled = true
mode = "aggressive"   # "aggressive" | "always" | "never"
max_files = 20

[commands.git]
patterns = [
  { regex = "^(Counting|Compressing|Receiving|Resolving) objects:.*", action = "Remove" },
  { regex = "^remote: (Counting|Compressing|Enumerating).*", action = "Remove" },
]

[commands.cargo]
patterns = [
  { regex = "^\\s+Compiling \\S+ v[\\d.]+", action = "Collapse" },
  { regex = "^\\s+Downloaded \\S+ v[\\d.]+", action = "Remove"   },
]
```

Pattern actions: `Remove` (delete line), `Collapse` (count consecutive matches → `[N lines collapsed]`), `ReplaceWith = "text"`.

To add a custom handler, implement the `Handler` trait and register it in `get_handler()` in `ccr/src/handlers/mod.rs`.

---

## Analytics

Every CCR operation appends a record to `~/.local/share/ccr/analytics.jsonl`:

```json
{
  "input_tokens": 4821,  "output_tokens": 612,  "savings_pct": 87.3,
  "command": "cargo",    "subcommand": "build",
  "timestamp_secs": 1742198400,  "duration_ms": 3420
}
```

All fields added after the initial release use `#[serde(default)]` for backward compatibility with old records.

---

## Tee: Raw Output Recovery

`ccr run` saves raw output to `~/.local/share/ccr/tee/<ts>_<cmd>.log` before filtering. When savings exceed 60%, the filtered output includes a recovery hint:

```
error: mismatched types [src/main.rs:42]
[full output: ~/.local/share/ccr/tee/1742198400_cargo.log]
```

Claude can `cat` that path without re-running the command. Max 20 files kept; oldest rotated out.

| Mode | Behavior |
|------|----------|
| `aggressive` | Write only when savings > 60% (default) |
| `always` | Write on every `ccr run` |
| `never` | Disabled |

---

## CCR-SDK: Conversation Compression

The `ccr-sdk` crate compresses old turns in the conversation history — orthogonal to per-command savings, and compounding across the session (~10–20% per turn).

```
messages (oldest → newest):
  [tier 2][tier 2][tier 1][tier 1][verbatim][verbatim][verbatim]
```

| Tier | Default age | Compression |
|------|-------------|-------------|
| Verbatim | most recent 3 | unchanged |
| Tier 1 | next 5 | extractive: keep 55% of sentences |
| Tier 2 | older | generative (Ollama) or extractive 20% |

**Sentence selection:** BERT centroid scoring. Hard-kept by role:
- *User:* questions, code (backticks/`::`), snake_case identifiers, constraint language (`must`, `never`, `always`, `ensure`, …)
- *Assistant:* code, list items, numbers/dates/currency, constraint language

**Generative tier 2 (Ollama):** Prompts `mistral:instruct` to compress to ~60% word count. BERT quality gate rejects output with cosine similarity < 0.80 vs original, falling back to extractive.

**Semantic deduplication:** Sentences with cosine similarity > 0.92 to content in older turns are replaced with `[covered in turn N]`. Assistant messages never modified.

**Budget enforcement:** If `max_context_tokens` is set, a second pass compresses user then assistant messages oldest-first until under budget.

```rust
let result = Compressor::new(CompressionConfig::default()).compress(messages)?;
println!("Saved {} tokens", result.tokens_in - result.tokens_out);
```

---

## Hook Architecture

### PreToolUse

Runs before Bash executes. `ccr-rewrite.sh` calls `ccr rewrite "<cmd>"`:

- **Known handler** → prints `ccr run <cmd>`, exits 0; hook patches `tool_input.command`
- **Unknown** → exits 1; hook emits nothing; Claude Code uses original command
- **Compound commands** (`&&`, `||`, `;`) → each segment rewritten independently: `cargo build && git push` → `ccr run cargo build && ccr run git push`
- **Already wrapped** → exits 1 (no double-wrap)

Multiple PreToolUse hooks run in order — CCR merges into the existing array, preserving RTK's hook.

### PostToolUse

`ccr hook` receives output JSON after any Bash call. Extracts `tool_response.output`, runs the BERT pipeline, returns `{ "output": "<filtered>" }`. Never fails — returns nothing on any error so Claude Code always sees a result.

### Hook JSON contract

```json
// PreToolUse output (when rewriting):
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "permissionDecisionReason": "CCR auto-rewrite",
    "updatedInput": { "command": "ccr run git status" }
  }
}

// PostToolUse output:
{ "output": "filtered output" }
```

---

## CCR vs RTK

| Feature | CCR | RTK |
|---------|-----|-----|
| Unknown commands | BERT fallback (~40%) | Pass through (0%) |
| Docker log dedup | BERT semantic (cosine > 0.90) | Exact-match only |
| `cat` large files | BERT importance scoring | head+tail |
| Conversation history | ccr-sdk: tiered + Ollama + dedup | — |
| Evaluation suite | ccr-eval (Q&A + conv fixtures) | — |
| Handler count | 9 | 40+ |
| Hooks preserved on init | Yes (merges) | Overwrites |

---

## Crate Overview

```
ccr/                     CLI binary
  src/main.rs            Commands enum, init() with merge_hook()
  src/hook.rs            PostToolUse (JSON in → JSON out)
  src/cmd/               filter, run, proxy, rewrite, gain, discover
  src/handlers/          cargo, git, curl, docker, npm, ls, read, grep, find

ccr-core/                Core library (no I/O)
  src/pipeline.rs        ANSI strip → normalize → patterns → BERT summarize
  src/summarizer.rs      fastembed AllMiniLML6V2, line + sentence level
  src/analytics.rs       Analytics struct (command, subcommand, duration_ms)
  src/config.rs          CcrConfig, GlobalConfig, TeeConfig, FilterAction
  src/tokens.rs          tiktoken cl100k_base

ccr-sdk/                 Conversation compression
  src/compressor.rs      Tiered compression + budget enforcement
  src/deduplicator.rs    Cross-turn semantic dedup (0.92 threshold)
  src/ollama.rs          Generative summarization + BERT quality gate

ccr-eval/                Evaluation suite
  fixtures/              .qa.toml (Q&A) + .conv.toml (conversation) test data
  src/runner.rs          Fixture execution against Claude API

config/
  default_filters.toml   Embedded default config (git, cargo, npm, docker patterns)
```
