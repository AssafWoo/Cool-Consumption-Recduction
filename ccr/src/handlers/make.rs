use std::sync::OnceLock;

use super::util;
use super::Handler;

pub struct MakeHandler;

/// Compiler/linker binary prefixes whose invocation lines are pure noise on success.
/// On a clean build Claude only needs to know *what* was built, not every gcc -O2 -c ...
const COMPILER_PREFIXES: &[&str] = &[
    "gcc ", "gcc\t", "g++ ", "g++\t",
    "cc ", "cc\t", "c++ ", "c++\t",
    "clang ", "clang\t", "clang++ ", "clang++\t",
    "ar ", "ar\t",
    "ld ", "ld\t",
    "as ", "as\t",
    "ranlib ", "ranlib\t",
    "strip ", "strip\t",
    "libtool ", "libtool\t",
    "install ", "install\t",
    "cp ", "cp\t",
    "rm ", "rm\t",
    "mkdir ", "mkdir\t",
    "cmake ", "cmake\t",
    "ninja ",
];

/// Returns true for lines that are just compiler/linker invocations (noisy on success).
fn is_compiler_invocation(t: &str) -> bool {
    COMPILER_PREFIXES.iter().any(|p| t.starts_with(p))
}

fn re_path_error() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^\S+:\d+:\d*:?\s+(error|warning)").expect("make path-error regex")
    })
}

