<p align="center">
  <img src="assets/logo.png" alt="PandaFilter" width="160" />
</p>

<h1 align="center">PandaFilter</h1>

<p align="center"><strong>Cut your AI agent's token bill by 60–95% — transparently, locally, automatically.</strong></p>

<p align="center">
  Built by Assaf Petronio — building PandaFilter in public, with the open-source community.
</p>

<p align="center">
  <a href="https://x.com/AssafPetronio">@AssafPetronio</a>&nbsp; • &nbsp;<a href="https://github.com/AssafWoo">github.com/AssafWoo</a>
</p>

<p align="center">
  <a href="https://github.com/AssafWoo/PandaFilter/stargazers">
    <img src="https://img.shields.io/github/stars/AssafWoo/PandaFilter?style=for-the-badge&logo=github&logoColor=white&label=Star%20the%20panda%20%F0%9F%90%BC%E2%AD%90&labelColor=4b4b4b&color=7c3aed" alt="Star PandaFilter on GitHub">
  </a>
</p>

---

## Quick start

```bash
brew tap AssafWoo/pandafilter
brew install ccr
```

**Linux / any platform:**

```bash
curl -fsSL https://raw.githubusercontent.com/AssafWoo/homebrew-pandafilter/main/install.sh | bash
```

> **First run:** CCR downloads the BERT model (~90 MB, `all-MiniLM-L6-v2`) from HuggingFace and caches it at `~/.cache/huggingface/`. Subsequent runs are instant.

---

## Why CCR?

AI coding agents are expensive to run — not because of what you ask them, but because of what they read back. Every `cargo build`, `git log`, or `npm install` dumps thousands of tokens of noise into the context window. I built CCR to fix that transparently: it sits between your agent and your shell, compresses the output, and hands back only what the model needs. No config changes, no workflow changes — just less waste.

---

## What it does

- Hooks into Claude Code, Cursor, Gemini CLI, Cline, and VS Code Copilot automatically after `ccr init`.
- Filters build logs, test noise, and progress bars before the model ever sees them.
- Uses BERT embeddings to match unknown commands to the closest handler — nothing falls through silently.
- Caches repeated commands (git, kubectl, docker, terraform) so the model isn't re-reading stale output.
- Runs 100% locally — no data leaves your machine.

---

## Token savings

Numbers from `ccr/tests/handler_benchmarks.rs`. Run `cargo test -p ccr benchmark -- --nocapture` to reproduce, or `ccr gain` to see your own live data.

| Operation | Without CCR | With CCR | Savings |
|-----------|------------:|---------:|:-------:|
| `pip install` | 1,787 | 9 | **−99%** |
| `uv sync` | 1,574 | 15 | **−99%** |
| `playwright test` | 1,367 | 19 | **−99%** |
| `gradle build` | 803 | 17 | **−98%** |
| `go test` | 4,507 | 148 | **−97%** |
| `pytest` | 3,818 | 162 | **−96%** |
| `terraform plan` | 3,926 | 163 | **−96%** |
| `npm install` | 648 | 25 | **−96%** |
| `ember build` | 3,377 | 139 | **−96%** |
| `cargo build` | 1,923 | 93 | **−95%** |
| `cargo test` | 2,782 | 174 | **−94%** |
| `next build` | 549 | 53 | **−90%** |
| `cargo clippy` | 786 | 93 | **−88%** |
| `make` | 545 | 72 | **−87%** |
| `git push` | 173 | 24 | **−86%** |
| `ls` | 691 | 102 | **−85%** |
| `webpack` | 882 | 143 | **−84%** |
| `vitest` | 625 | 103 | **−84%** |
| `nx run-many` | 1,541 | 273 | **−82%** |
| `turbo run build` | 597 | 115 | **−81%** |
| `ruff check` | 2,035 | 435 | −79% |
| `eslint` | 4,393 | 974 | −78% |
| `git log` | 1,573 | 353 | −78% |
| `grep` | 2,925 | 691 | −76% |
| `helm install` | 224 | 54 | −76% |
| `docker ps` | 1,057 | 266 | −75% |
| `golangci-lint` | 3,678 | 960 | −74% |
| `git status` | 650 | 184 | −72% |
| `kubectl get pods` | 2,306 | 689 | −70% |
| `vite build` | 526 | 182 | −65% |
| `jest` | 330 | 114 | −65% |
| `env` | 1,155 | 399 | −65% |
| `mvn install` | 4,585 | 1,613 | −65% |
| `brew install` | 368 | 148 | −60% |
| `gh pr list` | 774 | 321 | −59% |
| `git diff` | 6,370 | 2,654 | −58% |
| `biome lint` | 1,503 | 753 | −50% |
| `tsc` | 2,598 | 1,320 | −49% |
| `mypy` | 2,053 | 1,088 | −47% |
| `stylelint` | 1,100 | 845 | −23% |
| **Total** | **73,104** | **15,985** | **−78%** |

