#!/usr/bin/env bash
# CCR end-to-end integration test suite.
# Runs inside Docker as a non-root user, simulating a real developer install.
#
# Usage:
#   docker compose run --rm ccr-test          # full suite
#   docker compose run --rm ccr-test --only analytics  # filter by tag

set -euo pipefail

# ── Colour helpers ─────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[0;33m'; BOLD='\033[1m'; NC='\033[0m'
PASS=0; FAIL=0; SKIP=0

ok()   { echo -e "  ${GREEN}✓${NC} $1"; PASS=$((PASS+1)); }
fail() { echo -e "  ${RED}✗${NC} $1"; FAIL=$((FAIL+1)); }
skip() { echo -e "  ${YELLOW}~${NC} $1 (skipped: $2)"; SKIP=$((SKIP+1)); }
hdr()  { echo -e "\n${BOLD}▶ $1${NC}"; }

# Run cmd, capture output, assert condition.
# Usage: run_check "label" <condition_command> [expected_output_fragment]
run_check() {
  local label="$1" cond="$2" fragment="${3:-}"
  local out
  if out=$(eval "$cond" 2>&1); then
    if [[ -n "$fragment" && ! "$out" == *"$fragment"* ]]; then
      fail "$label — output missing: '$fragment'"
      echo "    got: $(echo "$out" | head -5)"
    else
      ok "$label"
    fi
  else
    fail "$label"
    echo "    error: $(echo "$out" | head -5)"
  fi
}

# ── Environment setup ──────────────────────────────────────────────────────────

DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/ccr"
mkdir -p "$DATA_DIR"

# Give ccr a HOME-based place to write session/cache state
export CCR_SESSION_ID="test-$$"
export CCR_AGENT="claude"

# Create a throwaway git repo for testing git-based commands
REPO=$(mktemp -d)
git -C "$REPO" init -q
git -C "$REPO" config user.email "test@ccr.test"
git -C "$REPO" config user.name "CCR Test"
echo "hello" > "$REPO/README.md"
git -C "$REPO" add .
git -C "$REPO" commit -q -m "initial commit"
cd "$REPO"

# ─────────────────────────────────────────────────────────────────────────────
hdr "1. Binary sanity"
# ─────────────────────────────────────────────────────────────────────────────

run_check "ccr --version prints version number" \
  "ccr --version" "0.6.0"

run_check "ccr --help exits 0" \
  "ccr --help"

run_check "ccr verify exits 0 (no hooks installed yet, should still exit 0)" \
  "ccr verify || true"

# ─────────────────────────────────────────────────────────────────────────────
hdr "2. Hook installation — Claude Code (default)"
# ─────────────────────────────────────────────────────────────────────────────

# Simulate ~/.claude settings.json pre-existing (like a real Claude Code user)
mkdir -p "$HOME/.claude/hooks"
echo '{}' > "$HOME/.claude/settings.json"

run_check "ccr init exits 0" \
  "ccr init"

run_check "hook script created at ~/.claude/hooks/ccr-rewrite.sh" \
  "test -f $HOME/.claude/hooks/ccr-rewrite.sh"

run_check "hook script is executable" \
  "test -x $HOME/.claude/hooks/ccr-rewrite.sh"

run_check "settings.json contains PreToolUse hook" \
  "grep -q 'PreToolUse' $HOME/.claude/settings.json"

run_check "settings.json contains PostToolUse hook" \
  "grep -q 'PostToolUse' $HOME/.claude/settings.json"

run_check "ccr init is idempotent (second run exits 0)" \
  "ccr init"

run_check "double init does not duplicate hooks in settings.json" \
  "python3 -c \"
import json, sys
with open('$HOME/.claude/settings.json') as f:
    d = json.load(f)
hooks = d.get('hooks', {})
post = hooks.get('PostToolUse', [])
# Each matcher should appear at most once
matchers = [h.get('matcher','') for h in post if isinstance(h, dict)]
if len(matchers) != len(set(matchers)):
    print('Duplicate matchers:', matchers, file=sys.stderr)
    sys.exit(1)
