//! Small glob helpers built on `globset`. Patterns use glob syntax
//! (`*`, `**`, `?`); path globs are matched against repo-relative paths.

use globset::{Glob, GlobMatcher};

/// Pre-compiled set of glob patterns, matched as a logical OR.
pub struct GlobSet {
    matchers: Vec<(String, GlobMatcher)>,
}

impl GlobSet {
    pub fn new(patterns: &[String]) -> Self {
        let matchers = patterns
            .iter()
            .filter_map(|p| {
                Glob::new(p)
                    .ok()
                    .map(|g| (p.clone(), g.compile_matcher()))
            })
            .collect();
        GlobSet { matchers }
    }

    pub fn is_empty(&self) -> bool {
        self.matchers.is_empty()
    }

    pub fn matches(&self, path: &str) -> bool {
        self.matchers.iter().any(|(_, m)| m.is_match(path))
    }
}

/// One-shot glob match (compiles the pattern each call).
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    Glob::new(pattern)
        .map(|g| g.compile_matcher().is_match(path))
        .unwrap_or(false)
}
