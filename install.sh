#!/usr/bin/env bash
# CCR installer — builds from source via cargo (works on any Rust-supported platform).
# macOS users can alternatively use: brew tap AssafWoo/ccr && brew install ccr
set -e

REPO_URL="https://github.com/AssafWoo/homebrew-ccr.git"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"

# ── 1. Ensure Rust / cargo is available ───────────────────────────────────────

if ! command -v cargo &>/dev/null; then
  echo "Rust not found — installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
  # Source cargo env for the rest of this script
  # shellcheck source=/dev/null
  source "$HOME/.cargo/env"
fi

# ── 2. Build and install ccr ──────────────────────────────────────────────────

echo "Building CCR from source (this takes ~1 min on first run)..."
cargo install --git "$REPO_URL" --bin ccr --locked 2>&1

# ── 3. Ensure ~/.cargo/bin is on PATH (shell rc files) ───────────────────────

add_to_path() {
  local rc="$1"
  local line='export PATH="$HOME/.cargo/bin:$PATH"'
  if [ -f "$rc" ] && ! grep -qF '.cargo/bin' "$rc"; then
    echo "" >> "$rc"
    echo "# Added by CCR installer" >> "$rc"
    echo "$line" >> "$rc"
    echo "  → Added cargo/bin to $rc"
  fi
}

if ! echo "$PATH" | grep -q '.cargo/bin'; then
  echo ""
  echo "Adding ~/.cargo/bin to PATH in your shell config..."
  add_to_path "$HOME/.bashrc"
  add_to_path "$HOME/.zshrc"
  add_to_path "$HOME/.profile"
  export PATH="$CARGO_BIN:$PATH"
  echo "  (effective now in this session)"
fi

# ── 4. Register Claude Code hooks ─────────────────────────────────────────────

echo ""
if command -v ccr &>/dev/null; then
  ccr init && echo "Claude Code hooks registered."
elif [ -x "$CARGO_BIN/ccr" ]; then
  "$CARGO_BIN/ccr" init && echo "Claude Code hooks registered."
else
  echo "Run 'ccr init' to register Claude Code hooks."
fi

echo ""
echo "CCR installed. Open a new terminal (or run: source ~/.cargo/env) and you're set."
