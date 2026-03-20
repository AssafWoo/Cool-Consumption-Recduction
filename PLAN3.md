# PLAN3.md — CCR Next Features: IX, RH, PC, NL

Four features that extend CCR beyond per-command compression into session intelligence,
bidirectional compression, pre-execution caching, and adaptive noise suppression.

---

## Implementation Order (TL;DR)

1. **NL** — Cross-session Noise Learning (purely additive, no dependencies)
2. **RH** — Read/Glob Hook Compression (extends existing hook dispatch)
3. **IX** — Intent Extraction (depends on hook refactor from RH)
4. **PC** — Pre-run Cache (modifies run path; implement after others are stable)

Shared prerequisite: extract `sha256_hex(s: &str) -> String` into `ccr/src/util.rs`
before starting IX/PC. Add `sha2 = "0.10"` to `ccr/Cargo.toml` once.

---

## Feature 1: Intent Extraction (IX)

### Overview

The PostToolUse hook currently uses the first word of the Bash command (`"cargo"`) as the
BERT query. Claude's actual intent is in its last assistant message, written in real time
to `~/.claude/projects/<project-hash>/<session>.jsonl`. Replacing the shallow command
word with Claude's natural-language intent ("trace where the memory leak occurs in the
connection pool") makes BERT scoring dramatically more relevant — lines kept are the ones
that answer what Claude actually asked.

### Implementation Steps

#### Step A — Create `ccr/src/intent.rs`

```rust
pub fn extract_intent(session_id: &str) -> Option<String>
```

1. **Locate the JSONL file** *(confirmed by inspecting live sessions)*
   - Project dir: cwd with every `/` replaced by `-`.
     e.g. `/Users/foo/Desktop/ccr` → `-Users-foo-Desktop-ccr`
   - Full path: `dirs::home_dir()?.join(".claude/projects").join(project_dir)`
   - Session file: UUID-based name (e.g. `1b0db1b2-906f-45cf-b4a1-98196fe8ee6c.jsonl`).
     **Not PPID**. Strategy: list all `.jsonl` files in the project dir, sort by mtime
     descending, pick the newest one.

2. **Read the tail efficiently**
   - Open file, seek to `max(0, file_len - 16_384)` bytes from end (16 KB tail).
   - Read forward from that offset, split on newlines.

3. **Parse entries defensively** *(confirmed schema)*
   - Each line: `serde_json::from_str::<serde_json::Value>`. Skip failures.
   - Match on `entry["type"] == "assistant"`.
   - `entry["message"]["content"]` is an array of blocks.
   - Find blocks where `block["type"] == "text"`, take `block["text"]`.
   - Skip blocks where `block["type"] == "thinking"` (extended thinking — not useful as query).