---

## Compared to doing nothing

| Scenario | Without CCR | With CCR |
|---|---|---|
| `cargo build` (errors) | 1,923 tokens | 93 tokens |
| `pytest` (all passing) | 3,818 tokens | 162 tokens |
| `npm install` | 648 tokens | 25 tokens |
| **Typical session** | **71k tokens** | **15k tokens** |

---

## Commands

**`ccr init`** — wire CCR into your agent's hooks:

```bash
ccr init                              # Claude Code (default)
ccr init --agent cursor               # Cursor
ccr init --agent gemini               # Gemini CLI
ccr init --agent cline                # Cline (.clinerules in project dir)
ccr init --agent copilot              # VS Code Copilot
ccr init --uninstall                  # remove (add --agent <x> for specific agent)
```

**`ccr gain`** — see your token savings:

```bash
ccr gain                    # overall summary
ccr gain --breakdown        # per-command table
ccr gain --history          # last 14 days
ccr gain --insight          # categorized savings + top saves
```

**`ccr doctor`** — diagnose the full installation in one command (run this first when something seems off).

**Other commands:**

```bash
ccr verify                            # check hook integrity for all installed agents
ccr discover                          # scan Claude history for commands that ran without CCR
ccr noise                             # show learned noise patterns; --reset to clear
ccr expand ZI_3                       # print original lines from a collapsed block
ccr read-file src/main.rs --level auto  # apply read-level filter and print savings
ccr compress --scan-session           # compress current conversation context
ccr filter --command cargo            # filter stdin as if it were cargo output
ccr run git status                    # run through CCR handler manually
ccr proxy git status                  # run raw (no filtering), record analytics baseline
```

---

<details>
<summary><strong>Handlers (49 handlers)</strong></summary>

49 handlers (60+ command aliases) in `ccr/src/handlers/`. Lookup cascade:

1. **User filters** — `.ccr/filters.toml` or `~/.config/ccr/filters.toml`
2. **Exact match** — direct command name
3. **Static alias table** — versioned binaries, wrappers, common aliases
4. **BERT routing** — unknown commands matched by embedding similarity