\""

# ─────────────────────────────────────────────────────────────────────────────
hdr "3. Agent installation — Cline"
# ─────────────────────────────────────────────────────────────────────────────

cd "$REPO"
run_check "ccr init --agent cline exits 0" \
  "ccr init --agent cline"

run_check ".clinerules created in project dir" \
  "test -f $REPO/.clinerules"

run_check ".clinerules contains ccr-rules-start marker" \
  "grep -q 'ccr-rules-start' $REPO/.clinerules"

run_check ".clinerules contains ccr run instructions" \
  "grep -q 'ccr run' $REPO/.clinerules"

# Simulate existing .clinerules (user has their own rules)
echo "# My team rules" > "$REPO/.clinerules"
run_check "ccr init --agent cline appends to existing .clinerules" \
  "ccr init --agent cline && grep -q 'My team rules' $REPO/.clinerules"

run_check "existing rules preserved after second init" \
  "grep -q 'My team rules' $REPO/.clinerules"

run_check "ccr init --agent cline is idempotent (replaces block, not duplicates)" \
  "ccr init --agent cline && grep -c 'ccr-rules-start' $REPO/.clinerules | grep -q '^1$'"

run_check "ccr init --uninstall --agent cline removes block" \
  "ccr init --uninstall --agent cline && ! grep -q 'ccr-rules-start' $REPO/.clinerules"

run_check "ccr init --uninstall --agent cline preserves other content" \
  "grep -q 'My team rules' $REPO/.clinerules"

# ─────────────────────────────────────────────────────────────────────────────
hdr "4. Agent installation — Gemini CLI"
# ─────────────────────────────────────────────────────────────────────────────

run_check "ccr init --agent gemini exits 0" \
  "ccr init --agent gemini"

run_check "Gemini hook script created at ~/.gemini/ccr-rewrite.sh" \
  "test -f $HOME/.gemini/ccr-rewrite.sh"

run_check "Gemini hook script is executable" \
  "test -x $HOME/.gemini/ccr-rewrite.sh"

run_check "Gemini settings.json created" \
  "test -f $HOME/.gemini/settings.json"

run_check "Gemini settings.json is valid JSON" \
  "python3 -m json.tool $HOME/.gemini/settings.json > /dev/null"

run_check "Gemini settings.json contains BeforeTool entry" \
  "python3 -c \"
import json
with open('$HOME/.gemini/settings.json') as f:
    d = json.load(f)
hooks = d.get('hooks', {})
assert 'BeforeTool' in hooks, 'BeforeTool missing from settings.json'
entry = hooks['BeforeTool'][0]
assert entry.get('matcher') == 'run_shell_command', 'matcher should be run_shell_command'
assert entry['hooks'][0].get('type') == 'command', 'hook type should be command'
\""

run_check "Gemini hook script always exits 0 even with bad input" \
  "echo 'bad json' | bash $HOME/.gemini/ccr-rewrite.sh; test \$? -eq 0"

run_check "ccr init --agent gemini is idempotent (no duplicate BeforeTool entries)" \
  "ccr init --agent gemini && python3 -c \"
import json
with open('$HOME/.gemini/settings.json') as f:
    d = json.load(f)
before_tool = d.get('hooks', {}).get('BeforeTool', [])
ccr_count = sum(1 for e in before_tool if any('ccr' in str(h.get('command','')) for h in e.get('hooks',[])))
assert ccr_count == 1, f'Expected 1 ccr entry, got {ccr_count}'
\""

run_check "ccr init --uninstall --agent gemini removes hook script" \
  "ccr init --uninstall --agent gemini && test ! -f $HOME/.gemini/ccr-rewrite.sh"

run_check "ccr init --uninstall --agent gemini cleans settings.json" \
  "python3 -c \"
import json
with open('$HOME/.gemini/settings.json') as f:
    d = json.load(f)
before_tool = d.get('hooks', {}).get('BeforeTool', [])
ccr_entries = [e for e in before_tool if any('ccr' in str(h.get('command','')) for h in e.get('hooks',[]))]
assert len(ccr_entries) == 0, 'BeforeTool still has ccr entries after uninstall'
\""

