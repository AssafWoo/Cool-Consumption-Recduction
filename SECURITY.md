# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in PandaFilter, please **do not open a public GitHub issue**.

Report it privately via GitHub's built-in security advisory system:
👉 [Open a private security advisory](https://github.com/AssafWoo/PandaFilter/security/advisories/new)

- **Response time**: We aim to acknowledge reports within 48 hours
- **Disclosure**: We follow responsible disclosure practices (90-day embargo)

---

## Why PandaFilter Is Sensitive

PandaFilter is a CLI tool that sits between Claude Code and your shell. It:
- Intercepts and rewrites shell commands via a `PreToolUse` hook
- Reads and compresses tool output via a `PostToolUse` hook
- Executes commands on your behalf via `ccr run`

This makes the following attack surfaces especially important:

---

## High-Risk Files

### Tier 1: Command Interception & Execution
- **`ccr/src/hook.rs`** — PostToolUse hook, processes all tool output
- **`ccr/src/cmd/run.rs`** — Executes commands on behalf of Claude Code
- **`ccr/src/cmd/rewrite.rs`** — Rewrites commands before execution (injection risk)
- **`ccr/src/main.rs`** — `init()` writes hooks into `~/.claude/settings.json`
- **`hooks/ccr-rewrite.sh`** / **`~/.claude/hooks/ccr-rewrite.sh`** — Shell hook that intercepts every Bash tool call in Claude Code

### Tier 2: Input Handling
- **`ccr/src/handlers/*.rs`** — Per-command output filters (parse untrusted command output)
- **`ccr/src/session.rs`** — Session state persistence
- **`ccr/src/noise_learner.rs`** — Learns and persists patterns from command output

### Tier 3: Supply Chain & CI/CD
- **`Cargo.toml`** — Dependency manifest
- **`.github/workflows/*.yml`** — Release pipeline (produces signed binaries)

---

## Dangerous Patterns We Watch For

| Pattern | Risk |
|---------|------|
| `Command::new("sh").arg("-c")` | Shell injection via user input |
| `.env("LD_PRELOAD")` | Library hijacking |
| `reqwest::`, `std::net::` | Unexpected network/exfiltration |
| `unsafe {` | Bypasses Rust memory safety |
| Hardcoded secrets or tokens | Credential exposure |
| Base64/hex encoded strings | Obfuscation of malicious payloads |
| Time-based conditionals | Logic bombs |

---

## Dependency Policy

New dependencies added to `Cargo.toml` must meet:
- **Downloads**: >10,000 on crates.io
- **License**: MIT or Apache-2.0 compatible
- **Activity**: Updated within the last 6 months
- **No typosquatting**: Manually verified against similar crate names

---

## Disclosure Timeline

1. **Day 0** — Acknowledgment sent to reporter
2. **Day 7** — Severity and impact assessed
3. **Day 14** — Patch development begins
4. **Day 30** — Patch released
5. **Day 90** — Public disclosure (or earlier if patch is deployed)

Critical vulnerabilities (command injection, data exfiltration) will be fast-tracked.

---

## Security Tooling

- **`cargo audit`** — CVE scanning (runs in CI on every release)
- **`cargo clippy`** — Lints for unsafe patterns
- **GitHub Dependabot** — Automated dependency updates

---

**Last updated**: 2026-04-02