| Handler | Keys | Key behavior |
|---------|------|-------------|
| **cargo** | `cargo` | `build`/`clippy`: errors + warning count. `test`: failures + summary. |
| **git** | `git` | `status`: counts. `log`: `--oneline`, cap 20. `diff`: 2 context lines, 200-line cap. |
| **go** | `go` | `test`: NDJSON streaming, FAIL blocks + summary. `build`: errors only. |
| **ember** | `ember` | `build`: errors + summary; drops fingerprint/asset spam. `test`: failures + summary. `serve`: serving URL only. |
| **tsc** | `tsc` | Errors grouped by file; deduplicates repeated TS codes. `Build OK` on clean. Injects `--noEmit`. |
| **vitest** | `vitest` | FAIL blocks + summary; drops `✓` lines. |
| **jest** | `jest`, `bun`, `deno` | `●` failure blocks + summary; drops `PASS` lines. |
| **pytest** | `pytest` | FAILED node IDs + AssertionError + short summary. |
| **rspec** | `rspec` | Injects `--format json`; example-level failures with message + location. |
| **rubocop** | `rubocop` | Injects `--format json`; offenses grouped by severity, capped. |
| **rake** | `rake`, `bundle` | Failure/error blocks + summary; drops passing test lines. |
| **mypy** | `mypy` | Errors grouped by file, capped at 10 per file. Injects `--no-color`. |
| **ruff** | `ruff` | Violations grouped by error code. `format`: summary line only. |
| **uv** | `uv`, `uvx` | Strips Downloading/Fetching/Preparing noise; keeps errors + summary. |
| **pip** | `pip`, `poetry`, `pdm`, `conda` | `install`: `[complete — N packages]` or already-satisfied short-circuit. |
| **python** | `python` | Traceback: keep block + final error. Long output: BERT. |
| **eslint** | `eslint` | Errors grouped by file, caps at 20 + `[+N more]`. |
| **next** | `next` | `build`: route table collapsed. `dev`: errors + ready line. |
| **playwright** | `playwright` | Failing test names + error messages; passing tests dropped. Injects `--reporter=list`. |
| **prettier** | `prettier` | `--check`: files needing formatting + count. |
| **vite** | `vite` | Asset chunk table collapsed, HMR deduplication. |
| **webpack** | `webpack` | Module resolution graph dropped; keeps assets, errors, build result. |
| **turbo** | `turbo` | Inner task output stripped; cache hit/miss per package + final summary. |
| **nx** | `nx`, `npx nx` | Passing tasks collapsed to `[N tasks passed]`; failing task output kept. |
| **stylelint** | `stylelint` | Issues grouped by file, caps at 40 + `[+N more]`. |
| **biome** | `biome` | Code context snippets stripped; keeps file:line, rule, message. |
| **kubectl** | `kubectl`, `k` | Smart column selection, log anomaly scoring, describe key sections. |
| **terraform** | `terraform`, `tofu` | `plan`: `+`/`-`/`~` + summary. `validate`: short-circuits on success. |
| **aws** | `aws`, `gcloud`, `az` | Resource extraction; `--output json` injected for read-only actions. |
| **gh** | `gh` | Compact tables for list commands; strips HTML from `pr view`. |
| **helm** | `helm` | `list`: compact table. `status`/`diff`/`template`: structured. |
| **docker** | `docker` | `logs`: ANSI strip + BERT. `ps`/`images`: formatted tables. |
| **make** | `make`, `ninja` | "Nothing to be done" short-circuit; keeps errors. Injects `--no-print-directory`. |
| **golangci-lint** | `golangci-lint` | Diagnostics grouped by file; runner noise dropped. |
| **prisma** | `prisma` | `generate`/`migrate`/`db push` structured summaries. |
| **mvn** | `mvn` | Drops `[INFO]` noise; keeps errors + reactor summary. |
| **gradle** | `gradle` | UP-TO-DATE tasks collapsed; FAILED tasks and errors kept. |
| **npm/yarn** | `npm`, `yarn` | `install`: package count; strips boilerplate. |
| **pnpm** | `pnpm` | `install`: summary; drops progress bars. |
| **brew** | `brew` | `install`/`update`: status lines + Caveats. |
| **curl** | `curl` | JSON → type schema. Non-JSON: cap 30 lines. |
| **grep / rg** | `grep`, `rg` | Compact paths, per-file 25-match cap. Injects `--no-heading --with-filename`. |
| **find** | `find` | Groups by directory, caps at 50. Injects `-maxdepth 8` if unset. |
| **journalctl** | `journalctl` | Injects `--no-pager -n 200`. BERT anomaly scoring. |
| **psql** | `psql` | Strips borders, caps at 20 rows. |
| **tree** | `tree` | Auto-injects `-I "node_modules\|.git\|target\|..."`. |
| **diff** | `diff` | `+`/`-`/`@@` + 2 context lines, max 5 hunks. |
| **jq** | `jq` | Array: schema of first element + `[N items]`. |
| **env** | `env` | Categorized sections; sensitive values redacted. |
| **ls** | `ls` | Drops noise dirs; top-3 extension summary. |
| **log** | `log` | Timestamp/UUID normalization, dedup `[×N]`, error summary block. |
| **wget** | `wget` | Injects `--quiet` if no verbosity flag set. |

