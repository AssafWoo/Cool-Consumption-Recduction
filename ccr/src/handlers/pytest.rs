use super::Handler;
use super::util;

pub struct PytestHandler;

impl Handler for PytestHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        let has_verbosity = args.iter().any(|a| a == "-v" || a == "-s" || a == "-q");
        let has_tb = args.iter().any(|a| a.starts_with("--tb") || a == "--tb");
        let mut out = args.to_vec();
        if !has_verbosity {
            out.push("-q".to_string());
        }
        if !has_tb {
            out.push("--tb=short".to_string());
        }
        out
    }

    fn filter(&self, output: &str, _args: &[String]) -> String {
        util::test_failures(output, "pytest")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::Handler;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tb_short_injected_by_default() {
        let result = PytestHandler.rewrite_args(&args(&["pytest"]));
        assert!(result.contains(&"--tb=short".to_string()));
        assert!(result.contains(&"-q".to_string()));
    }

    #[test]
    fn tb_short_not_doubled_when_user_passes_tb_long() {
        let result = PytestHandler.rewrite_args(&args(&["pytest", "--tb=long"]));
        let count = result.iter().filter(|a| a.starts_with("--tb")).count();
        assert_eq!(count, 1, "should not add a second --tb flag");
        assert!(!result.contains(&"--tb=short".to_string()));
    }

    #[test]
    fn tb_short_injected_alongside_v() {
        let result = PytestHandler.rewrite_args(&args(&["pytest", "-v"]));
        assert!(result.contains(&"--tb=short".to_string()), "should inject --tb=short even with -v");
        assert!(!result.contains(&"-q".to_string()), "should not inject -q when -v is present");
    }
}
