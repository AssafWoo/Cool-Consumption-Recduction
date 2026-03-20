//! Shared utility functions used across CCR features.

/// FNV-1a 64-bit hash of a string, returned as a 16-char lowercase hex string.
/// Used for stable project/content identifiers that don't require cryptographic security.
pub fn hash_str(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

/// Derive the Claude Code project directory name from the current working directory.
/// Claude names project dirs by replacing every '/' in the cwd with '-'.
/// e.g. `/Users/foo/Desktop/ccr` → `-Users-foo-Desktop-ccr`
pub fn project_dir_from_cwd() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().replace('/', "-"))
}

/// Compute a stable project key for the current repo.
/// Priority: sha of `git remote get-url origin` → sha of cwd.
/// Returns a 16-char hex string.
pub fn project_key() -> Option<String> {
    // Try git remote URL first (stable across machines if same repo)
    let remote = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    if let Some(url) = remote {
        return Some(hash_str(&url));
    }

    // Fall back to cwd hash
    std::env::current_dir()
        .ok()
        .map(|p| hash_str(&p.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_str_is_deterministic() {
        assert_eq!(hash_str("hello"), hash_str("hello"));
        assert_eq!(hash_str("hello").len(), 16);
    }

    #[test]
    fn hash_str_differs_for_different_inputs() {
        assert_ne!(hash_str("git status"), hash_str("git log"));
    }

    #[test]
    fn project_dir_from_cwd_replaces_slashes() {
        // Can't assert exact value (depends on machine), but slashes should be gone
        if let Some(dir) = project_dir_from_cwd() {
            assert!(!dir.contains('/'));
        }
    }
}
