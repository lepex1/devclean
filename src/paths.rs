//! Where devclean keeps its user-facing files: directly next to the
//! executable, so a portable copy carries its log and remembered folder with
//! it. Callers that must always succeed fall back to the working directory.

use std::path::{Path, PathBuf};

/// Folder holding the executable; `None` only in exotic environments where the
/// process cannot name its own binary.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// A file next to the executable, or `None` when its folder is unknown.
#[must_use]
pub fn state_file(name: &str) -> Option<PathBuf> {
    Some(state_dir()?.join(name))
}

/// Equality with filesystem semantics: ASCII case-insensitive on Windows,
/// byte-exact elsewhere.
#[must_use]
pub fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn state_files_live_next_to_the_exe() {
        let dir = super::state_dir().expect("exe folder");
        let log = super::state_file("devclean.log").expect("state file");
        assert_eq!(log.parent(), Some(dir.as_path()));
        assert_eq!(
            log.file_name().and_then(|name| name.to_str()),
            Some("devclean.log")
        );
    }

    #[test]
    fn same_path_follows_platform_casing() {
        let a = std::path::Path::new("C:/Users/User");
        let b = std::path::Path::new("c:/users/user");
        #[cfg(windows)]
        assert!(super::same_path(a, b));
        #[cfg(not(windows))]
        assert!(!super::same_path(a, b));
    }
}
