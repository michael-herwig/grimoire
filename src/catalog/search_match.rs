// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The single shared search matcher for `grim search` and the TUI filter.
//!
//! A raw query string is parsed once into a [`SearchQuery`]: ASCII
//! whitespace splits it into tokens, each lowercased. A bare *kind keyword*
//! (`skill`/`skills`/`rule`/`rules`/`bundle`/`bundles`) is a kind **filter**
//! (never a literal text term); every other token is a text term. Matching
//! is an AND across all of them:
//!
//! - each text term must independently substring-match *any* of an entry's
//!   kind, repo, summary, description, or keywords (case-insensitive), and
//! - if any kind filter is present, the entry's kind must equal one of them.
//!
//! An empty / all-whitespace query matches everything.
//!
//! [`SearchQuery::prefilter_term`] derives the cheap repository-name
//! prefilter the bounded catalog build uses. A single substring cannot
//! express the full AND across multiple terms, but it can still narrow the
//! build *soundly*: because matching ANDs every term, any repo whose **name**
//! satisfies a multi-term query must contain every term, so prefiltering by
//! any one term is a superset of the name-matches — never dropping one. We
//! pick the **longest** term (the most selective substring) so a multi-term
//! query like `rust async` scopes the build to repos containing `async`
//! instead of falling back to the capped lexicographic browse window. The
//! in-memory matcher then re-applies the full AND.
//!
//! Trade-off: the prefilter only narrows by repository *name*. A query whose
//! terms match solely a summary/description/keyword (never the repo name) is
//! still served from the capped browse window, and the build records whether
//! that cap was hit (see [`super::registry_catalog::Catalog::truncated`]). A
//! kind-only query (no text terms) has no substring to scope by and likewise
//! takes the browse window.

use crate::oci::artifact_kind::ArtifactKind;

/// A parsed search query: lowercased text terms plus parsed kind filters.
///
/// Constructed via [`Self::parse`]; fields stay private so the parse rules
/// (kind-keyword extraction, lowercasing) are the single source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Lowercased text terms — each must match (AND) somewhere in an entry.
    terms: Vec<String>,
    /// Parsed kind filters from bare kind keywords. Non-empty ⇒ the entry's
    /// kind must equal one of these.
    kinds: Vec<ArtifactKind>,
}

impl SearchQuery {
    /// Parse `raw` into a query: split on ASCII whitespace, lowercase each
    /// token, then route bare kind keywords to [`Self::kinds`] and every
    /// other token to [`Self::terms`]. An empty / all-whitespace `raw`
    /// yields an empty query (matches everything).
    pub fn parse(raw: &str) -> Self {
        let mut terms = Vec::new();
        let mut kinds = Vec::new();
        for token in raw.split_whitespace() {
            let lowered = token.to_lowercase();
            if let Some(kind) = kind_keyword(&lowered) {
                kinds.push(kind);
            } else {
                terms.push(lowered);
            }
        }
        Self { terms, kinds }
    }

    /// Whether the query constrains nothing (no text terms and no kind
    /// filters) — i.e. it matches every entry.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.kinds.is_empty()
    }

    /// Whether this query matches an entry projected to its fields.
    ///
    /// Field-agnostic so both `CatalogEntry` and the TUI's `TuiRow` call it
    /// with borrowed views. Semantics:
    ///
    /// - an empty query matches everything;
    /// - if [`Self::kinds`] is non-empty, the entry's `kind` (lowercased)
    ///   must equal one of them (AND with the text terms);
    /// - each text term must independently substring-match (case-insensitive)
    ///   *any* of: kind, repo, summary, description, or any keyword.
    pub fn matches_fields(
        &self,
        kind: Option<&str>,
        repo: &str,
        summary: &str,
        description: &str,
        keywords: &[String],
    ) -> bool {
        if self.is_empty() {
            return true;
        }
        if !self.kinds.is_empty() {
            let kind_ok = kind
                .map(str::to_lowercase)
                .as_deref()
                .is_some_and(|k| self.kinds.iter().any(|wanted| wanted.to_string() == k));
            if !kind_ok {
                return false;
            }
        }
        self.terms.iter().all(|term| {
            kind.is_some_and(|k| k.to_lowercase().contains(term))
                || repo.to_lowercase().contains(term)
                || summary.to_lowercase().contains(term)
                || description.to_lowercase().contains(term)
                || keywords.iter().any(|k| k.to_lowercase().contains(term))
        })
    }

    /// The repository-name prefilter for the bounded catalog build.
    ///
    /// Returns the **longest** text term, which soundly narrows the build:
    /// matching ANDs every term, so any repo whose *name* satisfies the query
    /// must contain each term — prefiltering by one term yields a superset of
    /// the name-matches and drops none. The longest term is the most selective
    /// substring, so it scopes a multi-term query (e.g. `rust async`) far
    /// tighter than the lexicographic browse window an empty prefilter forces.
    ///
    /// Returns the empty string (⇒ capped browse window) only when there is no
    /// text term to scope by: a kind-only query, or an empty query. Terms of
    /// equal length tie-break to the first, keeping the result deterministic.
    ///
    /// The in-memory matcher always re-applies the full AND, so a prefilter
    /// that is broader than the query (terms matching summary/description/
    /// keywords rather than the name) only affects build *coverage*, never
    /// correctness of the rows that survive.
    pub fn prefilter_term(&self) -> &str {
        // `fold` keeps the first term of any equal-length tie (a strict `>`
        // never replaces an equally-long earlier term), unlike `max_by_key`
        // which would keep the last — so the prefilter is order-stable.
        self.terms
            .iter()
            .fold("", |best: &str, term| if term.len() > best.len() { term } else { best })
    }
}

