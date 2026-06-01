// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Registry URL canonicalization shared by the credential read path
//! (`oci::access::registry_client`) and the write path (`auth::store`).
//!
//! Both paths MUST route through [`canonicalize_registry`] so a credential
//! written under the key `ghcr.io` by `grim login https://ghcr.io/v1/` is
//! found by a later read for `ghcr.io`. Single source of truth — prevents
//! drift between the helper-lookup key and the stored key.

/// Canonicalize a user-supplied registry argument into the key form used
/// by `~/.docker/config.json` `auths` / `credHelpers` / `credsStore`
/// lookups.
///
/// Algorithm (matches `docker/cli` `normalizeRegistry`):
/// 1. Strip a leading `http://` or `https://` scheme.
/// 2. Strip a trailing `/vN` or `/vN/` API-version suffix.
/// 3. Strip a trailing `/`.
/// 4. Special-case `docker.io` / `index.docker.io` →
///    `https://index.docker.io/v1/` for round-trip with `docker login`.
pub fn canonicalize_registry(input: &str) -> String {
    let stripped = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);

    let trimmed = strip_trailing_api_version(stripped);
    let trimmed = trimmed.trim_end_matches('/');

    if trimmed == "docker.io" || trimmed == "index.docker.io" {
        return "https://index.docker.io/v1/".to_string();
    }

    trimmed.to_string()
}

/// Strip a trailing `/vN` or `/vN/` segment where `N` is one or more
/// digits. Returns the input unchanged when no such suffix is present.
fn strip_trailing_api_version(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return s;
    }
    let mut cursor = bytes.len();
    if bytes[cursor - 1] == b'/' {
        cursor -= 1;
    }
    let digits_end = cursor;
    while cursor > 0 && bytes[cursor - 1].is_ascii_digit() {
        cursor -= 1;
    }
    if cursor == digits_end {
        return s; // no digits — not a /vN suffix
    }
    if cursor == 0 || bytes[cursor - 1] != b'v' {
        return s;
    }
    cursor -= 1; // consume 'v'
    if cursor == 0 || bytes[cursor - 1] != b'/' {
        return s;
    }
    &s[..cursor]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_registry_matches_docker_normalization() {
        let cases = [
            ("ghcr.io", "ghcr.io"),
            ("https://ghcr.io", "ghcr.io"),
            ("https://ghcr.io/", "ghcr.io"),
            ("https://ghcr.io/v1/", "ghcr.io"),
            ("https://ghcr.io/v2/", "ghcr.io"),
            ("http://localhost:5000", "localhost:5000"),
            ("docker.io", "https://index.docker.io/v1/"),
            ("index.docker.io", "https://index.docker.io/v1/"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                canonicalize_registry(input),
                expected,
                "canonicalize_registry({input:?}) should equal {expected:?}"
            );
        }
    }

    #[test]
    fn canonicalize_registry_preserves_non_version_paths() {
        // A path segment that merely looks numeric but is not a `/vN` suffix
        // must survive untouched.
        assert_eq!(canonicalize_registry("example.com/v"), "example.com/v");
        assert_eq!(canonicalize_registry("example.com/123"), "example.com/123");
    }
}