4. **Extract query string**
   - Take the last matching assistant entry.
   - Strip markdown (`**`, `*`, `` ` ``, `#`, `>`).
   - Truncate at first sentence boundary (`.`, `?`, `!`) within 256 chars.
   - Return `Some(text)` or `None` on any failure.

5. **Error contract**: every failure returns `None` silently. Zero panics, zero stderr
   (hook output must be clean JSON).

#### Step B — Add sha256 helper

`fn sha256_hex(s: &str) -> String` in `ccr/src/util.rs`. Use `sha2` crate.
Both IX and PC share this function — add it once.

#### Step C — Wire into `hook.rs`

After the existing `command_hint` derivation, add:

```rust
let intent_query: Option<String> = crate::intent::extract_intent(&sid);
```

Replace the existing `query` derivation with:

```rust
let query = intent_query.or_else(|| {
    hook_input.tool_input.get("command")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
});
```

Intent when available, command word as fallback.

#### Step D — Register module

Add `mod intent;` to `ccr/src/main.rs`.

### New Files

- `ccr/src/intent.rs`
- `ccr/src/util.rs` (shared sha256_hex; also used by PC)

### Modified Files

- `ccr/src/main.rs`: add `mod intent;`, `mod util;`
- `ccr/src/hook.rs`: replace query derivation
- `ccr/Cargo.toml`: add `sha2 = "0.10"`

### Tests

**Unit tests in `ccr/src/intent.rs` (`#[cfg(test)]`)**

| Test | Asserts |
|------|---------|
| `returns_none_when_file_missing` | `extract_intent("nonexistent_99999")` returns `None` without panic |
| `parses_simple_assistant_message` | JSONL with one assistant entry → `Some(s)` starting with expected text |
| `uses_last_assistant_message` | Two entries ("old", "new") → returns "new" |
| `strips_markdown` | Input `"**Run** the \`cargo\` command"` → output contains neither `**` nor backtick |
| `truncates_to_256_chars` | 500-char content with no sentence boundary → len ≤ 256 |
| `truncates_at_sentence_boundary` | `"First sentence. Second long..."` → ends with `"First sentence."` |
| `sha256_hex_is_deterministic` | Same input → same 64-char hex output |

**Integration test `ccr/tests/intent_integration.rs`**

| Test | Asserts |
|------|---------|
| `hook_falls_back_when_no_jsonl` | With no live Claude session file, hook uses command-word query |

### Potential Issues

- **JSONL schema instability**: Claude Code's format is not a public API. Try multiple field
  paths; return `None` on mismatch. Never hard-code a single path.
- **Race condition**: Claude writes while we read. Only parse complete lines (skip
  `serde_json::from_str` failures).
- **Session ID mismatch**: `$PPID` may not match the JSONL filename. Fallback to newest
  file by mtime is the safety net — test this empirically before shipping.
- **Performance**: tail read must be < 5 ms. Profile with a 10 MB JSONL. Reduce tail
  window or cache the last-read offset in session state if too slow.
- **`dirs::home_dir()` returns `None`**: in some CI environments. Return `None` — safe.

### End-to-End Validation

1. Add `eprintln!("[IX] intent={:?}", intent_query)` temporarily to `hook.rs`.
2. Run a Claude Code session: ask "trace the memory leak in the connection pool", then
   let Claude run a Bash command. Redirect hook stderr to a file.
3. Verify the intent string is Claude's actual sentence, not `"cargo"`.
4. Remove debug line. Compare BERT savings with/without IX on identical command sequences.
5. Rename the JSONL file. Verify hook still works (falls back gracefully).

---

## Feature 2: Read/Glob Hook Compression (RH)

### Overview

CCR's PostToolUse hook only compresses Bash outputs. But Read (file contents) and Glob
(file listings) tool outputs also flow into context and can be large — a 2000-line source
file or 500-path Glob wastes thousands of tokens. Extending the hook to intercept these
tool types applies CCR's compression to two more high-volume entry points, with
purpose-built strategies: BERT pipeline for Read, ls-handler-style truncation for Glob.

### Implementation Steps

#### Step A — Refactor `hook.rs` dispatch

Extract the current `run()` body (after parsing) into `fn process_bash(...)`. Add:

```rust
match hook_input.tool_name.as_str() {
    "Bash" => process_bash(&hook_input, &config, &mut session),
    "Read" => process_read(&hook_input, &config, &mut session),
    "Glob" => process_glob(&hook_input, &config, &mut session),
    _ => return Ok(()),
}
```

All three functions share the same session and config — load both before dispatch.

Each function returns `Option<String>` (the compressed output, or `None` for passthrough).
The outer `run()` serialises the `HookOutput` if `Some`.

Remove `#[allow(dead_code)]` from `tool_name` in `HookInput`.

#### Step B — `fn process_read(...)`

1. `file_path` from `hook_input.tool_input["file_path"]`.
2. `command_hint`: file extension (`.rs`, `.py`, `.json`, etc.).
3. `query`: `intent::extract_intent(&sid)` → fallback to filename without extension.
4. Session dedup: look up `file_path` as `cmd_key` in session. If found, run
   `session.compute_delta(&file_path, &lines, &emb)`.
5. Run `pipeline.process(text, extension_hint, query, historical_centroid)`.
6. Apply session-aware passes (delta, C1 dedup, C2 budget) with `file_path` as `cmd_key`.
7. Record in session with `is_state = false`.
8. Binary file guard: if output contains null bytes or > 30% non-UTF8 sequences,
   return `None` (passthrough unchanged).

#### Step C — `fn process_glob(...)`

1. `pattern` from `hook_input.tool_input["pattern"]`, optional `path` from `hook_input.tool_input["path"]`.
2. Group paths by parent directory.
3. Show at most 60 entries total, summarise excess as `[+N more in path/to/dir/]`.
4. Append summary: `[Glob: <pattern> — N paths total]`.
5. No BERT needed. Session dedup on exact path-list hash:
   - Hash the full path list with sha256.
   - If same hash seen this session under `glob:<pattern>`, emit
     `[same glob result as turn N — N paths]`.
6. Record under `cmd_key = format!("glob:{}", pattern)`.

#### Step D — Update `ccr init` hook registration

In `main.rs` `init()` function, add two more `merge_hook` calls *(confirmed: settings.json
uses separate objects per matcher, not a single entry for all tool types)*:

```rust
merge_hook(&mut settings, "PostToolUse", "Read",  &ccr_hook_cmd);
merge_hook(&mut settings, "PostToolUse", "Glob",  &ccr_hook_cmd);
```

This adds two new entries alongside the existing `matcher: "Bash"` entry.

### New Files

None. All logic in existing files.

### Modified Files

- `ccr/src/hook.rs`: dispatch + `process_read` + `process_glob`
- `ccr/src/main.rs`: two additional `merge_hook` calls in `init()`

### Tests

**Unit/integration tests in `ccr/tests/hook_read_glob.rs`**

| Test | Asserts |
|------|---------|
| `process_read_passthrough_short_file` | 50-line input → output equals input |
| `process_read_compresses_large_file` | 600-line input → output line count < 600 |
| `process_read_preserves_error_lines` | 600 lines + `"error: undefined variable"` → error line present |
| `process_read_passthrough_binary` | Input with null bytes → output unchanged |
| `process_glob_truncates_at_60` | 200 paths → output < 80 lines |
| `process_glob_summary_line` | 10 paths, pattern `src/**/*.rs` → last line contains "10 paths total" |
| `process_glob_dedup_same_session` | Same pattern+output twice → second call returns `[same glob result]` |
| `tool_name_read_dispatches` | HookInput with `tool_name: "Read"` → non-empty response |
| `tool_name_unknown_passthrough` | HookInput with `tool_name: "Write"` → `run()` returns Ok, prints nothing |

### Potential Issues

- **Hook output schema for Read/Glob**: verify Claude Code expects the same
  `{"output": "..."}` JSON for all tool types before assuming. Check Claude Code docs.
- **Binary files**: detect null bytes; return unchanged. Never pass binary through BERT.
- **Short Glob results** (0–1 paths): do not compress; return as-is.
- **`file_path` as cmd_key collision**: always use the full path, never just basename.
- **`ccr init` idempotence**: `merge_hook` already deduplicates — safe to re-run.

### End-to-End Validation

1. Run `ccr init`. Inspect `~/.claude/settings.json` — verify three PostToolUse entries
   exist (Bash, Read, Glob).
2. In a Claude Code session, ask Claude to read a 500+ line file. Observe compressed
   output in the tool result.
3. Ask Claude to Glob `**/*.rs` in a large repo. Observe truncated listing with summary.
4. Read the same file twice in one session. Second result shows delta/dedup marker.
5. Run `ccr gain` — verify Read and Glob entries appear in analytics.

---

## Feature 3: Pre-run Cache (PC)

### Overview

The existing B3 semantic cache runs *after* command execution. For state commands like
`git status`, this means the command always executes even when the repo has not changed.
A structural cache key computed *before* execution (git HEAD hash + staged+unstaged diff
hashes for git) skips execution entirely when state is unchanged — saving both execution
latency and output tokens. This only applies to the `ccr run` path (the hook path cannot
prevent execution since the command already ran).

### Implementation Steps

#### Step A — Create `ccr/src/pre_cache.rs`

```rust
pub struct PreCacheKey {
    pub key: String,   // 16-char hex, stable when state unchanged
    pub cmd: String,   // "git status", etc.
}

pub struct PreCacheEntry {
    pub key: String,
    pub output: String,
    pub ts: u64,
    pub tokens: usize,
}

pub struct PreCache {
    entries: HashMap<String, PreCacheEntry>,  // keyed on cmd string
}
```

Storage: `~/.local/share/ccr/pre_cache/<session_id>.json`

Load/save pattern mirrors `SessionState`.

**Methods**:

- `pub fn compute_key(args: &[String]) -> Option<PreCacheKey>`
  - Dispatches on `args[0]`.
  - For `git`: run two plumbing commands (see Step B), combine hashes.
  - For `kubectl`/`docker`: hash kubeconfig/dockerd socket mtime.
  - All others: `None`.

- `pub fn lookup(&self, key: &PreCacheKey) -> Option<&PreCacheEntry>`
  - Look up by `cmd`; return entry only if `entry.key == key.key`.

- `pub fn insert(&mut self, key: PreCacheKey, output: &str, tokens: usize)`

- `pub fn evict_old(&mut self)` — remove entries where `ts < now - 3600`.

#### Step B — Git cache key computation

Only cache read-only git subcommands:

```rust
match subcmd {
    "status" | "diff" | "log" | "branch" | "stash" => { /* continue */ }
    _ => return None,
}
```

Two sub-processes (both < 2 ms):

```
git rev-parse HEAD                    → HEAD hash
git diff-index --cached --stat HEAD   → staged summary (not full diff)
git diff-files --stat                 → unstaged summary
```

Concatenate all three, sha256, take first 16 chars → cache key.

Fall through to `None` on any subprocess failure (detached HEAD, no commits, non-git dir).

#### Step C — Wire into `cmd/run.rs`

**Before execution**, after `delta_key` derivation:

```rust
let pre_cache = crate::pre_cache::PreCache::load(&crate::session::session_id());
let pre_cache_key = crate::pre_cache::PreCache::compute_key(&args);

if let Some(ref pck) = pre_cache_key {
    if let Some(entry) = pre_cache.lookup(pck) {
        let age = crate::session::format_age(now_unix() - entry.ts);
        let mut out = entry.output.clone();
        out.push_str(&format!(
            "\n[PC: cached from {} ago — ~{} tokens saved; key {}]",
            age, entry.tokens, &pck.key[..8]
        ));
        print!("{}", out);
        if !out.ends_with('\n') { println!(); }
        append_analytics(/* input_tokens=entry.tokens, output_tokens=0, duration_ms=0 */);
        return Ok(());
    }
}
```

**After pipeline** (just before B3 session cache check):

```rust
if let Some(ref pck) = pre_cache_key {
    let tokens = ccr_core::tokens::count_tokens(&filtered);
    let mut pc = crate::pre_cache::PreCache::load(&sid);
    pc.evict_old();
    pc.insert(pck.clone(), &filtered, tokens);
    pc.save(&sid);
}
```

#### Step D — Config (future)

For MVP, hardcode `enabled = true` and `ttl_secs = 3600`.
Add `[pre_cache]` section to `CcrConfig` in a follow-up once behaviour is validated.

### New Files

- `ccr/src/pre_cache.rs`

### Modified Files

- `ccr/src/main.rs`: add `mod pre_cache;`
- `ccr/src/cmd/run.rs`: pre-cache check + write-through
- `ccr/Cargo.toml`: `sha2 = "0.10"` (if not already added for IX)

### Tests

**Unit tests in `ccr/src/pre_cache.rs` (`#[cfg(test)]`)**

| Test | Asserts |
|------|---------|
| `compute_key_returns_none_for_unknown_cmd` | `compute_key(&["python3","main.py"])` → `None` |
| `compute_key_returns_none_for_git_push` | `git push` is not cacheable → `None` |
| `lookup_returns_none_on_key_mismatch` | Insert key "aabb", lookup "eeff" → `None` |
| `lookup_returns_entry_on_key_match` | Insert + lookup same key → `Some` with correct output |
| `evict_old_removes_stale` | Entry with `ts = now - 7200` → gone after `evict_old()` |
| `evict_old_keeps_fresh` | Entry with `ts = now - 30` → present after `evict_old()` |
| `insert_overwrites_existing_cmd` | Insert "git status" twice → lookup returns newest output |

**Integration test `ccr/tests/pre_cache_integration.rs`**

| Test | Asserts |
|------|---------|
| `run_git_status_cache_hit_skips_execution` | With temp git repo: first run normal, second run returns `[PC: cached` marker without executing git |

### Potential Issues

- **Detached HEAD / fresh repo**: `git rev-parse HEAD` fails → `compute_key` returns `None`
  → falls through to normal execution. Must not crash.
- **Unstaged changes missed**: use both `diff-index --cached --stat` AND `diff-files --stat`
  to cover staged and unstaged state. Otherwise, editing a file without staging would return
  stale cached `git status`.
- **Large staged diffs**: use `--stat` (summary only) not the full diff output.
  Avoids reading megabytes just to compute a hash.
- **Race condition**: file modified between key computation and command execution.
  Acceptable — TTL bounds the staleness.
- **Analytics double-counting**: cache hits record `input_tokens = entry.tokens`,
  `output_tokens = 0`, `duration_ms = 0`. This accurately reflects the savings.
- **`append_analytics` scope**: keep analytics calls in `cmd/run.rs`; do not move into
  `pre_cache.rs`.

### End-to-End Validation

1. `ccr run git status` in a git repo → normal output.
2. Immediately again (no changes) → `[PC: cached from Ns ago]` appended.
3. `git add somefile` → `ccr run git status` → fresh output (key changed).
4. `ccr gain` → second run shows 100% savings, 0 ms duration.
5. Wait 70 minutes. `ccr run git status` → fresh output (TTL expired).

---

## Feature 4: Cross-session Noise Learning (NL)

### Overview

Every project has lines that are always noise: progress bars, version download lines,
`Already up to date`. CCR currently re-processes these through BERT on every run. Learning
which lines are reliably suppressed across many invocations lets CCR pre-filter them before
BERT, reducing latency and improving precision. Patterns are promoted after 10 suppressions
at ≥ 90% rate, stored per-project, and evicted after 30 days of inactivity.

### Implementation Steps

#### Step A — Create `ccr/src/noise_learner.rs`

```rust
#[derive(Serialize, Deserialize, Clone)]
pub struct NoisePattern {
    pub pattern: String,   // normalized line
    pub count: u32,        // times seen
    pub last_seen: u64,    // unix timestamp
    pub suppressed: u32,   // times removed by pipeline
    pub promoted: bool,    // true when permanently active
}

#[derive(Serialize, Deserialize, Default)]
pub struct NoiseStore {
    pub patterns: HashMap<String, NoisePattern>,
}
```

Storage: `~/.local/share/ccr/projects/<project_key>/noise.json`

**Project key derivation** (public utility, shared with IX):
1. `git remote get-url origin` → `sha256_hex(url)[..16]`
2. Fallback: `sha256_hex(cwd_string)[..16]`

**Methods**:

- `pub fn path_for_project(key: &str) -> Option<PathBuf>`
- `pub fn load(project_key: &str) -> Self`
- `pub fn save(&self, project_key: &str)` — atomic write via `.tmp` + rename
- `pub fn record_lines(&mut self, input_lines: &[&str], output_lines: &[&str])`
  - For each `input_lines` line: increment `count`, update `last_seen`.
  - If not in `output_lines`: increment `suppressed`.
  - Use `normalize_line()` as the map key.
- `pub fn apply_pre_filter<'a>(&self, lines: &[&'a str]) -> Vec<&'a str>`
  - Remove lines that are promoted AND do NOT match the critical safety regex.
  - Safety regex: `error|warning|failed|fatal|panic|exception|critical` (compiled once
    with `once_cell::sync::Lazy`).
- `pub fn promote_eligible(&mut self)`
  - For each pattern: `count >= 10 && suppressed/count >= 0.90 && !promoted`
    → set `promoted = true`.
- `pub fn evict_stale(&mut self, now: u64)`
  - Remove entries where `last_seen < now - 30 * 86400`.
- `pub fn project_key() -> Option<String>`

**Normalization** (`fn normalize_line(line: &str) -> String`):
- Trim + lowercase.
- Collapse sequences of `=`, `-`, `>`, `<`, `|`, `[`, `]`, space (4+ chars) to
  `[progress]`. This groups all progress-bar variants.
- Used as the HashMap key — not stored as the display pattern.

**Pattern explosion guard**: if `patterns.len() >= 10_000`, stop inserting new entries
(still update existing ones). This bounds memory in pathological cases.

**Atomic save**:
```rust
let tmp = path.with_extension("tmp");
std::fs::write(&tmp, json)?;
std::fs::rename(&tmp, &path)?;
```

#### Step B — Wire into `hook.rs` (`process_bash`)

Load the store once at the top of `process_bash`. Apply two passes:

**Before pipeline**:
```rust
let project_key = crate::noise_learner::NoiseStore::project_key();
let noise_store = project_key.as_ref()
    .map(|k| crate::noise_learner::NoiseStore::load(k));

let output_text = if let Some(ref store) = noise_store {
    let lines: Vec<&str> = raw_output_text.lines().collect();
    let kept = store.apply_pre_filter(&lines);
    if kept.len() < lines.len() { kept.join("\n") } else { raw_output_text.clone() }
} else {
    raw_output_text.clone()
};
```

**After pipeline** (after `result.output` is produced):
```rust
if let (Some(ref key), Some(mut store)) = (&project_key, noise_store) {
    let input_lines: Vec<&str> = raw_output_text.lines().collect();
    let output_lines: Vec<&str> = result.output.lines().collect();
    store.record_lines(&input_lines, &output_lines);
    store.promote_eligible();
    store.evict_stale(now_unix());
    store.save(key);
}
```

Wire identically into `cmd/run.rs` for the `ccr run` path.

#### Step C — `ccr noise` command

Add to `Commands` enum in `main.rs`:

```rust
/// Show or reset learned noise patterns for the current project
Noise {
    #[arg(long)]
    reset: bool,
}
```

Create `ccr/src/cmd/noise.rs` with `pub fn run(reset: bool) -> anyhow::Result<()>`:

- `--reset`: delete `noise.json` for current project, print confirmation.
- Default: print table of patterns sorted by count descending:
  ```
  Learned noise patterns for project a1b2c3d4 (47 total):
  count  suppr  rate%    status       pattern
  ─────────────────────────────────────────────────────────────────────
  142    138    97.2     promoted     downloading [progress] 100%
  89     81     91.0     promoted     finished in 0.00s
  23     18     78.3     learning     already up to date
  ```

### New Files

- `ccr/src/noise_learner.rs`
- `ccr/src/cmd/noise.rs`

### Modified Files

- `ccr/src/main.rs`: `mod noise_learner;`, `Noise` command variant, match arm
- `ccr/src/cmd/mod.rs`: `pub mod noise;`
- `ccr/src/hook.rs`: pre-filter + learning calls in `process_bash`
- `ccr/src/cmd/run.rs`: pre-filter + learning calls

### Tests

**Unit tests in `ccr/src/noise_learner.rs` (`#[cfg(test)]`)**

| Test | Asserts |
|------|---------|
| `record_lines_increments_count` | `["foo","bar"]` in, `["foo"]` out → `bar.suppressed==1`, `foo.suppressed==0` |
| `promote_eligible_after_threshold` | `count=10, suppressed=10` → `promoted=true` |
| `no_promote_below_threshold` | `count=10, suppressed=8` (80%) → `promoted=false` |
| `pre_filter_removes_promoted` | Promoted "downloading packages" → removed from output |
| `pre_filter_keeps_error_lines` | Promoted "error: something" → kept (safety guard) |
| `evict_stale_removes_old` | `last_seen = now - 40*86400` → evicted |
| `evict_stale_keeps_recent` | `last_seen = now - 5*86400` → kept |
| `normalize_collapses_progress_bar` | `"Downloading [=====>  ] 80%"` → contains `[progress]` |
| `normalize_lowercases` | `"Compiling Foo v1.0"` → all lowercase |
| `project_key_is_deterministic` | Same dir → same result twice |
| `pattern_explosion_guard` | 10,001 distinct lines → map size stays ≤ 10,000 |
| `atomic_save_no_corruption` | Simulate write failure mid-save → old file intact |

**Integration tests in `ccr/tests/noise_integration.rs`**

| Test | Asserts |
|------|---------|
| `noise_cmd_prints_header_when_empty` | `cmd::noise::run(false)` in clean temp dir → "No noise patterns" |
| `noise_reset_deletes_file` | Create noise.json, run `noise::run(true)` → file gone |
| `pipeline_skips_promoted_lines` | Simulate 10 runs suppressing same line → 11th run output is shorter |

### Potential Issues

- **Pattern explosion**: guard with `patterns.len() >= 10_000` before insert.
- **Progress-bar normalization over-aggressive**: limit normalization to lines that are
  *entirely* progress-bar characters with no other content. Do not normalize lines that
  have meaningful text mixed with separator chars.
- **Literal storage (not regex)**: deliberate design choice — avoids regex injection risk.
  Normalization handles variability. Do not switch to regex without considering injection.
- **Concurrent writes**: always use atomic write (`.tmp` + `rename`).
- **Non-git directories**: `project_key()` falls back to cwd hash. Covered in tests.
- **Safety guard must be first check** in `apply_pre_filter` — before promoted pattern
  check. A line matching the critical regex is never suppressed, regardless of promotion.

### End-to-End Validation

1. Run `ccr run cargo build` 15+ times in a Rust project.
2. After each run, inspect `noise.json` — verify repeated suppressed lines accumulate
   `count` and `suppressed` fields.
3. Run `ccr noise` — verify table shows `promoted` status for lines suppressed ≥ 90%.
4. Next `ccr run cargo build` → output is shorter than first run (promoted lines
   pre-filtered before BERT).
5. Run `ccr noise --reset` → table is empty.
6. Manually construct a session where "error: out of memory" is promoted. Run
   `apply_pre_filter` with that line. Assert it is NOT suppressed.

---

## Shared Infrastructure

### `ccr/src/util.rs` (create before starting)

```rust
use sha2::{Sha256, Digest};

pub fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}
```

Both IX and PC use this. NL's `project_key()` also calls it.

### `ccr/Cargo.toml` — add once

```toml
sha2 = "0.10"
```

### Project key — single source of truth

`NoiseStore::project_key()` in `noise_learner.rs` is the canonical implementation.
Intent extraction in `intent.rs` calls `crate::noise_learner::NoiseStore::project_key()`
rather than reimplementing cwd hashing independently.

---

## Risk Summary

| Feature | Highest Risk | Mitigation |
|---------|-------------|------------|
| IX | JSONL schema changes | Defensive multi-path parsing; return None on any failure |
| RH | Read hook output schema | Verify Claude Code docs for non-Bash PostToolUse format |
| PC | Stale git key | Include both staged AND unstaged hashes in key computation |
| NL | False suppression | Safety regex always checked before promotion applies |
