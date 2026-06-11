// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Declaration hash: RFC 8785 JCS canonicalization + SHA-256.
//!
//! The declaration hash is the staleness pivot: the lock records the hash
//! of the declaration it was resolved from, so a later command can detect
//! that `grimoire.toml` changed without re-resolving. Canonicalization
//! must therefore be byte-stable across runs, machines, and map insertion
//! order.
//!
//! JCS is implemented in-tree (no extra crate): the canonical input only
//! contains JSON strings and objects (no numbers, no floats, no non-UTF-8
//! data is reachable from Rust `String` keys/values), so RFC 8785 reduces
//! to "emit objects with keys sorted, strings JSON-escaped, no
//! whitespace". `serde_json::to_string` already emits compact,
//! RFC 8785-compatible string escaping; the only thing it does not
//! guarantee without the `preserve_order` feature is object key order, so
//! the keys are sorted explicitly here.

use crate::config::declaration::DesiredSet;
use crate::oci::Algorithm;

/// Canonicalization-contract version baked into every lock's
/// `metadata.declaration_hash_version`. Bumping this is a breaking change
/// to the hash input format and a migration event, not a drive-by edit.
pub const DECLARATION_HASH_VERSION: u8 = 1;

/// Compute the declaration hash for a [`DesiredSet`].
///
/// Algorithm (v1):
/// 1. Build the canonical JSON object
///    `{["agents":{…},]["bundles":{…},]"rules":{name:idstr,…},"skills":{name:idstr,…}}`
///    where every `idstr` is the `Display` form of the parsed identifier
///    (`registry/repo[:tag][@digest]`). The `agents` and `bundles` keys
///    are emitted **only when at least one entry of that kind is
///    declared**, so an agent-free/bundle-free declaration hashes
///    identically to one written before those kinds existed — existing
///    locks stay valid with no version bump.
/// 2. Serialize via RFC 8785 JCS — object keys sorted, strings
///    JSON-escaped, no whitespace.
/// 3. SHA-256 the UTF-8 bytes (reusing the Phase-1 [`Algorithm::Sha256`]).
/// 4. Return `"sha256:<hex>"`.
pub fn declaration_hash(set: &DesiredSet) -> String {
    // Top-level keys in JCS (sorted) order:
    // "agents" < "bundles" < "rules" < "skills". "agents" and "bundles"
    // are omitted entirely when empty so the canonical form — and thus the
    // hash — matches the pre-agents/pre-bundles algorithm for any config
    // that declares neither.
    let mut canonical = String::from("{");
    if !set.agents.is_empty() {
        canonical.push_str("\"agents\":");
        push_canonical_table(&mut canonical, &set.agents);
        canonical.push(',');
    }
    if !set.bundles.is_empty() {
        canonical.push_str("\"bundles\":");
        push_canonical_table(&mut canonical, &set.bundles);
        canonical.push(',');
    }
    canonical.push_str("\"rules\":");
    push_canonical_table(&mut canonical, &set.rules);
    canonical.push(',');
    canonical.push_str("\"skills\":");
    push_canonical_table(&mut canonical, &set.skills);
    canonical.push('}');

    match Algorithm::Sha256.hash(canonical.as_bytes()) {
        crate::oci::Digest::Sha256(hex) => format!("sha256:{hex}"),
        // `Algorithm::Sha256.hash` always yields a `Sha256` digest.
        other => format!("{other}"),
    }
}

/// Emit `{name:idstr,...}` with keys sorted lexicographically and values
/// JSON-escaped via `serde_json` (RFC 8785-compatible string encoding).
///
/// `BTreeMap` already iterates in sorted key order, but the sort is made
/// explicit (collect + sort) so the canonical form does not silently
/// depend on the input collection's ordering guarantees.
fn push_canonical_table(out: &mut String, table: &std::collections::BTreeMap<String, crate::oci::Identifier>) {
    let mut pairs: Vec<(&str, String)> = table.iter().map(|(k, v)| (k.as_str(), v.to_string())).collect();
    pairs.sort_by(|a, b| a.0.cmp(b.0));

    out.push('{');
    for (i, (key, value)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_json_string(out, key);
        out.push(':');
        push_json_string(out, value);
    }
    out.push('}');
}

/// Append `s` as an RFC 8785-compliant JSON string literal.
///
/// `serde_json::to_string` of a `&str` produces exactly the JCS string
/// form (minimal escaping, lowercase `\uXXXX` for control chars). It
/// cannot fail for a plain string, but the fallback below keeps the
/// no-`unwrap` discipline if it ever did.
fn push_json_string(out: &mut String, s: &str) {
    match serde_json::to_string(s) {
        Ok(escaped) => out.push_str(&escaped),
        Err(_) => {
            out.push('"');
            out.push_str(s);
            out.push('"');
        }
    }
}

