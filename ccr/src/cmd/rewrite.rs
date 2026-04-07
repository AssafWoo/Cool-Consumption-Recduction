use anyhow::Result;

/// Rewrite a command string for PreToolUse injection.
/// Prints the rewritten command and exits 0, or exits 1 if no rewrite is needed.
pub fn run(command: String) -> Result<()> {
    let rewritten = rewrite_command(&command);
    match rewritten {
        Some(r) => {
            print!("{}", r);
            Ok(())
        }
        None => {
            // No rewrite — exit 1 so the hook passes through silently
            std::process::exit(1);
        }
    }
}

/// Rewrite a full command string. Returns `Some(rewritten)` if rewrite is needed,
/// or `None` if no handler matches or already wrapped.
pub fn rewrite_command(command: &str) -> Option<String> {
    // Handle compound commands: &&, ||, ;
    // Try to split and rewrite each part
    if let Some(result) = rewrite_compound(command, " && ") {
        return Some(result);
    }
    if let Some(result) = rewrite_compound(command, " || ") {
        return Some(result);
    }
    if let Some(result) = rewrite_compound(command, "; ") {
        return Some(result);
    }

    // Single command
    rewrite_single(command)
}

/// Returns the byte offset where the actual command starts, after any leading
/// `KEY=VALUE` environment-variable prefix tokens.
///
/// Quote-aware: `KEY="val with spaces" cargo build` correctly identifies `cargo`
/// as the command start despite the space inside the quoted value.
///
/// Example: `"RUST_LOG=debug cargo build"` → 15 (offset of `cargo`)
fn env_prefix_end(s: &str) -> usize {
    let mut pos = 0;
    let bytes = s.as_bytes();
    loop {
        // skip whitespace
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            return pos;
        }
        let tok_start = pos;
        pos = scan_token(s, tok_start);
        let token = &s[tok_start..pos];
        if !is_env_var_assignment(token) {
            return tok_start;
        }
        // was KEY=VALUE — keep scanning
    }
}

/// Scan one shell token starting at `start`, consuming quoted strings as a unit.
/// Returns the byte offset of the first whitespace (or end of string) after the token.
///
/// Handles single-quoted strings (no escape processing) and double-quoted strings
/// (backslash escapes), so a token like `KEY="val with spaces"` is scanned as one unit.
fn scan_token(s: &str, start: usize) -> usize {
    let bytes = s.as_bytes();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b if b.is_ascii_whitespace() => break,
            b'\'' => {
                // Single-quoted: scan until closing ', no escape processing
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i += 1;
                }
                if i < bytes.len() { i += 1; } // consume closing '
            }
            b'"' => {
                // Double-quoted: scan until closing ", honouring backslash escapes
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2; // skip escaped character
                    } else if bytes[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
            }
            _ => i += 1,
        }
    }
    i
}

/// Returns true if `token` looks like a shell environment-variable assignment
/// (`KEY=VALUE` where KEY is `[A-Za-z_][A-Za-z0-9_]*`).
fn is_env_var_assignment(token: &str) -> bool {
    if let Some((key, _)) = token.split_once('=') {
        !key.is_empty()
            && key.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    } else {
        false
    }
}

