//! `ccr doctor` — diagnose a CCR installation.
//!
//! Checks every layer of the analytics pipeline so users can self-diagnose
//! the "ccr gain shows 0 runs" problem without filing a bug report.

use anyhow::Result;
use owo_colors::{OwoColorize, Stream::Stdout};
use std::path::{Path, PathBuf};

pub fn run() -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot locate home directory"))?;
    let mut any_error = false;

    println!("{}", "CCR Doctor".bold());
    println!("{}", "═".repeat(52));

    // ── 1. Hook Setup ─────────────────────────────────────────────────────────
    println!();
    println!("{}", "Hook Setup".bold());
    let hook_script = home.join(".claude").join("hooks").join("ccr-rewrite.sh");
    let bin_in_hook = check_hook_script(&hook_script, &mut any_error);
    let settings = home.join(".claude").join("settings.json");
    check_settings(&settings, &mut any_error);
    check_jq();

    // ── 2. Analytics ─────────────────────────────────────────────────────────
    println!();
    println!("{}", "Analytics".bold());
    check_analytics(&mut any_error);

    // ── 3. End-to-end rewrite check ───────────────────────────────────────────
    println!();
    println!("{}", "Rewrite Check".bold());
    check_rewrite();

    // ── 4. Binary path in hook ────────────────────────────────────────────────
    if let Some(ref p) = bin_in_hook {
        println!();
        println!("{}", "Hook Binary".bold());
        check_hook_binary(p, &mut any_error);
    }

    // ── 5. Tips ───────────────────────────────────────────────────────────────
    println!();
    if any_error {
        println!(
            "{}",
            "One or more checks failed. See items marked ✗ above.".red()
        );
    } else {
        println!("{}", "All checks passed.".green().bold());
    }
    println!();
    println!("If ccr gain still shows 0 runs after all checks pass:");
    println!("  1. Commands must be run BY Claude Code (ask it: \"run git status\")");
    println!("     — not typed by you in the terminal");
    println!("  2. Restart Claude Code after running 'ccr init'");
    println!("     — hooks in settings.json only activate at session start");
    println!("  3. Verify manually: ask Claude Code to run a command, then check");
    println!("     'ccr gain' — Runs should increment");

    Ok(())
}

// ── Check helpers ────────────────────────────────────────────────────────────

fn ok(label: &str, detail: &str) {
    println!(
        "  {}  {:<28} {}",
        "✓".if_supports_color(Stdout, |t| t.green()),
        label,
        detail.if_supports_color(Stdout, |t| t.dimmed()),
    );
}

fn warn(label: &str, detail: &str) {
    println!(
        "  {}  {:<28} {}",
        "~".if_supports_color(Stdout, |t| t.yellow()),
        label,
        detail.if_supports_color(Stdout, |t| t.yellow()),
    );
}

fn err(label: &str, detail: &str, fix: &str, any_error: &mut bool) {
    *any_error = true;
    println!(
        "  {}  {:<28} {}",
        "✗".if_supports_color(Stdout, |t| t.red()),
        label,
        detail.if_supports_color(Stdout, |t| t.red()),
    );
    if !fix.is_empty() {
        println!(
            "     {:<28} {}",
            "",
            format!("fix: {}", fix).if_supports_color(Stdout, |t| t.yellow()),
        );
    }
}

/// Check the hook script exists and is executable. Returns the ccr binary path
/// embedded in the script (if parseable).
fn check_hook_script(script: &Path, any_error: &mut bool) -> Option<PathBuf> {
    if !script.exists() {
        err(
            "Hook script",
            "NOT found — commands won't be rewritten",
            "run: ccr init   then restart Claude Code",
            any_error,
        );
        return None;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(script).ok()?.permissions().mode();
        if mode & 0o111 == 0 {
            err(
                "Hook script",
                "exists but NOT executable",
                &format!("chmod +x {}", script.display()),
                any_error,
            );
        } else {
            ok("Hook script", &script.display().to_string());
        }
    }
    #[cfg(not(unix))]
    {
        ok("Hook script", &script.display().to_string());
    }

    let content = std::fs::read_to_string(script).ok()?;
    extract_bin_path(&content)
}

/// Parse the ccr binary path out of the rewrite hook script.
fn extract_bin_path(content: &str) -> Option<PathBuf> {
    for line in content.lines() {
        if line.contains("REWRITTEN=") && line.contains("rewrite") {
            // Line looks like: REWRITTEN=$(CCR_SESSION_ID=$PPID "/path/to/ccr" rewrite "$CMD" ...)
            if let Some(ppid_pos) = line.find("$PPID ") {
                let after = &line[ppid_pos + 6..];
                if after.starts_with('"') {
                    if let Some(end) = after[1..].find('"') {
                        return Some(PathBuf::from(&after[1..end + 1]));
                    }
                }
            }
        }
    }
    None
}