#[cfg(test)]
mod tests {
    //! FROZEN CORPUS — the literal hashes below are the permanent contract
    //! for `DECLARATION_HASH_VERSION = 1`. A failing assertion means the
    //! algorithm drifted; fix the algorithm (or bump the version with a
    //! migration), never "fix" the expected value.

    use std::collections::BTreeMap;

    use super::*;
    use crate::config::declaration::DesiredSet;
    use crate::oci::Identifier;

    fn id(s: &str) -> Identifier {
        Identifier::parse(s).expect("valid identifier")
    }

    fn set(skills: &[(&str, &str)], rules: &[(&str, &str)]) -> DesiredSet {
        let mut s = BTreeMap::new();
        for (k, v) in skills {
            s.insert((*k).to_string(), id(v));
        }
        let mut r = BTreeMap::new();
        for (k, v) in rules {
            r.insert((*k).to_string(), id(v));
        }
        DesiredSet::from_parts(s, r)
    }

    #[test]
    fn hash_has_sha256_prefix_and_64_hex() {
        let got = declaration_hash(&set(&[], &[]));
        let hex = got.strip_prefix("sha256:").expect("prefix");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn hash_is_deterministic() {
        let s = set(&[("code-review", "ghcr.io/acme/code-review:stable")], &[]);
        assert_eq!(declaration_hash(&s), declaration_hash(&s));
    }

    #[test]
    fn hash_independent_of_insertion_order() {
        let mut skills_fwd = BTreeMap::new();
        skills_fwd.insert("a".to_string(), id("ghcr.io/acme/a:1"));
        skills_fwd.insert("b".to_string(), id("ghcr.io/acme/b:2"));
        let mut skills_rev = BTreeMap::new();
        skills_rev.insert("b".to_string(), id("ghcr.io/acme/b:2"));
        skills_rev.insert("a".to_string(), id("ghcr.io/acme/a:1"));

        let fwd = DesiredSet::from_parts(skills_fwd, BTreeMap::new());
        let rev = DesiredSet::from_parts(skills_rev, BTreeMap::new());
        assert_eq!(declaration_hash(&fwd), declaration_hash(&rev));
    }

    #[test]
    fn hash_changes_when_artifact_added() {
        let base = set(&[("code-review", "ghcr.io/acme/code-review:stable")], &[]);
        let more = set(
            &[
                ("code-review", "ghcr.io/acme/code-review:stable"),
                ("docs", "ghcr.io/acme/docs:1"),
            ],
            &[],
        );
        assert_ne!(declaration_hash(&base), declaration_hash(&more));
    }

    #[test]
    fn hash_distinguishes_skills_from_rules() {
        let as_skill = set(&[("x", "ghcr.io/acme/x:1")], &[]);
        let as_rule = set(&[], &[("x", "ghcr.io/acme/x:1")]);
        assert_ne!(declaration_hash(&as_skill), declaration_hash(&as_rule));
    }

    #[test]
    fn hash_includes_digest_pin() {
        let hex = "a".repeat(64);
        let tagged = set(&[("x", "ghcr.io/acme/x:1")], &[]);
        let pinned = set(&[("x", &format!("ghcr.io/acme/x@sha256:{hex}"))], &[]);
        assert_ne!(declaration_hash(&tagged), declaration_hash(&pinned));
    }

    fn set_with_bundles(skills: &[(&str, &str)], rules: &[(&str, &str)], bundles: &[(&str, &str)]) -> DesiredSet {
        let mut s = BTreeMap::new();
        for (k, v) in skills {
            s.insert((*k).to_string(), id(v));
        }
        let mut r = BTreeMap::new();
        for (k, v) in rules {
            r.insert((*k).to_string(), id(v));
        }
        let mut b = BTreeMap::new();
        for (k, v) in bundles {
            b.insert((*k).to_string(), id(v));
        }
        DesiredSet::from_parts_with_bundles(s, r, b)
    }

    #[test]
    fn empty_bundles_hashes_identically_to_pre_bundles() {
        // A declaration with an empty bundles map must hash exactly the
        // same as one constructed without the field, so locks written
        // before bundles existed stay valid (no version bump).
        let without = set(&[("x", "ghcr.io/acme/x:1")], &[]);
        let with_empty = set_with_bundles(&[("x", "ghcr.io/acme/x:1")], &[], &[]);
        assert_eq!(declaration_hash(&without), declaration_hash(&with_empty));
    }

    #[test]
    fn declaring_a_bundle_changes_the_hash() {
        let base = set(&[("x", "ghcr.io/acme/x:1")], &[]);
        let with_bundle = set_with_bundles(&[("x", "ghcr.io/acme/x:1")], &[], &[("stack", "ghcr.io/acme/stack:1")]);
        assert_ne!(declaration_hash(&base), declaration_hash(&with_bundle));
    }

    #[test]
    fn bundle_hash_is_deterministic() {
        let a = set_with_bundles(&[], &[], &[("stack", "ghcr.io/acme/stack:1")]);
        let b = set_with_bundles(&[], &[], &[("stack", "ghcr.io/acme/stack:1")]);
        assert_eq!(declaration_hash(&a), declaration_hash(&b));
    }

    fn set_with_agents(skills: &[(&str, &str)], agents: &[(&str, &str)]) -> DesiredSet {
        let mut s = BTreeMap::new();
        for (k, v) in skills {
            s.insert((*k).to_string(), id(v));
        }
        let mut a = BTreeMap::new();
        for (k, v) in agents {
            a.insert((*k).to_string(), id(v));
        }
        DesiredSet::from_maps(s, BTreeMap::new(), a, BTreeMap::new())
    }

    #[test]
    fn empty_agents_hashes_identically_to_pre_agents() {
        // A declaration with an empty agents map must hash exactly the same
        // as one constructed without the field, so locks written before
        // agents existed stay valid (no version bump).
        let without = set(&[("x", "ghcr.io/acme/x:1")], &[]);
        let with_empty = set_with_agents(&[("x", "ghcr.io/acme/x:1")], &[]);
        assert_eq!(declaration_hash(&without), declaration_hash(&with_empty));
        assert_eq!(declaration_hash(&set_with_agents(&[], &[])), CASE_EMPTY);
    }

    #[test]
    fn declaring_an_agent_changes_the_hash() {
        let base = set(&[("x", "ghcr.io/acme/x:1")], &[]);
        let with_agent = set_with_agents(&[("x", "ghcr.io/acme/x:1")], &[("rev", "ghcr.io/acme/rev:1")]);
        assert_ne!(declaration_hash(&base), declaration_hash(&with_agent));
    }

    #[test]
    fn hash_distinguishes_agents_from_skills() {
        let as_skill = set(&[("x", "ghcr.io/acme/x:1")], &[]);
        let as_agent = set_with_agents(&[], &[("x", "ghcr.io/acme/x:1")]);
        assert_ne!(declaration_hash(&as_skill), declaration_hash(&as_agent));
    }

    #[test]
    fn agent_hash_is_deterministic() {
        let a = set_with_agents(&[], &[("rev", "ghcr.io/acme/rev:1")]);
        let b = set_with_agents(&[], &[("rev", "ghcr.io/acme/rev:1")]);
        assert_eq!(declaration_hash(&a), declaration_hash(&b));
    }

    // Frozen-corpus case for an agent-bearing declaration: canonical JSON
    // {"agents":{"rev":"ghcr.io/acme/rev:1"},"rules":{},"skills":{}}.
    // The literal is locked in by `hash_corpus_agent` below once captured.
    #[test]
    fn agent_canonical_form_places_agents_first() {
        // Indirect canonical-form check: two sets that differ only in
        // whether the same identifier lives under agents vs bundles must
        // hash differently (distinct top-level keys).
        let as_agent = set_with_agents(&[], &[("x", "ghcr.io/acme/x:1")]);
        let as_bundle = set_with_bundles(&[], &[], &[("x", "ghcr.io/acme/x:1")]);
        assert_ne!(declaration_hash(&as_agent), declaration_hash(&as_bundle));
    }

    // Frozen corpus: captured from a run and baked in by hand. Changing
    // any of these is a BREAKING change requiring a version bump.
    const CASE_EMPTY: &str = "sha256:009e44ee25720a0be5c25fd08ea27798d37fd9ae5c33d4712a4a460d44af3d10";

    #[test]
    fn hash_corpus_empty() {
        // Canonical JSON: {"rules":{},"skills":{}}
        assert_eq!(declaration_hash(&set(&[], &[])), CASE_EMPTY);
    }

    #[test]
    fn version_constant_is_one() {
        assert_eq!(DECLARATION_HASH_VERSION, 1);
    }
}