</details>

<details>
<summary><strong>Pipeline architecture</strong></summary>

```
0. Hard input ceiling (200k chars — truncates before any stage)
1. Strip ANSI codes
2. Normalize whitespace
3. Global regex pre-filter (progress bars, spinners, download lines, decorators)
4. NDJSON streaming compaction (go test -json, jest JSON reporter)
5. Command-specific pattern filter
6. If over summarize_threshold_lines:
   6a. BERT noise pre-filter
   6b. Entropy-adaptive BERT summarization (up to 7 passes)
7. Hard output cap (50k chars)
```

Outputs under 15 tokens skip the pipeline entirely. Step 6b falls back to head+tail if BERT is unavailable.

**Pre-run cache** (fires before execution): git, kubectl, docker, and terraform commands are hashed against live state. A hit skips execution entirely and returns the cached output with a `[PC: cached from Xm ago]` marker.

</details>

<details>
<summary><strong>Configuration</strong></summary>

Config loaded from: `./ccr.toml` → `~/.config/ccr/config.toml` → embedded default.

```toml
[global]
summarize_threshold_lines = 50
head_lines = 30
tail_lines = 30
strip_ansi = true
normalize_whitespace = true
deduplicate_lines = true
input_char_ceiling = 200000
output_char_cap = 50000
# cost_per_million_tokens = 15.0

[tee]
enabled = true
mode = "aggressive"   # "aggressive" | "always" | "never"

[read]
mode = "auto"   # "passthrough" | "auto" | "strip" | "aggressive"

[commands.git]
patterns = [
  { regex = "^(Counting|Compressing|Receiving|Resolving) objects:.*", action = "Remove" },
]

[commands.cargo]
patterns = [
  { regex = "^\\s+Compiling \\S+ v[\\d.]+", action = "Collapse" },
  { regex = "^\\s+Downloaded \\S+ v[\\d.]+", action = "Remove"   },
]
```

Pattern actions: `Remove`, `Collapse`, `ReplaceWith = "text"`, `TruncateLinesAt = N`, `HeadLines = N`, `TailLines = N`, `MatchOutput = "msg"`, `OnEmpty = "msg"`.

Pricing uses `cost_per_million_tokens` from `ccr.toml` if set, otherwise `ANTHROPIC_MODEL` env var (Opus 4.6: $15, Sonnet 4.6: $3, Haiku 4.5: $0.80), otherwise $3.00.

</details>

<details>
<summary><strong>User-defined filters</strong></summary>

Place `filters.toml` at `.ccr/filters.toml` (project-local) or `~/.config/ccr/filters.toml` (global). Project-local overrides global for the same key. Runs before any built-in handler.

```toml
[commands.myapp]
patterns = [
  { regex = "^DEBUG:",            action = "Remove" },
  { regex = "^\\S+\\.ts\\(",     action = "TruncateLinesAt", max_chars = 120 },
]
on_empty = "(no relevant output)"

[commands.myapp.match_output]
pattern        = "Server started"
message        = "ok — server ready"
unless_pattern = "error"
```

</details>

<details>
<summary><strong>Session intelligence</strong></summary>

State tracked via `CCR_SESSION_ID=$PPID`, stored at `~/.local/share/ccr/sessions/<id>.json`.