impl Handler for MakeHandler {
    fn filter(&self, output: &str, _args: &[String]) -> String {
        const MAKE_RULES: &[util::MatchOutputRule] = &[util::MatchOutputRule {
            success_pattern: r"(?i)nothing to be done|build ok|all targets up to date",
            error_pattern: r"(?i)\*\*\* \[|: error:|make.*Error",
            ok_message: "make: ok (nothing to do)",
        }];
        if let Some(msg) = util::check_match_output(output, MAKE_RULES) {
            return msg;
        }

        let lines: Vec<&str> = output.lines().collect();
        let has_error = lines
            .iter()
            .any(|l| l.contains("Error ") || l.contains(": error:") || l.contains("*** ["));

        let mut out: Vec<String> = Vec::new();
        let mut compiled_count: usize = 0;

        for line in &lines {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            // Drop make recursion internals (make[N]: Entering/Leaving)
            if t.starts_with("make[") {
                continue;
            }
            // Always keep compiler errors/warnings/notes
            if t.contains(": error:") || t.contains(": warning:") || t.contains(": note:") {
                out.push(line.to_string());
                continue;
            }
            // Keep file:line:col error/warning paths (already compiled once via OnceLock)
            if re_path_error().is_match(t) {
                out.push(line.to_string());
                continue;
            }
            // Keep make failure lines
            if t.starts_with("make:") && t.contains("Error") {
                out.push(line.to_string());
                continue;
            }
            // Non-make lines
            if !t.starts_with("make") {
                if has_error {
                    // On errors keep all context so Claude can understand the build state
                    out.push(line.to_string());
                } else {
                    // On success: drop compiler/linker invocations (noise), count them instead
                    if is_compiler_invocation(t) {
                        compiled_count += 1;
                    } else {
                        // Keep custom echo lines, progress messages, final binary names
                        out.push(line.to_string());
                    }
                }
            }
        }

        if !has_error {
            // Success: emit a clean summary
            let summary = if compiled_count > 0 {
                format!("[make: complete — {} file(s) compiled]", compiled_count)
            } else {
                "[make: complete]".to_string()
            };

            // Keep only meaningful non-compiler lines (custom echo, etc.)
            let meaningful: Vec<String> = out
                .into_iter()
                .filter(|l| !l.trim().is_empty())
                .collect();

            if meaningful.is_empty() {
                return summary;
            }
            // Show last 5 meaningful lines + summary
            let tail: Vec<String> = meaningful
                .iter()
                .rev()
                .take(5)
                .rev()
                .cloned()
                .collect();
            return format!("{}\n{}", tail.join("\n"), summary);
        }

        if out.is_empty() {
            output.to_string()
        } else {
            out.join("\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> MakeHandler {
        MakeHandler
    }

    // ── short-circuit tests (unchanged behaviour) ─────────────────────────────

    #[test]
    fn nothing_to_be_done_short_circuits() {
        let output = "make[1]: Entering directory '/project'\nmake[1]: Nothing to be done for 'all'.\nmake[1]: Leaving directory '/project'";
        assert_eq!(h().filter(output, &[]), "make: ok (nothing to do)");
    }

    #[test]
    fn error_output_not_short_circuited() {
        let output =
            "gcc -o main main.c\nmain.c:5:1: error: expected ';' before '}'\n*** [main] Error 1";
        let result = h().filter(output, &[]);
        assert_ne!(result, "make: ok (nothing to do)");
        assert!(result.contains("error") || result.contains("Error"));
    }

    // ── success path: compiler lines are now dropped ──────────────────────────

    #[test]
    fn success_drops_compiler_invocations() {
        let output = "\
gcc -O2 -c src/foo.c -o obj/foo.o\n\
gcc -O2 -c src/bar.c -o obj/bar.o\n\
gcc -O2 -c src/baz.c -o obj/baz.o\n\
gcc -o myapp obj/foo.o obj/bar.o obj/baz.o\n";
        let result = h().filter(output, &[]);
        // Should NOT contain raw gcc invocations
        assert!(!result.contains("gcc -O2"), "gcc lines should be dropped, got: {}", result);
        // Should report compiled count
        assert!(result.contains("4 file(s) compiled"), "should count compilations, got: {}", result);
        assert!(result.contains("[make: complete"), "should have completion marker, got: {}", result);
    }

    #[test]
    fn success_keeps_custom_echo_lines() {
        let output = "\
gcc -O2 -c src/main.c -o obj/main.o\n\
ar rcs libfoo.a obj/main.o\n\
Build complete — libfoo.a ready\n";
        let result = h().filter(output, &[]);
        // Custom echo line must be preserved
        assert!(result.contains("Build complete"), "custom echo should be kept, got: {}", result);
        assert!(result.contains("[make: complete"), "got: {}", result);
    }

    #[test]
    fn success_large_build_collapses() {
        // Simulate a 50-file C project build
        let mut output = String::new();
        for i in 0..50 {
            output.push_str(&format!("gcc -O2 -Wall -c src/module{}.c -o obj/module{}.o\n", i, i));
        }
        output.push_str("gcc -o myapp obj/*.o -lm\n");
        output.push_str("Linking complete\n");

        let result = h().filter(&output, &[]);
        let line_count = result.lines().count();
        // 51 input lines should collapse to ≤ 3 output lines
        assert!(line_count <= 3, "expected ≤3 lines for large build, got {}: {}", line_count, result);
        assert!(result.contains("51 file(s) compiled") || result.contains("file(s) compiled"), "got: {}", result);
        assert!(result.contains("Linking complete"), "custom link message should survive, got: {}", result);
    }

    // ── error path: all context kept ─────────────────────────────────────────

    #[test]
    fn error_path_keeps_all_context() {
        let output = "\
gcc -O2 -c src/foo.c -o obj/foo.o\n\
src/foo.c:42:5: error: use of undeclared identifier 'bar'\n\
src/foo.c:42:5: note: did you mean 'baz'?\n\
*** [obj/foo.o] Error 1\n";
        let result = h().filter(output, &[]);
        assert!(result.contains("error: use of undeclared"), "error line must be kept, got: {}", result);
        assert!(result.contains("note: did you mean"), "note line must be kept, got: {}", result);
        assert!(result.contains("Error 1"), "make error line must be kept, got: {}", result);
    }

    #[test]
    fn error_path_file_lineno_format() {
        // gcc-style "file:line:col: error:" kept via path-error regex
        let output = "gcc -c main.c\nmain.c:10:3: error: stray '\\' in program\n*** [main.o] Error 1";
        let result = h().filter(output, &[]);
        assert!(result.contains("main.c:10:3"), "file:line:col must be kept, got: {}", result);
    }

    #[test]
    fn make_internals_stripped() {
        let output = "\
make[1]: Entering directory '/project/src'\n\
gcc -c foo.c\n\
make[1]: Leaving directory '/project/src'\n\
make[2]: Nothing to be done for 'tests'.\n";
        let result = h().filter(output, &[]);
        assert!(!result.contains("make[1]"), "make[N] lines should be stripped, got: {}", result);
        assert!(!result.contains("make[2]"), "got: {}", result);
    }

    // ── regex caching: OnceLock is called multiple times without panic ────────

    #[test]
    fn regex_onceLock_stable_across_calls() {
        let output = "src/a.c:1:1: warning: unused variable\n*** [a.o] Error 1";
        for _ in 0..20 {
            let result = h().filter(output, &[]);
            assert!(result.contains("warning"), "should be stable across calls");
        }
    }
}