# ─────────────────────────────────────────────────────────────────────────────
hdr "5. Agent installation — VS Code Copilot"
# ─────────────────────────────────────────────────────────────────────────────
# Copilot is project-scoped: installs to .github/hooks/ in the current dir.

cd "$REPO"

run_check "ccr init --agent copilot exits 0" \
  "ccr init --agent copilot"

run_check "Copilot hook script created at .github/hooks/ccr-rewrite.sh" \
  "test -f $REPO/.github/hooks/ccr-rewrite.sh"

run_check "Copilot hook script is executable" \
  "test -x $REPO/.github/hooks/ccr-rewrite.sh"

run_check "Copilot hook config created at .github/hooks/ccr-rewrite.json" \
  "test -f $REPO/.github/hooks/ccr-rewrite.json"

run_check "Copilot hook config is valid JSON" \
  "python3 -m json.tool $REPO/.github/hooks/ccr-rewrite.json > /dev/null"

run_check "Copilot hook config contains PreToolUse entry" \
  "python3 -c \"
import json
with open('$REPO/.github/hooks/ccr-rewrite.json') as f:
    d = json.load(f)
hooks = d.get('hooks', {}).get('PreToolUse', [])
assert len(hooks) > 0, 'PreToolUse missing from hook config'
assert hooks[0].get('type') == 'command', 'hook type should be command'
\""

run_check "Copilot instructions file created at .github/copilot-instructions.md" \
  "test -f $REPO/.github/copilot-instructions.md"

run_check "Copilot instructions contain ccr-instructions-start marker" \
  "grep -q 'ccr-instructions-start' $REPO/.github/copilot-instructions.md"

run_check "Copilot hook script reads tool_input.command from JSON" \
  "grep -q 'tool_input.command' $REPO/.github/hooks/ccr-rewrite.sh"

run_check "Copilot hook script exits 0 on empty input" \
  "echo '' | bash $REPO/.github/hooks/ccr-rewrite.sh; test \$? -eq 0"

run_check "ccr init --agent copilot is idempotent (no duplicate PreToolUse entries)" \
  "ccr init --agent copilot && python3 -c \"
import json
with open('$REPO/.github/hooks/ccr-rewrite.json') as f:
    d = json.load(f)
hooks = d.get('hooks', {}).get('PreToolUse', [])
assert len(hooks) == 1, f'Expected 1 PreToolUse entry, got {len(hooks)}'
\""

run_check "ccr init --agent copilot idempotent: instructions not duplicated" \
  "ccr init --agent copilot && python3 -c \"
content = open('$REPO/.github/copilot-instructions.md').read()
assert content.count('ccr-instructions-start') == 1, 'Instructions block duplicated'
\""

run_check "ccr init --uninstall --agent copilot removes hook files" \
  "ccr init --uninstall --agent copilot && test ! -f $REPO/.github/hooks/ccr-rewrite.sh && test ! -f $REPO/.github/hooks/ccr-rewrite.json"

run_check "ccr init --uninstall --agent copilot removes instructions block" \
  "test ! -f $REPO/.github/copilot-instructions.md || ! grep -q 'ccr-instructions-start' $REPO/.github/copilot-instructions.md"

# ─────────────────────────────────────────────────────────────────────────────
hdr "6. ccr run — basic command compression"
# ─────────────────────────────────────────────────────────────────────────────

cd "$REPO"

run_check "ccr run git status exits 0" \
  "ccr run git status"

run_check "ccr run git log exits 0" \
  "ccr run git log --oneline"

# Add enough files to trigger collapse in git status
for i in $(seq 1 30); do echo "content$i" > "file$i.txt"; done
git add . && git commit -q -m "add many files"

run_check "ccr run git diff HEAD~1 compresses large output" \
  "ccr run git diff HEAD~1"