/// Returns true if `command` contains a stdout redirect (`>`, `>>`) or pipe
/// (`|`) outside of single or double quotes.
///
/// Simple heuristic — not a full shell parser. Commands whose stdout is
/// diverted to a file or another process must not be wrapped with `ccr run`,
/// because CCR's dedup/delta annotations would replace the real content.
fn has_stdout_diversion(command: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = command.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => { in_single = !in_single; }
            b'"'  if !in_single => { in_double = !in_double; }
            b'>'  if !in_single && !in_double => {
                // Exclude '->' and '=>' (not a redirect)
                let prev = if i > 0 { bytes[i - 1] } else { b' ' };
                if prev != b'-' && prev != b'=' {
                    return true;
                }
            }
            b'|'  if !in_single && !in_double => {
                // '||' is logical OR, not a pipe — skip both characters
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    i += 1; // skip the second '|'
                } else {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Rewrite `head [-N] file` and `tail [-N] file` to `ccr run cat file`,
/// routing through ReadHandler for code-aware filtering instead of raw truncation.
///
/// Handles: `head file`, `head -N file`, `head -n N file`, `head --lines=N file`,
/// and the same variants for `tail`. Skips byte-mode (`-c`), follow-mode (`-f`),
/// multi-file invocations, and stdin (`-`).
fn rewrite_head_tail(cmd: &str) -> Option<String> {
    let args: Vec<&str> = cmd.split_whitespace().collect();
    let binary = *args.first()?;
    if binary != "head" && binary != "tail" {
        return None;
    }

    let mut file: Option<&str> = None;
    let mut i = 1;
    while i < args.len() {
        let a = args[i];
        if a.starts_with('-') {
            match a {
                // Byte mode or follow mode — unsupported, bail
                "-c" | "--bytes" | "-f" | "-F" | "--follow" => return None,
                // -n N or --lines N — skip the value argument too
                "-n" | "--lines" => { i += 2; continue; }
                _ => {
                    if a.starts_with("--lines=") { i += 1; continue; }
                    // -N where N is all digits (e.g. -20, -100)
                    let digits = a.trim_start_matches('-');
                    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                        i += 1; continue;
                    }
                    // Any other flag — bail
                    return None;
                }
            }
        }
        // File argument
        if file.is_some() { return None; } // multiple files — skip
        file = Some(a);
        i += 1;
    }

    let file = file?; // no file argument (reading stdin) — skip
    if file == "-" { return None; }

    Some(format!("ccr run cat {}", file))
}

/// Rewrite a single (non-compound) command.
/// Uses the handler's `rewrite_args` to inject flags (e.g. --message-format json)
/// so the rewritten command string reflects the actual args that will be run.
fn rewrite_single(command: &str) -> Option<String> {
    let trimmed = command.trim();

    // Don't double-wrap
    if trimmed.starts_with("ccr run ") || trimmed == "ccr run" {
        return None;
    }

    // Never wrap commands that divert stdout (redirect or pipe).
    // CCR's dedup/delta annotations would replace real content.
    if has_stdout_diversion(trimmed) {
        return None;
    }

    // Strip any leading KEY=VALUE env-variable prefix tokens so we can match
    // the actual command name (e.g. `RUST_LOG=debug cargo build` → `cargo`).
    let cmd_start = env_prefix_end(trimmed);
    let env_part = &trimmed[..cmd_start]; // e.g. "RUST_LOG=debug " or ""
    let cmd_part = trimmed[cmd_start..].trim_start();

    // head/tail file → ccr run cat file (ReadHandler applies code-aware filtering)
    if let Some(r) = rewrite_head_tail(cmd_part) {
        return Some(format!("{}{}", env_part, r));
    }

    // Extract argv[0]
    let first = cmd_part.split_whitespace().next()?;

    let handler = crate::handlers::get_handler(first)?;

    // Build the flag-injected arg list via the handler (no env prefix in args)
    let args: Vec<String> = cmd_part.split_whitespace().map(String::from).collect();
    let rewritten_args = handler.rewrite_args(&args);

    // Preserve env prefix before `ccr run` so the shell sets those vars for the process.
    Some(format!("{}ccr run {}", env_part, rewritten_args.join(" ")))
}

/// Try to split a compound command on `operator` and rewrite each part.
/// Returns `Some(rewritten)` only if at least one part was rewritten.
fn rewrite_compound(command: &str, operator: &str) -> Option<String> {
    if !command.contains(operator) {
        return None;
    }

    let parts: Vec<&str> = command.split(operator).collect();
    if parts.len() < 2 {
        return None;
    }

    let mut any_rewritten = false;
    let rewritten: Vec<String> = parts
        .iter()
        .map(|part| {
            if let Some(r) = rewrite_single(part.trim()) {
                any_rewritten = true;
                r
            } else {
                part.trim().to_string()
            }
        })
        .collect();

    if any_rewritten {
        Some(rewritten.join(operator))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_command_rewritten() {
        let result = rewrite_command("git status");
        // git status gets --porcelain injected via rewrite_args
        assert_eq!(result, Some("ccr run git status --porcelain".to_string()));
    }

    #[test]
    fn flag_injection_for_cargo_build() {
        let result = rewrite_command("cargo build");
        // cargo build gets --message-format json injected
        let r = result.expect("cargo build should be rewritten");
        assert!(r.starts_with("ccr run cargo build"), "should be wrapped: {}", r);
        assert!(r.contains("--message-format"), "should inject --message-format: {}", r);
        assert!(r.contains("json"), "should inject json format: {}", r);
    }

    #[test]
    fn no_double_flag_injection() {
        // If --message-format already present, it should not be added again
        let result = rewrite_command("cargo build --message-format human");
        let r = result.expect("should be rewritten");
        let count = r.matches("--message-format").count();
        assert_eq!(count, 1, "flag should appear exactly once: {}", r);
    }

    #[test]
    fn unknown_command_not_rewritten() {
        let result = rewrite_command("some-unknown-tool --flag");
        assert_eq!(result, None);
    }

    #[test]
    fn no_double_wrap() {
        let result = rewrite_command("ccr run git status");
        assert_eq!(result, None);
    }

    #[test]
    fn compound_and() {
        let result = rewrite_command("cargo build && git push");
        let r = result.expect("should be rewritten");
        assert!(r.contains("ccr run cargo build"), "cargo part: {}", r);
        assert!(r.contains("ccr run git push"), "git part: {}", r);
        assert!(r.contains(" && "), "should preserve && operator: {}", r);
    }

    #[test]
    fn compound_mixed() {
        // Only known commands get wrapped; git status gets --porcelain injected
        let result = rewrite_command("some-tool && git status");
        assert_eq!(result, Some("some-tool && ccr run git status --porcelain".to_string()));
    }

    #[test]
    fn compound_no_known() {
        // No known commands → no rewrite
        let result = rewrite_command("tool-a && tool-b");
        assert_eq!(result, None);
    }

    #[test]
    fn redirect_bare() {
        assert!(has_stdout_diversion("git show HEAD:src/main.rs > main.rs"));
    }

    #[test]
    fn redirect_append() {
        assert!(has_stdout_diversion("cargo build >> build.log"));
    }

    #[test]
    fn redirect_inside_single_quotes_not_detected() {
        // > inside quotes is not a redirect
        assert!(!has_stdout_diversion("echo 'a > b'"));
    }

    #[test]
    fn redirect_inside_double_quotes_not_detected() {
        assert!(!has_stdout_diversion("echo \"a > b\""));
    }

    #[test]
    fn arrow_operators_not_redirect() {
        // -> and => in code snippets / descriptions must not trigger
        assert!(!has_stdout_diversion("git log --format='%H -> %s'"));
        assert!(!has_stdout_diversion("some-tool => output"));
    }

    #[test]
    fn pipe_detected() {
        assert!(has_stdout_diversion("git log | head -5"));
    }

    #[test]
    fn pipe_inside_quotes_not_detected() {
        assert!(!has_stdout_diversion("echo 'a | b'"));
        assert!(!has_stdout_diversion("echo \"a | b\""));
    }

    #[test]
    fn logical_or_not_detected_as_pipe() {
        assert!(!has_stdout_diversion("test -f foo || echo missing"));
    }

    #[test]
    fn pipe_with_redirect_detected() {
        assert!(has_stdout_diversion("git show HEAD:file | head -1"));
    }

    #[test]
    fn piped_command_not_wrapped() {
        let result = rewrite_command("git show HEAD:file | head -5");
        assert_eq!(result, None, "should not wrap a piped command");
    }

    #[test]
    fn subshell_pipe_detected_as_false_positive() {
        // Pipes inside $() are detected — accepted trade-off since we are
        // not a full shell parser. Prevents wrapping, which is the safe default.
        assert!(has_stdout_diversion("echo $(git log | head -1)"));
    }

    #[test]
    fn pipe_at_start_of_string() {
        assert!(has_stdout_diversion("| cat"));
    }

    #[test]
    fn compound_with_pipe_only_wraps_non_piped_part() {
        // rewrite_compound splits on && then has_stdout_diversion guards each part.
        // The piped part must NOT be wrapped; the non-piped part should be.
        let result = rewrite_command("cargo build && git log | head -5");
        assert!(result.is_some(), "compound should still rewrite the non-piped part");
        let r = result.unwrap();
        assert!(r.contains("ccr run cargo build"), "cargo build should be wrapped");
        assert!(!r.contains("ccr run git log"), "piped git log must not be wrapped");
        assert!(r.contains("git log | head -5"), "piped part should pass through unchanged");
    }

    #[test]
    fn git_show_redirect_not_wrapped() {
        // git show with redirect must not be wrapped — would corrupt the output file
        let result = rewrite_command("git show origin/main:src/lib.rs > /tmp/lib.rs");
        assert_eq!(result, None, "should not wrap a redirected command");
    }

    #[test]
    fn git_show_no_redirect_still_wrapped() {
        // git show without redirect should still be wrapped normally
        let result = rewrite_command("git show HEAD");
        assert!(result.is_some(), "should wrap git show without redirect");
        assert!(result.unwrap().starts_with("ccr run git show"));
    }

    // ── env prefix tests ──────────────────────────────────────────────────────

    #[test]
    fn env_prefix_single_var() {
        let result = rewrite_command("RUST_LOG=debug cargo build");
        let r = result.expect("should rewrite despite env prefix");
        assert!(r.starts_with("RUST_LOG=debug ccr run cargo build"), "got: {}", r);
        assert!(r.contains("--message-format"), "should still inject --message-format: {}", r);
    }

    #[test]
    fn env_prefix_multiple_vars() {
        let result = rewrite_command("CI=1 NODE_ENV=production npm install");
        let r = result.expect("should rewrite despite multiple env prefixes");
        assert!(r.starts_with("CI=1 NODE_ENV=production ccr run npm"), "got: {}", r);
    }

    #[test]
    fn env_prefix_no_handler_still_none() {
        let result = rewrite_command("RUST_LOG=debug unknown-tool --flag");
        assert_eq!(result, None, "no handler → no rewrite even with env prefix");
    }

    #[test]
    fn is_env_var_assignment_valid() {
        assert!(is_env_var_assignment("RUST_LOG=debug"));
        assert!(is_env_var_assignment("CI=1"));
        assert!(is_env_var_assignment("_VAR=value"));
        assert!(is_env_var_assignment("KEY="));        // empty value is valid
    }

    #[test]
    fn is_env_var_assignment_invalid() {
        assert!(!is_env_var_assignment("cargo"));      // no '='
        assert!(!is_env_var_assignment("--flag=val")); // starts with '-'
        assert!(!is_env_var_assignment("1KEY=val"));   // starts with digit
        assert!(!is_env_var_assignment("=value"));     // empty key
    }

    #[test]
    fn env_prefix_compound_command() {
        let result = rewrite_command("CI=1 cargo build && git status");
        let r = result.expect("should rewrite compound with env prefix");
        assert!(r.contains("CI=1 ccr run cargo build"), "cargo part: {}", r);
        assert!(r.contains("ccr run git status"), "git part: {}", r);
    }

    // ── quoted env prefix tests ───────────────────────────────────────────────

    #[test]
    fn env_prefix_quoted_double_value() {
        let result = rewrite_command("KEY=\"val with spaces\" cargo build");
        let r = result.expect("should rewrite despite quoted env value");
        assert!(r.contains("ccr run cargo build"), "got: {}", r);
        assert!(r.contains("--message-format"), "should inject flag: {}", r);
    }

    #[test]
    fn env_prefix_quoted_single_value() {
        let result = rewrite_command("NODE_ENV='production mode' npm install");
        let r = result.expect("should rewrite despite single-quoted env value");
        assert!(r.contains("ccr run npm"), "got: {}", r);
    }

    #[test]
    fn scan_token_plain() {
        assert_eq!(scan_token("cargo build", 0), 5); // "cargo"
    }

    #[test]
    fn scan_token_double_quoted() {
        // KEY="val with spaces" → token ends after closing "
        let s = r#"KEY="val with spaces" cargo"#;
        let end = scan_token(s, 0);
        assert_eq!(&s[..end], r#"KEY="val with spaces""#);
    }

    #[test]
    fn scan_token_single_quoted() {
        let s = "KEY='val with spaces' cargo";
        let end = scan_token(s, 0);
        assert_eq!(&s[..end], "KEY='val with spaces'");
    }

    #[test]
    fn scan_token_escaped_in_double_quotes() {
        let s = r#"KEY="val\"quoted" cargo"#;
        let end = scan_token(s, 0);
        assert_eq!(&s[..end], r#"KEY="val\"quoted""#);
    }

    // ── head / tail rewrite tests ─────────────────────────────────────────────

    #[test]
    fn head_plain_file() {
        assert_eq!(
            rewrite_command("head src/main.rs"),
            Some("ccr run cat src/main.rs".to_string())
        );
    }

    #[test]
    fn head_numeric_flag() {
        assert_eq!(
            rewrite_command("head -20 src/main.rs"),
            Some("ccr run cat src/main.rs".to_string())
        );
    }

    #[test]
    fn head_n_flag_with_space() {
        assert_eq!(
            rewrite_command("head -n 50 src/lib.rs"),
            Some("ccr run cat src/lib.rs".to_string())
        );
    }

    #[test]
    fn head_lines_long_flag() {
        assert_eq!(
            rewrite_command("head --lines=30 README.md"),
            Some("ccr run cat README.md".to_string())
        );
    }

    #[test]
    fn tail_numeric_flag() {
        assert_eq!(
            rewrite_command("tail -20 src/main.rs"),
            Some("ccr run cat src/main.rs".to_string())
        );
    }

    #[test]
    fn tail_n_flag_with_space() {
        assert_eq!(
            rewrite_command("tail -n 10 src/lib.rs"),
            Some("ccr run cat src/lib.rs".to_string())
        );
    }

    #[test]
    fn head_byte_mode_skipped() {
        assert_eq!(rewrite_command("head -c 100 src/main.rs"), None);
    }

    #[test]
    fn tail_follow_mode_skipped() {
        assert_eq!(rewrite_command("tail -f /var/log/app.log"), None);
    }

    #[test]
    fn head_multiple_files_skipped() {
        assert_eq!(rewrite_command("head -20 a.rs b.rs"), None);
    }

    #[test]
    fn head_no_file_skipped() {
        // head with no file reads stdin — don't rewrite
        assert_eq!(rewrite_command("head -20"), None);
    }

    #[test]
    fn head_stdin_dash_skipped() {
        assert_eq!(rewrite_command("head -20 -"), None);
    }

    #[test]
    fn head_in_compound_with_git() {
        let result = rewrite_command("head -50 src/main.rs && git status");
        let r = result.expect("compound should rewrite");
        assert!(r.contains("ccr run cat src/main.rs"), "head part: {}", r);
        assert!(r.contains("ccr run git status"), "git part: {}", r);
    }
}