- **Result cache** — post-pipeline bytes frozen per input hash; returned identically on repeat calls to prevent prompt cache busts.
- **Semantic delta** — repeated commands emit only new/changed lines: `[Δ from turn N: +M new, K repeated — ~T tokens saved]`.
- **Cross-turn dedup** — identical outputs (cosine > 0.92) collapse to `[same output as turn 4 (3m ago) — 1.2k tokens saved]`.
- **Elastic context** — pipeline pressure scales with session size. At >80% pressure: `[⚠ context near full — run ccr compress --scan-session]`.
- **Intent-aware query** — reads the agent's last message from the live session JSONL and uses it as the BERT query.

</details>

<details>
<summary><strong>Hook architecture</strong></summary>

| Agent | Config | Script |
|-------|--------|--------|
| Claude Code | `~/.claude/settings.json` | `~/.claude/hooks/ccr-rewrite.sh` |
| Cursor | `~/.cursor/hooks.json` | `~/.cursor/hooks/ccr-rewrite.sh` |
| Gemini CLI | `~/.gemini/hooks.json` | `~/.gemini/ccr-rewrite.sh` |
| Cline | `.clinerules` (project dir) | — (rules-based) |
| VS Code Copilot | `~/.vscode/settings.json` | `~/.vscode/extensions/.ccr-hook/ccr-rewrite.sh` |

All agents share the same binary and compression pipeline.

**PreToolUse:** known handler → rewrites to `ccr run <cmd>`; unknown → no-op; already wrapped → no double-wrap; compound commands → each segment rewritten independently.

**PostToolUse:** Bash → full pipeline; Read → BERT + session dedup; Glob → grouped by directory; Grep → compact paths.

**Hook integrity:** `ccr init` writes SHA-256 baselines (chmod 0o444). CCR verifies at every invocation and exits 1 with a warning if tampered. `ccr verify` checks all installed agents.

</details>

<details>
<summary><strong>Crate overview</strong></summary>

```
ccr/        CLI binary — handlers, hooks, session state, commands
ccr-core/   Core library (no I/O) — pipeline, BERT summarizer, config, analytics
ccr-sdk/    Conversation compression — tiered compressor, deduplicator, Ollama
ccr-eval/   Evaluation suite — fixtures against Claude API
config/     Embedded default filter patterns
```

</details>

<details>
<summary><strong>Uninstall</strong></summary>

```bash
ccr init --uninstall                        # Claude Code
ccr init --agent cursor --uninstall         # Cursor
ccr init --agent gemini --uninstall         # Gemini CLI
ccr init --agent cline --uninstall          # Cline
ccr init --agent copilot --uninstall        # VS Code Copilot

brew uninstall ccr && brew untap AssafWoo/pandafilter   # Homebrew
# or: cargo uninstall ccr

rm -rf ~/.local/share/ccr                   # analytics + sessions
rm -rf ~/.cache/huggingface/hub/models--sentence-transformers--all-MiniLM-L6-v2
```

</details>

---

## FAQ

**Does CCR degrade Claude's output quality?**
No. CCR only removes noise — build logs, module graphs, passing test lines, progress bars. Errors, file paths, and summaries are always kept.

**What about tools CCR doesn't know?**
BERT semantic routing matches against all known handlers. If confidence is high enough the closest handler applies; otherwise output passes through unchanged. CCR never silently drops output.

**How do I verify it's working?**
`ccr gain` after a session. To inspect what the model received from a specific command: `ccr proxy git log --oneline -20`.

**Does CCR send any data outside my machine?**
Never. All processing is fully local. BERT runs on-device.

---

## Contributing

Open an issue or PR on [GitHub](https://github.com/AssafWoo/PandaFilter). To add a handler: implement the `Handler` trait and register it in `ccr/src/handlers/mod.rs` — see `git.rs` as a template.

---

## License

MIT — see [LICENSE](LICENSE).