fn check_settings(settings: &Path, any_error: &mut bool) {
    if !settings.exists() {
        err(
            "settings.json",
            "NOT found — hooks will never fire",
            "run: ccr init   then restart Claude Code",
            any_error,
        );
        return;
    }

    let content = match std::fs::read_to_string(settings) {
        Ok(c) => c,
        Err(_) => {
            err("settings.json", "cannot read file", "", any_error);
            return;
        }
    };

    let has_pre = content.contains("ccr-rewrite.sh");
    let has_post = content.contains("PostToolUse");

    if has_pre {
        ok("settings.json PreToolUse", "ccr-rewrite.sh registered");
    } else {
        err(
            "settings.json PreToolUse",
            "NOT registered — commands won't be rewritten",
            "run: ccr init   then restart Claude Code",
            any_error,
        );
    }

    if has_post {
        ok("settings.json PostToolUse", "ccr hook registered");
    } else {
        err(
            "settings.json PostToolUse",
            "NOT registered — output won't be filtered",
            "run: ccr init   then restart Claude Code",
            any_error,
        );
    }
}

fn check_jq() {
    let available = std::process::Command::new("which")
        .arg("jq")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if available {
        ok("jq", "available");
    } else {
        warn("jq", "NOT found in PATH — hook scripts need jq");
        println!(
            "     {:<28} {}",
            "",
            "install: brew install jq".if_supports_color(Stdout, |t| t.yellow()),
        );
    }
}

fn check_analytics(any_error: &mut bool) {
    let db_path = match crate::analytics_db::db_path() {
        Some(p) => p,
        None => {
            err("DB path", "cannot determine data directory", "", any_error);
            return;
        }
    };

    ok("DB path", &db_path.display().to_string());

    if !db_path.exists() {
        err(
            "DB",
            "NOT created yet — ccr run has never been called",
            "test now:  ccr run git status",
            any_error,
        );
        println!(
            "     {:<28} {}",
            "",
            "then re-run 'ccr doctor' — DB should appear and show 1 record".if_supports_color(Stdout, |t| t.dimmed()),
        );
        println!(
            "     {:<28} {}",
            "",
            "if DB still missing after that, check file permissions on the path above".if_supports_color(Stdout, |t| t.dimmed()),
        );
        return;
    }

    // Record count
    match crate::analytics_db::load_all(None) {
        Ok(records) => {
            let total = records.len();
            if total == 0 {
                warn("DB records", "0 records — ccr run never succeeded");
                println!(
                    "     {:<28} {}",
                    "",
                    "test:  ccr run git status   (should write a record)".if_supports_color(Stdout, |t| t.yellow()),
                );
                println!(
                    "     {:<28} {}",
                    "",
                    "then:  ccr gain             (should show Runs: 1)".if_supports_color(Stdout, |t| t.dimmed()),
                );
            } else {
                // Count today's records
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let today_start = now - (now % 86400);
                let today = records.iter().filter(|r| r.timestamp_secs >= today_start).count();
                ok(
                    "DB records",
                    &format!("{} total, {} today", total, today),
                );
            }
        }
        Err(e) => {
            err("DB", &format!("read error: {}", e), "check file permissions", any_error);
            return;
        }
    }

    // Writeability test
    check_db_writable(&db_path, any_error);
}

fn check_db_writable(db_path: &Path, any_error: &mut bool) {
    // Try opening the DB and doing a no-op (schema already exists)
    match crate::analytics_db::open() {
        Ok(_) => ok("DB writable", "open OK"),
        Err(e) => err(
            "DB writable",
            &format!("FAILED: {}", e),
            &format!("run: chmod 755 \"{}\"", db_path.parent().unwrap_or(db_path).display()),
            any_error,
        ),
    }
}

fn check_rewrite() {
    match std::process::Command::new(
        std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("ccr")),
    )
    .args(["rewrite", "git status"])
    .output()
    {
        Ok(out) if out.status.success() => {
            let rewritten = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if rewritten.starts_with("ccr run ") {
                ok(
                    "'git status'",
                    &format!("→ '{}'", rewritten),
                );
            } else {
                warn(
                    "'git status'",
                    &format!("unexpected rewrite: '{}'", rewritten),
                );
            }
        }
        Ok(_) => {
            warn("'git status'", "no rewrite (git handler may be missing)");
        }
        Err(e) => {
            warn("rewrite check", &format!("could not run ccr rewrite: {}", e));
        }
    }
}

fn check_hook_binary(bin_path: &Path, any_error: &mut bool) {
    if bin_path.exists() {
        ok(
            "Binary in hook",
            &format!("{} (exists)", bin_path.display()),
        );
    } else {
        err(
            "Binary in hook",
            &format!("{} NOT FOUND", bin_path.display()),
            "run: ccr init   (rewrites hook with current binary path)",
            any_error,
        );
        println!(
            "     {:<28} {}",
            "",
            "Common cause: 'brew upgrade ccr' changed the cellar path.".if_supports_color(Stdout, |t| t.dimmed()),
        );
        println!(
            "     {:<28} {}",
            "",
            "Fix takes 2 seconds: ccr init && restart Claude Code".if_supports_color(Stdout, |t| t.dimmed()),
        );
    }
}
