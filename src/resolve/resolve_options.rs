// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Tuning knobs for a resolution pass.
//!
//! Adapted from OCX `project::resolve::ResolveLockOptions`. The per-
//! artifact timeout wraps the *entire* retry chain for one artifact;
//! `max_retries` counts retries after the initial attempt; `base_backoff`
//! is doubled on each retry.

use std::time::Duration;

/// Default timeout wrapping the full retry chain for one artifact.
pub const DEFAULT_PER_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default number of retries after the initial attempt.
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// Default backoff before the first retry; doubled each retry.
pub const DEFAULT_BASE_BACKOFF: Duration = Duration::from_millis(200);

/// Per-pass resolution tuning.
#[derive(Debug, Clone)]
pub struct ResolveOptions {
    /// Timeout wrapping the entire retry chain for a single artifact.
    pub per_artifact_timeout: Duration,
    /// Retries attempted after the initial attempt on transient failures.
    pub max_retries: u32,
    /// Backoff applied before the first retry; doubled each retry.
    pub base_backoff: Duration,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            per_artifact_timeout: DEFAULT_PER_ARTIFACT_TIMEOUT,
            max_retries: DEFAULT_MAX_RETRIES,
            base_backoff: DEFAULT_BASE_BACKOFF,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_documented_constants() {
        let o = ResolveOptions::default();
        assert_eq!(o.per_artifact_timeout, Duration::from_secs(30));
        assert_eq!(o.max_retries, 3);
        assert_eq!(o.base_backoff, Duration::from_millis(200));
    }
}