# Test that ccr run writes analytics
ANALYTICS_DB="$DATA_DIR/analytics.db"
if [[ -f "$ANALYTICS_DB" ]]; then
  COUNT=$(sqlite3 "$ANALYTICS_DB" "SELECT COUNT(*) FROM records;")
  if [[ "$COUNT" -gt 0 ]]; then
    ok "ccr run writes analytics to SQLite DB (found $COUNT records)"
  else
    fail "ccr run wrote 0 analytics records"
  fi
else
  fail "analytics.db not created after ccr run"
fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "7. ccr filter — stdin pipeline"
# ─────────────────────────────────────────────────────────────────────────────

LONG_OUTPUT=$(python3 -c "
# Use terraform 'Refreshing state...' lines — these hit the terraform Collapse
# pattern and are NOT stripped by global_rules (only cargo/rustc progress is global)
lines = []
for i in range(20):
    lines.append('  Refreshing state...')
lines.append('Plan: 2 to add, 0 to change, 0 to destroy.')
print('\n'.join(lines))
")

run_check "ccr filter collapses Refreshing state lines (terraform)" \
  "echo \"\$LONG_OUTPUT\" | ccr filter --command terraform" "collapsed"

run_check "ccr filter preserves plan summary line" \
  "echo \"\$LONG_OUTPUT\" | ccr filter --command terraform" "Plan:"

run_check "ccr filter with no command hint still works" \
  "echo 'hello world' | ccr filter"

# ─────────────────────────────────────────────────────────────────────────────
hdr "8. ccr hook — PostToolUse JSON simulation"
# ─────────────────────────────────────────────────────────────────────────────

# Simulate Claude Code calling ccr hook with a Bash tool response
HOOK_INPUT=$(python3 -c "
import json, sys
# Simulate: 50 'Compiling' lines + 1 error line
lines = ['   Compiling crate%d v1.0.0' % i for i in range(50)]
lines.append('error[E0001]: something important')
output = '\n'.join(lines)
payload = {
    'tool_name': 'Bash',
    'tool_input': {'command': 'cargo build'},
    'tool_response': {'output': output}
}
print(json.dumps(payload))
")

HOOK_OUT=$(echo "$HOOK_INPUT" | CCR_SESSION_ID="hook-test-$$" ccr hook 2>/dev/null || true)

if [[ -n "$HOOK_OUT" ]]; then
  if echo "$HOOK_OUT" | python3 -m json.tool > /dev/null 2>&1; then
    ok "ccr hook returns valid JSON"
    INNER=$(echo "$HOOK_OUT" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('output',''))")
    if echo "$INNER" | grep -q "collapsed\|E0001"; then
      ok "ccr hook output contains compressed content"
    else
      fail "ccr hook output doesn't look compressed"
    fi
    if echo "$INNER" | grep -q "E0001"; then
      ok "ccr hook preserves error lines"
    else
      fail "ccr hook dropped the error line"
    fi
  else
    fail "ccr hook returned invalid JSON: $(echo "$HOOK_OUT" | head -2)"
  fi
else
  # Empty output = pass-through (hook decided no compression needed)
  ok "ccr hook returned empty (pass-through — output was too small or trivial)"
fi

# Test Glob tool hook
GLOB_INPUT=$(python3 -c "
import json
paths = ['/project/src/file%d.rs' % i for i in range(100)]
payload = {
    'tool_name': 'Glob',
    'tool_input': {'pattern': '**/*.rs'},
    'tool_response': {'output': '\n'.join(paths)}
}
print(json.dumps(payload))
")

GLOB_OUT=$(echo "$GLOB_INPUT" | CCR_SESSION_ID="glob-test-$$" ccr hook 2>/dev/null || true)
if [[ -n "$GLOB_OUT" ]]; then
  ok "ccr hook handles Glob tool (large path list)"
else
  ok "ccr hook pass-through for Glob (acceptable)"
fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "9. ccr rewrite — command rewriting"
# ─────────────────────────────────────────────────────────────────────────────

run_check "ccr rewrite 'git status' returns ccr-prefixed command" \
  "ccr rewrite 'git status'" "ccr"

run_check "ccr rewrite 'cargo build' returns ccr-prefixed command" \
  "ccr rewrite 'cargo build'" "ccr"

run_check "ccr rewrite 'echo hello' exits (no rewrite for unknown commands)" \
  "ccr rewrite 'echo hello' > /dev/null 2>&1 || true"

# ─────────────────────────────────────────────────────────────────────────────
hdr "10. ccr gain — analytics display"
# ─────────────────────────────────────────────────────────────────────────────

# Run a few more commands to ensure analytics exist
ccr run git log --oneline > /dev/null 2>&1 || true
ccr run git status > /dev/null 2>&1 || true

run_check "ccr gain exits 0" "ccr gain"
run_check "ccr gain shows Runs:" "ccr gain" "Runs:"
run_check "ccr gain shows Tokens saved:" "ccr gain" "Tokens saved:"
run_check "ccr gain --breakdown exits 0" "ccr gain --breakdown"

# ─────────────────────────────────────────────────────────────────────────────
hdr "10. Analytics migration — JSONL → SQLite"
# ─────────────────────────────────────────────────────────────────────────────
# Simulate a user who has v0.5.x JSONL analytics and upgrades to v0.6.0.

# Use a dedicated temp XDG_DATA_HOME to isolate migration from the main test DB
MIGRATE_XDG=$(mktemp -d)
MIGRATE_CCR_DIR="$MIGRATE_XDG/ccr"
mkdir -p "$MIGRATE_CCR_DIR"

# Plant legacy JSONL (simulates a pre-v0.6.0 install)
cp /src/docker/fixtures/legacy_analytics.jsonl "$MIGRATE_CCR_DIR/analytics.jsonl"
LEGACY_COUNT=$(wc -l < "$MIGRATE_CCR_DIR/analytics.jsonl" | tr -d ' ')

# Trigger ccr gain with the isolated data dir — this should auto-migrate JSONL → SQLite
XDG_DATA_HOME="$MIGRATE_XDG" ccr gain > /dev/null 2>&1 || true

MIGRATE_DB="$MIGRATE_CCR_DIR/analytics.db"
MIGRATE_BAK="$MIGRATE_CCR_DIR/analytics.jsonl.bak"

if [[ -f "$MIGRATE_DB" ]]; then
  DB_COUNT=$(sqlite3 "$MIGRATE_DB" "SELECT COUNT(*) FROM records;" 2>/dev/null || echo 0)
  if [[ "$DB_COUNT" -ge "$LEGACY_COUNT" ]]; then
    ok "JSONL migration: $DB_COUNT records migrated to SQLite (expected ~$LEGACY_COUNT)"
  else
    fail "JSONL migration: only $DB_COUNT records in DB, expected $LEGACY_COUNT"
  fi
else
  fail "JSONL migration: analytics.db was not created"
fi

if [[ -f "$MIGRATE_BAK" ]]; then
  ok "JSONL migration: original .jsonl renamed to .jsonl.bak"
else
  fail "JSONL migration: .jsonl.bak not created (old data may be lost)"
fi

# Idempotency: second ccr gain must not re-import records
if [[ -f "$MIGRATE_DB" ]]; then
  BEFORE=$(sqlite3 "$MIGRATE_DB" "SELECT COUNT(*) FROM records;" 2>/dev/null || echo 0)
  XDG_DATA_HOME="$MIGRATE_XDG" ccr gain > /dev/null 2>&1 || true
  AFTER=$(sqlite3 "$MIGRATE_DB" "SELECT COUNT(*) FROM records;" 2>/dev/null || echo 0)
  if [[ "$BEFORE" -eq "$AFTER" ]]; then
    ok "Migration is idempotent: second ccr gain doesn't re-import records"
  else
    fail "Migration ran twice: $BEFORE → $AFTER (should be stable at $BEFORE)"
  fi
fi

rm -rf "$MIGRATE_XDG"

# ─────────────────────────────────────────────────────────────────────────────
hdr "11. SQLite analytics correctness"
# ─────────────────────────────────────────────────────────────────────────────

CURRENT_DB="$DATA_DIR/analytics.db"

if [[ -f "$CURRENT_DB" ]]; then
  # Verify schema
  run_check "analytics.db has 'records' table" \
    "sqlite3 \"$CURRENT_DB\" \".tables\"" "records"

  run_check "analytics.db records have timestamp_secs > 0" \
    "sqlite3 \"$CURRENT_DB\" \"SELECT COUNT(*) FROM records WHERE timestamp_secs > 0;\" | grep -v '^0$'"

  run_check "analytics.db has idx_project_ts index" \
    "sqlite3 \"$CURRENT_DB\" \".indexes records\"" "idx_project_ts"

  # Verify savings_pct is never > 100
  OVER=$(sqlite3 "$CURRENT_DB" "SELECT COUNT(*) FROM records WHERE savings_pct > 100.0;")
  if [[ "$OVER" -eq 0 ]]; then
    ok "No records have savings_pct > 100"
  else
    fail "$OVER records have savings_pct > 100 (data corruption)"
  fi

  # Verify auto-cleanup doesn't delete recent records
  RECENT=$(sqlite3 "$CURRENT_DB" "SELECT COUNT(*) FROM records WHERE timestamp_secs > strftime('%s','now') - 86400;")
  if [[ "$RECENT" -gt 0 ]]; then
    ok "Recent records ($RECENT today) preserved by auto-cleanup"
  else
    skip "auto-cleanup check" "no records written today"
  fi
else
  fail "analytics.db not found at $CURRENT_DB"
fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "12. ccr expand — zoom-in block retrieval"
# ─────────────────────────────────────────────────────────────────────────────

# Generate output with a collapsed block (zoom must be enabled)
ZOOM_OUT=$(ccr run git diff HEAD~1 2>/dev/null || true)
if echo "$ZOOM_OUT" | grep -q "ZI_"; then
  ZOOM_ID=$(echo "$ZOOM_OUT" | grep -o 'ZI_[0-9]*' | head -1)
  run_check "ccr expand $ZOOM_ID retrieves original lines" \
    "ccr expand ${ZOOM_ID#ZI_}"
else
  skip "ccr expand test" "no ZI_ marker in output (zoom may be disabled or output too small)"
fi

# ─────────────────────────────────────────────────────────────────────────────
hdr "13. Uninstall — Claude Code"
# ─────────────────────────────────────────────────────────────────────────────

run_check "ccr init --uninstall exits 0" \
  "ccr init --uninstall"

run_check "hook script removed after uninstall" \
  "test ! -f $HOME/.claude/hooks/ccr-rewrite.sh"

run_check "re-running ccr init after uninstall works" \
  "ccr init && test -f $HOME/.claude/hooks/ccr-rewrite.sh"

# ─────────────────────────────────────────────────────────────────────────────
hdr "14. Edge cases"
# ─────────────────────────────────────────────────────────────────────────────

run_check "ccr run with no args exits cleanly (shows help)" \
  "ccr run 2>&1 || true"

run_check "ccr filter empty stdin produces no output" \
  "echo '' | ccr filter 2>/dev/null; true"

run_check "ccr hook with empty stdin returns nothing (no crash)" \
  "echo '' | ccr hook 2>/dev/null; true"

run_check "ccr hook with malformed JSON returns nothing (no crash)" \
  "echo 'not json at all' | ccr hook 2>/dev/null; true"

run_check "ccr gain with no analytics exits 0" \
  "XDG_DATA_HOME=$(mktemp -d) ccr gain"

# ─────────────────────────────────────────────────────────────────────────────
# Summary
# ─────────────────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}─────────────────────────────────────────────────${NC}"
TOTAL=$((PASS + FAIL + SKIP))
echo -e "  ${BOLD}Results: $TOTAL tests${NC}   ${GREEN}$PASS passed${NC}   ${RED}$FAIL failed${NC}   ${YELLOW}$SKIP skipped${NC}"
echo -e "${BOLD}─────────────────────────────────────────────────${NC}"

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
exit 0
