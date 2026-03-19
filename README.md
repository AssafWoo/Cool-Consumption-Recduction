# CCR — Cool Cost Reduction

> **60–95% token savings on Claude Code tool outputs.** CCR intercepts shell commands before Claude reads their output, routes them through specialized handlers, and returns compact summaries.

---

## Contents

- [How It Works](#how-it-works)
- [Installation](#installation)
- [Usage](#usage)
- [Commands](#commands)
- [Handlers](#handlers)
- [Pipeline (Unknown Commands)](#pipeline-unknown-commands)
- [Session Intelligence](#session-intelligence)
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
    ↓ PreToolUse: no handler match (exact → alias table → BERT similarity)
    ↓ PostToolUse hook (ccr hook)
      query-biased BERT compression (~40% savings on anything)
      + session dedup: "[same output as turn N (3m ago) — 1.2k tokens saved]"
    ↓ Claude reads: compressed, deduplicated output
```

**CCR's edge over rule-based proxies:**

- **31 handlers (40+ aliases)** — covers the full surface area of common dev tools
- **BERT semantic routing** — unknown commands fuzzy-matched to nearest handler via sentence embeddings
- **Intent-aware query** — PostToolUse blends command string (30%) + last assistant message (70%) so output relevant to Claude's current task scores highest
- **Semantic line clustering** — near-duplicate lines collapse to one representative + `[N similar]` instead of repeating them N times
- **Entropy-adjusted budget** — uniform/repetitive output (npm install, progress bars) gets a tight budget automatically; diverse output gets the full budget
- **Contextual anchoring** — error lines keep their nearest semantic neighbors (function signatures, file pointers) for immediate context
- **Zero-shot noise classifier** — prototype embeddings score each line as useful vs boilerplate before anomaly ranking
- **Semantic delta compression** — repeated commands (cargo build N times) emit only new/changed lines + `[Δ from turn N: +M new, K repeated — ~T tokens saved]`; subcommand-aware keys keep `git status` and `git log` histories separate; state commands (`git`, `kubectl`, …) store full content for accurate diff beyond 4 KB
- **Per-command historical centroid** — anomaly scored against what this command *usually* produces, so truly new output is surfaced aggressively
- **Session output cache** — identical tool outputs across turns replaced with a single reference line
- **Elastic Context (EC)** — pipeline tightens dynamically as the session fills: pressure ramps 0→1 between 25k–80k cumulative output tokens, shrinking BERT threshold and head/tail budget; critical pressure (>80%) appends a warning and suggests `ccr gain`
- **Zoom-In (ZI)** — collapsed/omitted markers embed `ZI_N` IDs; run `ccr expand ZI_1` to retrieve the original lines without re-running the command
- **Session-aware compression** — sentence-level cross-turn dedup via ccr-sdk
- **Conversation compression** (ccr-sdk) — 10–20% savings per turn that compound across a long session

---

## Installation

### One-liner (macOS + Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/AssafWoo/Cool-Consumption-Reduction/main/install.sh | bash
```

Downloads the pre-built binary for your platform, installs to `~/.local/bin/ccr`, and runs `ccr init` to register the Claude Code hooks.

Make sure `~/.local/bin` is on your PATH:

```bash
export PATH="$HOME/.local/bin:$PATH"   # add to ~/.zshrc or ~/.bashrc
```

### From source

```bash
git clone https://github.com/AssafWoo/Cool-Consumption-Reduction.git && cd Cool-Consumption-Reduction
cargo build --release
cp target/release/ccr ~/.local/bin/
ccr init
```

### Verify

```bash
ccr run git status    # compact output
ccr gain              # shows a run recorded
```

`ccr init` writes `~/.claude/hooks/ccr-rewrite.sh` (PreToolUse) and merges both hook entries into `settings.json` **without removing existing hooks** from other tools.

> **First run note:** CCR downloads the BERT model (~90 MB, `all-MiniLM-L6-v2`) from HuggingFace on first use and caches it at `~/.cache/huggingface/`. Subsequent runs are instant.

---

## Usage

After `ccr init`, **everything is automatic** — no changes to how you use Claude Code.

### Zero-config workflow

CCR hooks into Claude Code at two points:

**1. Before a command runs (PreToolUse)** — known commands are transparently rewritten:
```
Claude runs: cargo build
             ↓ rewritten to: ccr run cargo build
             ↓ output filtered before Claude reads it
```

**2. After any command runs (PostToolUse)** — unknown commands get BERT compression:
```
Claude runs: some-custom-tool --flag
             ↓ output passed through BERT pipeline
             ↓ ~40% savings on anything
```

You don't invoke `ccr` directly in normal use. Just work with Claude Code as usual.

### Check your savings

```bash
ccr gain
```
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

### Find missed opportunities

```bash
ccr discover
```

Scans your Claude Code session history for Bash commands that ran without CCR. Shows which commands could have been filtered and the estimated tokens that would have been saved.

### Pipe arbitrary output manually

```bash
cargo clippy 2>&1 | ccr filter --command cargo
kubectl get pods -A 2>&1 | ccr filter --command kubectl
cat big-log-file.txt | ccr filter
```

### Recover full output

When CCR compresses aggressively (>60% savings), it appends the path to the raw output:
```
error[E0308]: mismatched types → src/main.rs:42
[full output: ~/.local/share/ccr/tee/1742198400_cargo.log]
```

Claude can `cat` that path to see the unfiltered output without re-running the command.

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

### ccr expand

```
ccr expand [ZI_N | --list]
```

Retrieve original lines that were collapsed or omitted in a previous CCR run. Each compressed marker embeds an ID (e.g. `ZI_3`):

```
[5 lines collapsed — ccr expand ZI_3]
[... 42 lines omitted — ccr expand ZI_3 ...]
```

```bash
ccr expand ZI_3       # print the original lines
ccr expand --list     # list all available IDs in this session
```

Blocks are stored per-session at `~/.local/share/ccr/expand/<session_id>/ZI_N.txt`.

### ccr proxy

Execute raw (no filtering), record analytics as a baseline. Writes a `_proxy.log` tee file.

---

## Handlers

31 handlers (40+ command aliases) in `ccr/src/handlers/`. Handler lookup is a 3-level cascade:

1. **Exact match** — direct command name lookup
2. **Static alias table** — versioned binaries (`python3.11`→`python`), wrappers (`./gradlew`→`gradle`), common aliases (`bun`→`jest`, `az`→`aws`, etc.)
3. **BERT similarity** — unknown commands embedded and compared to 15 handler representative sentences; threshold 0.55

Each handler implements:

```rust
fn rewrite_args(&self, args: &[String]) -> Vec<String>  // inject flags before execution
fn filter(&self, output: &str, args: &[String]) -> String
```

**TypeScript / JavaScript**

| Handler | Keys | Savings | Key behavior |
|---------|------|---------|-------------|
| **tsc** | `tsc` | ~90% | Groups `error TS\d+` lines by file. `Build OK` on clean. |
| **vitest** | `vitest` | ~88% | FAIL blocks + summary; drops `✓` passing lines. |
| **jest** | `jest`, `bun`, `deno`, `nx` | ~88% | `●` failure blocks + summary; drops `PASS` lines. |
| **eslint** | `eslint` | ~85% | Errors grouped by file, caps at 20 + `[+N more]`. |

**Python**

| Handler | Keys | Savings | Key behavior |
|---------|------|---------|-------------|
| **pytest** | `pytest`, `py.test` | ~87% | FAILED node IDs + AssertionError block + short summary. |
| **pip** | `pip`, `pip3`, `uv`, `poetry`, `pdm`, `conda` | ~80% | `install`: `[complete — N packages]`. `uv`: parses resolved/installed counts. |
| **python** | `python`, `python3`, `python3.X` | ~60% | Traceback: keep block + final error. Long output: BERT summarize. |

**DevOps / Cloud**

| Handler | Keys | Savings | Key behavior |
|---------|------|---------|-------------|
| **kubectl** | `kubectl`, `k`, `minikube`, `kind` | ~85% | `get`: compact table (NAME/READY/STATUS/AGE). `logs`: BERT anomaly scoring. `describe`: key sections only. |
| **gh** | `gh` | ~90% | `pr list`/`issue list`: compact tables. `pr checks`: `✓ N passed, ✗ M failed`. `run view`: failed steps only. |
| **terraform** | `terraform`, `tofu` | ~88% | `plan`: `+`/`-`/`~` lines + summary. `apply`: resource lines + completion. `init`/`validate`: errors or success. |
| **aws** | `aws`, `gcloud`, `az` | ~85% | JSON → schema (allowlisted subcommands only). `s3 ls`: grouped by prefix. |
| **make** | `make`, `gmake`, `ninja` | ~75% | Drops directory noise. Keeps compiler errors + recipe failures. |
| **go** | `go` | ~82% | `build`/`vet`: errors only. `test`: FAIL blocks. `run`: traceback or BERT. |
| **mvn** | `mvn`, `mvnw`, `./mvnw` | ~80% | Drops `[INFO]` noise; keeps errors, warnings, reactor summary. |
| **gradle** | `gradle`, `gradlew`, `./gradlew` | ~80% | Keeps FAILED tasks, Kotlin errors, failure blocks. |
| **helm** | `helm`, `helm3` | ~85% | `list`: compact table. `status`/`diff`/`template`: structured output. |

**System / Utility**

| Handler | Keys | Savings | Key behavior |
|---------|------|---------|-------------|
| **cargo** | `cargo` | ~87% | `build`/`check`/`clippy`: injects `--message-format json`, keeps errors + warning count. `test`: failure names + detail + summary. |
| **git** | `git` | ~80% | `status` caps 20 files. `log` injects `--oneline`, caps 20. `diff` keeps `+`/`-`/`@@` only. |
| **curl** | `curl` | ~96% | JSON → type schema. Arrays: first-element schema + `[N items total]`. |
| **docker** | `docker`, `docker-compose` | ~85% | `logs`: BERT anomaly scoring (centroid distance). `ps`/`images`: compact table. |
| **npm/pnpm/yarn** | `npm`, `pnpm`, `yarn` | ~85% | `install`: package count. `test`: failures + summary. |
| **journalctl** | `journalctl` | ~80% | Injects `--no-pager -n 200`. BERT anomaly scoring. |
| **psql** | `psql`, `pgcli` | ~88% | Strips table borders, caps at 20 rows + `[+N more]`. |
| **brew** | `brew` | ~75% | `install`/`update`: status lines + Caveats. `list`/`info`: compact. |
| **tree** | `tree` | ~70% | ≤30 lines pass through. >30: first 25 + `[... N more]` + summary. |
| **diff** | `diff` | ~75% | Keeps `+`/`-`/`@@`/header lines only (same as git diff). |
| **jq** | `jq` | ~80% | ≤20 lines pass through. Array: schema of first element + `[N items]`. |
| **env** | `env`, `printenv` | ~70% | Masks secrets (`KEY`, `TOKEN`, `PASSWORD`, …). Sorted, capped at 40. |
| **ls** | `ls` | ~80% | Dirs first, alphabetical, limit 40, `[N dirs, M files]` summary. |
| **cat** | `cat` | ~70% | ≤100 lines: pass through. 101–500: head/tail. >500: BERT importance scoring. |
| **grep / rg** | `grep`, `rg` | ~80% | Groups by file, truncates to 120 chars, caps at 50 matches. |
| **find** | `find` | ~78% | Strips common prefix, groups by directory, 5 files/dir, caps at 50. |

---

## Pipeline (Unknown Commands)

Any command without a handler goes through four stages:

1. **Strip ANSI** — removes color/cursor escape sequences
2. **Normalize whitespace** — trim trailing spaces, deduplicate consecutive identical lines, collapse multiple blanks
3. **Apply regex patterns** — per-command rules from config (`Remove` / `Collapse` / `ReplaceWith`)
4. **BERT semantic summarization** — triggered when line count > `summarize_threshold_lines` (default 200)

**BERT scoring:** Each line is scored as `1 - cosine_similarity(embedding, centroid)`. High score = outlier = informative. Lines matching error/warning patterns are hard-kept regardless of score. Falls back to head+tail if the model is unavailable.

Seven BERT passes run in the pipeline — all active in production:

| Pass | Trigger | What it does |
|------|---------|--------------|
| Intent-aware query | PostToolUse | Blends command + last assistant message as query; task-relevant lines rank higher |
| Semantic clustering | Any output | Groups near-identical lines (cosine > 0.85) → one rep + `[N similar]` |
| Entropy budget | Long uniform output | Samples embeddings; tight budget when variance is low, full budget when diverse |
| Contextual anchors | After anomaly selection | Keeps up to N semantic neighbors of each kept anomaly for context |
| Noise classifier | Pre-ranking | Prototype embeddings score each line as useful vs boilerplate before anomaly sort |
| Delta compression | Repeated commands | Compares against session history; suppresses shared lines, surfaces new ones |
| Historical centroid | Per-command | Scores anomaly vs rolling mean of prior runs; genuinely new output stands out more |

---

## Session Intelligence

CCR tracks state across tool-use turns within a Claude Code session. The session is identified by `CCR_SESSION_ID=$PPID` (Claude Code's PID, injected by the hook script) — stable across all hook invocations in one session. State is persisted at `~/.local/share/ccr/sessions/<id>.json`.

### B3: Cross-turn output cache

When `ccr run` produces output it embeds it (384-dim BERT) and checks against recent entries with cosine similarity > 0.92. On a hit, the output is replaced with:

```
[same output as turn 4 (3m ago) — 1.2k tokens saved]
```

On a miss, the embedding and a 600-char content preview are recorded (ring buffer, max 30 entries).

### B2: Query-biased BERT summarization

The PostToolUse hook passes the command string as a query to the summarizer. Scoring becomes:

```
score = 0.5 × anomaly_score + 0.5 × cosine_similarity(line, query_embedding)
```

This surfaces lines that are both informative (outliers) and relevant to what Claude asked for.

### C1: Sentence-level cross-turn deduplication

Before emitting output, the hook runs ccr-sdk's sentence deduplicator against the 8 most recent session entries. Sentences that repeat earlier content are replaced with `[covered in turn N]`.

### EC: Elastic Context

As cumulative session tokens grow, `ccr hook` tightens the full pipeline — not just the output budget:

| Session tokens | Pressure | Effect |
|---------------|----------|--------|
| < 25k | 0.0 | No change |
| 25k–80k | 0.0 → 1.0 | BERT threshold and head/tail budget scale linearly |
| > 80k | 1.0 | Max pressure: threshold ≤25% of configured value (min 30 lines), budget ≤40% (min 4 lines each) |
| > 80% pressure | warning | Appends `[⚠ context near full — output compressed aggressively; run ccr gain to review]` |

### C2: Session-aware compression budget

On top of EC, `ccr hook` applies a second-pass budget to the already-compressed output when the session is token-heavy (compression_factor < 0.90). Historical per-command centroid is used for this pass when available.

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

`ccr hook` receives output JSON after any Bash call. Pipeline:

1. Extract `tool_response.output` (or error)
2. Compute session `context_pressure()` → adjust pipeline config (EC)
3. Enable Zoom-In (ZI); run 4-stage BERT pipeline with command string as query (B2 biasing)
4. Persist zoom blocks to `~/.local/share/ccr/expand/` (ZI)
5. Apply delta compression against prior runs of same subcommand key (SD)
6. Apply sentence-level cross-turn dedup via ccr-sdk (C1)
7. If session is token-heavy, apply extra BERT compression (C2)
8. Embed output, update per-command centroid, record to session cache (B3)
9. Return `{ "output": "<filtered>" }`

Never fails — returns nothing on any error so Claude Code always sees a result.

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
| Handler count | **31 (40+ aliases)** | 40+ |
| Unknown commands | BERT routing + fallback (~40%) | Pass through (0%) |
| Handler routing | Exact → alias table → BERT similarity | Exact match only |
| Log handlers (docker/kubectl/journal) | BERT anomaly scoring (centroid distance) | Exact-match dedup |
| `cat` large files | BERT importance scoring | head+tail |
| Cross-turn output cache | Yes (cosine > 0.92, turn reference) | — |
| Query-biased summarization | Yes (anomaly + command relevance blend) | — |
| Session-aware compression | Yes (scales to 50% at 100k tokens) | — |
| Sentence-level cross-turn dedup | Yes (ccr-sdk, marks `[covered in turn N]`) | — |
| Conversation history compression | ccr-sdk: tiered + Ollama + dedup | — |
| Evaluation suite | ccr-eval (Q&A + conv fixtures) | — |
| Hooks preserved on init | Yes (merges arrays) | Overwrites |

---

## Crate Overview

```
ccr/                     CLI binary
  src/main.rs            Commands enum, init() with merge_hook()
  src/hook.rs            PostToolUse: EC pressure, ZI enable, B2 query-biased BERT,
                         SD delta, C1 sentence dedup, C2 budget, B3 cache record
  src/session.rs         Per-session state: output cache, context_pressure (EC),
                         compression budget, subcommand-aware delta keys (SD),
                         state_content full storage, cross-turn dedup context
  src/zoom_store.rs      ZI block persistence: save_blocks / load_block / list_blocks
  src/cmd/               filter, run, proxy, rewrite, gain, discover, expand (ZI)
  src/handlers/          31 handlers: cargo, git, curl, docker, npm, ls, read,
                         grep, find, tsc, vitest, jest, eslint, pytest, pip,
                         python, kubectl, gh, terraform, aws, make, go, maven,
                         brew, helm, journalctl, psql, tree, diff, jq, env
                         + util.rs (compact_table, test_failures, is_hard_keep,
                                    json_to_schema, cosine_similarity)

ccr-core/                Core library (no I/O)
  src/pipeline.rs        ANSI strip → normalize → patterns → BERT summarize
                         (process() accepts optional query for B2; returns zoom_blocks)
  src/zoom.rs            Thread-local ZI accumulator: enable/disable/register/drain
  src/summarizer.rs      fastembed AllMiniLML6V2, OnceCell model cache;
                         anomaly scoring, clustering, intent-aware query,
                         entropy budget, contextual anchoring, noise classifier,
                         delta compression, historical-centroid scoring
  src/analytics.rs       Analytics struct (command, subcommand, duration_ms)
  src/config.rs          CcrConfig, GlobalConfig (with_pressure EC), TeeConfig, FilterAction
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

---

## Uninstall

```bash
# Remove the binary
rm ~/.local/bin/ccr

# Remove hooks from Claude Code settings
# Open ~/.claude/settings.json and delete the two CCR entries under
# hooks.PostToolUse and hooks.PreToolUse

# Remove cached data (optional)
rm -rf ~/.local/share/ccr
rm -rf ~/.local/share/ccr/.bert_ready
rm ~/.claude/hooks/ccr-rewrite.sh
```

---

## Contributing

Contributions welcome — open an issue or PR on [GitHub](https://github.com/AssafWoo/Cool-Consumption-Reduction).

To add a new handler: implement the `Handler` trait and register it in `get_handler()` in `ccr/src/handlers/mod.rs`. See any existing handler (e.g. `git.rs`) as a template.

---

## License

MIT — see [LICENSE](LICENSE) for details.