/// Map a lowercased token to a kind filter, accepting both singular and
/// plural spellings (`skill`/`skills`, `rule`/`rules`, `bundle`/`bundles`).
/// `None` for any other token (it is a text term).
fn kind_keyword(token: &str) -> Option<ArtifactKind> {
    // Strip a single trailing plural `s`, then delegate to the canonical
    // singular parser so the six spellings share one mapping.
    let singular = token.strip_suffix('s').unwrap_or(token);
    ArtifactKind::from_kind_str(singular)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parse_splits_on_whitespace_and_lowercases() {
        let q = SearchQuery::parse("  Rust   LINT  ");
        assert_eq!(q.terms, vec!["rust".to_string(), "lint".to_string()]);
        assert!(q.kinds.is_empty());
    }

    #[test]
    fn empty_query_is_empty_and_matches_all() {
        let q = SearchQuery::parse("   ");
        assert!(q.is_empty());
        assert!(q.matches_fields(Some("skill"), "acme/x", "", "", &[]));
        assert!(SearchQuery::parse("").is_empty());
    }

    #[test]
    fn single_term_substring_match_across_fields() {
        let q = SearchQuery::parse("review");
        assert!(q.matches_fields(Some("skill"), "acme/code-review", "", "", &[]), "repo");
        assert!(
            q.matches_fields(Some("skill"), "acme/x", "code review skill", "", &[]),
            "summary"
        );
        assert!(
            q.matches_fields(Some("skill"), "acme/x", "", "do a review", &[]),
            "description"
        );
        assert!(
            q.matches_fields(Some("skill"), "acme/x", "", "", &kw(&["review"])),
            "keyword"
        );
        assert!(
            !q.matches_fields(Some("skill"), "acme/x", "", "", &kw(&["lint"])),
            "no match"
        );
    }

    #[test]
    fn term_matches_kind_field_too() {
        // A non-keyword text term may substring-match the kind field.
        let q = SearchQuery::parse("ski");
        assert!(
            q.matches_fields(Some("skill"), "acme/x", "", "", &[]),
            "kind in haystack"
        );
        assert!(!q.matches_fields(Some("rule"), "acme/x", "", "", &[]));
    }

    #[test]
    fn multi_term_is_and() {
        let q = SearchQuery::parse("rust lint");
        // Both terms present (one in repo, one in keywords).
        assert!(q.matches_fields(Some("rule"), "acme/rust-style", "", "", &kw(&["lint"])));
        // Only one term present ⇒ no match.
        assert!(!q.matches_fields(Some("rule"), "acme/rust-style", "", "", &kw(&["quality"])));
        assert!(!q.matches_fields(Some("rule"), "acme/python", "", "", &kw(&["lint"])));
    }

    #[test]
    fn case_insensitive_across_every_field() {
        let q = SearchQuery::parse("REVIEW QUALITY");
        assert!(q.matches_fields(Some("SKILL"), "ACME/CODE-REVIEW", "QUALITY blurb", "", &[]));
    }

    #[test]
    fn multi_term_ands_summary_and_keyword() {
        // One term lands only in the summary, the other only in keywords —
        // both must hit for the AND to pass.
        let q = SearchQuery::parse("terse lint");
        assert!(q.matches_fields(Some("rule"), "acme/x", "terse blurb", "", &kw(&["lint"])));
        assert!(!q.matches_fields(Some("rule"), "acme/x", "terse blurb", "", &kw(&["fmt"])));
    }

    #[test]
    fn bare_kind_keyword_filters_by_kind() {
        let q = SearchQuery::parse("rule");
        assert!(q.kinds == vec![ArtifactKind::Rule]);
        assert!(q.terms.is_empty());
        assert!(q.matches_fields(Some("rule"), "acme/x", "", "", &[]), "rule entry");
        assert!(
            !q.matches_fields(Some("skill"), "acme/x", "", "", &[]),
            "skill filtered out"
        );
        // A kindless entry never satisfies a kind filter.
        assert!(!q.matches_fields(None, "acme/x", "", "", &[]));
    }

    #[test]
    fn plural_kind_keywords_map_to_kinds() {
        assert_eq!(SearchQuery::parse("skills").kinds, vec![ArtifactKind::Skill]);
        assert_eq!(SearchQuery::parse("rules").kinds, vec![ArtifactKind::Rule]);
        assert_eq!(SearchQuery::parse("bundles").kinds, vec![ArtifactKind::Bundle]);
        // Singular spellings too.
        assert_eq!(SearchQuery::parse("skill").kinds, vec![ArtifactKind::Skill]);
        assert_eq!(SearchQuery::parse("bundle").kinds, vec![ArtifactKind::Bundle]);
    }

    #[test]
    fn kind_keyword_and_text_term_is_and() {
        // `skill review` = kind==skill AND a text term `review` matches.
        let q = SearchQuery::parse("skill review");
        assert_eq!(q.kinds, vec![ArtifactKind::Skill]);
        assert_eq!(q.terms, vec!["review".to_string()]);
        assert!(
            q.matches_fields(Some("skill"), "acme/code-review", "", "", &[]),
            "skill + term"
        );
        // Right kind, wrong term.
        assert!(!q.matches_fields(Some("skill"), "acme/lint", "", "", &[]));
        // Right term, wrong kind.
        assert!(!q.matches_fields(Some("rule"), "acme/code-review", "", "", &[]));
    }

    #[test]
    fn kind_only_query_matching_nothing_yields_no_match() {
        // A bundle filter against a registry that lists none ⇒ empty, never
        // a fallback to literal-term matching.
        let q = SearchQuery::parse("bundle");
        assert!(!q.is_empty());
        assert!(!q.matches_fields(Some("skill"), "acme/bundle-ish", "bundle words", "", &[]));
        assert!(!q.matches_fields(Some("rule"), "acme/x", "", "", &[]));
    }

    #[test]
    fn prefilter_term_is_the_sole_text_term() {
        assert_eq!(SearchQuery::parse("review").prefilter_term(), "review");
        assert_eq!(SearchQuery::parse("  Review ").prefilter_term(), "review");
    }

    #[test]
    fn prefilter_term_picks_the_longest_term_for_multi_term() {
        // The longest (most selective) term scopes the build instead of the
        // capped browse window an empty prefilter would force.
        assert_eq!(SearchQuery::parse("rust async").prefilter_term(), "async");
        assert_eq!(SearchQuery::parse("a longest mid").prefilter_term(), "longest");
        // Case-folded by the parser, so the prefilter is already lowercase.
        assert_eq!(SearchQuery::parse("Go ASYNCHRONY").prefilter_term(), "asynchrony");
    }

    #[test]
    fn prefilter_term_ties_break_to_first_for_determinism() {
        // Equal-length terms must resolve deterministically to the first.
        assert_eq!(SearchQuery::parse("lint rust").prefilter_term(), "lint");
        assert_eq!(SearchQuery::parse("rust lint").prefilter_term(), "rust");
    }

    #[test]
    fn prefilter_term_empty_for_zero_text_terms() {
        // No text term to scope by ⇒ empty prefilter ⇒ capped browse window.
        assert_eq!(SearchQuery::parse("").prefilter_term(), "");
        // Kind-only query: the kind keyword is not a text term and the kind
        // string is not in the repo name, so there is nothing to scope by.
        assert_eq!(SearchQuery::parse("rule").prefilter_term(), "");
    }

    #[test]
    fn prefilter_term_uses_longest_text_term_alongside_a_kind_filter() {
        // A kind filter does not suppress the prefilter: the text term still
        // scopes the build by name. The in-memory matcher re-applies the kind
        // filter and the full AND, so build coverage improves without changing
        // which rows survive.
        assert_eq!(SearchQuery::parse("skill review").prefilter_term(), "review");
        assert_eq!(
            SearchQuery::parse("rules formatting lint").prefilter_term(),
            "formatting"
        );
    }
}
