// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Anchor-relativized install paths: store each materialized target as a
//! `(anchor, relative)` pair instead of an absolute path so the install
//! state is portable across machines (shared `$GRIM_HOME`, devcontainers
//! with a different `$HOME`).
//!
//! A [`PathAnchor`] names a logical root (the workspace, a vendor's native
//! config dir, `$GRIM_HOME`). [`AnchorRoots`] resolves every anchor's
//! concrete on-disk root **once** at scope-resolution time, so
//! [`PathAnchor::root`] is a pure table lookup (no ambient env at resolve
//! time → unit-testable without env). An [`AnchoredPath`] re-joins the two
//! through a two-layer containment guard ([`AnchoredPath::resolve`]) that
//! never lets a tampered `relative` escape its anchor root.

use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::scope::ConfigScope;
use crate::context::Context;
use crate::install::client_target::ClientTarget;
use crate::install::vendor::{env_dir, home_dir, xdg_config_dir};
use crate::install::{vendor_claude, vendor_codex, vendor_copilot, vendor_opencode};
use crate::oci::ArtifactKind;

/// A logical root an install target is stored relative to.
///
/// Serialized as a kebab-case string tag (human-readable, forward-additive
/// JSON). Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathAnchor {
    /// Project scope: `<workspace>/…` (project state only).
    Workspace,
    /// Global Claude skills + rules: `$CLAUDE_CONFIG_DIR` else `~/.claude`.
    ClaudeRoot,
    /// Global Copilot skills: `$COPILOT_HOME` else `~/.copilot`.
    CopilotRoot,
    /// Global OpenCode skills: `$OPENCODE_CONFIG_DIR/skills` else
    /// `$XDG_CONFIG_HOME`|`~/.config`/opencode/skills.
    OpenCodeSkills,
    /// Global OpenCode config dir (the parent of [`Self::OpenCodeSkills`]):
    /// hosts the sibling `agents/` dir, so a global OpenCode agent lands at
    /// `<opencode-root>/agents/<name>.md`. Derived as the parent of the
    /// resolved skills root — no separate `AnchorRoots` field.
    OpenCodeRoot,
    /// `$GRIM_HOME`: the global OpenCode rules dir and the inert global
    /// Copilot rules path.
    GrimHome,
    /// Global Codex skills: the cross-vendor open standard `$HOME/.agents/skills`
    /// (keyed on `$HOME`, **not** relocated by `$CODEX_HOME`).
    AgentsSkills,
    /// Global Codex config root: `$CODEX_HOME` else `~/.codex`. Hosts the
    /// `agents/` dir, so a global Codex agent lands at
    /// `<codex-root>/agents/<name>.toml`.
    CodexRoot,
}

impl std::fmt::Display for PathAnchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Workspace => "workspace",
            Self::ClaudeRoot => "claude-root",
            Self::CopilotRoot => "copilot-root",
            Self::OpenCodeSkills => "opencode-skills",
            Self::OpenCodeRoot => "opencode-root",
            Self::GrimHome => "grim-home",
            Self::AgentsSkills => "agents-skills",
            Self::CodexRoot => "codex-root",
        })
    }
}

/// Every anchor's concrete on-disk root, resolved once at scope-resolution
/// time so [`PathAnchor::root`] is a pure lookup (no ambient env reads, no
/// I/O at resolve time).
///
/// `claude_root` / `copilot_root` / `opencode_skills` are `None` when the
/// vendor root is unresolvable (neither the vendor env override nor `$HOME`
/// / `$XDG_CONFIG_HOME`).
///
/// **`OpenCodeRoot` has NO field here.** It is derived at lookup time as
/// `opencode_skills.as_ref().and_then(|s| s.parent())` — a structurally
/// derivable relationship that does not need its own stored root.
/// New anchors whose root is derivable from an existing field (e.g. a
/// parent or sibling of a stored path) should follow this pattern rather
/// than adding a new `Option<PathBuf>` field to this struct.
pub struct AnchorRoots {
    /// The workspace root project-scope targets are rooted at.
    pub workspace: PathBuf,
    /// `$GRIM_HOME`.
    pub grim_home: PathBuf,
    /// The global Claude root, when resolvable.
    pub claude_root: Option<PathBuf>,
    /// The global Copilot native root, when resolvable.
    pub copilot_root: Option<PathBuf>,
    /// The global OpenCode skills root, when resolvable. The OpenCode config
    /// root ([`PathAnchor::OpenCodeRoot`]) is derived as the parent of this
    /// path — no separate field is needed.
    pub opencode_skills: Option<PathBuf>,
    /// The global Codex skills root (`$HOME/.agents/skills`), when resolvable.
    pub agents_skills: Option<PathBuf>,
    /// The global Codex config root (`$CODEX_HOME` else `~/.codex`), when
    /// resolvable. Hosts the sibling `agents/` dir.
    pub codex_root: Option<PathBuf>,
}

impl AnchorRoots {
    /// Resolve every anchor root once, calling the vendor helpers with the
    /// same env inputs the materializer uses (single source of truth). The
    /// resolved set is then a pure lookup table for [`PathAnchor::root`].
    pub fn resolve(workspace: PathBuf, ctx: &Context) -> Self {
        Self {
            workspace,
            grim_home: ctx.grim_home().to_path_buf(),
            claude_root: vendor_claude::global_root(env_dir("CLAUDE_CONFIG_DIR"), home_dir()),
            copilot_root: vendor_copilot::global_native_root(env_dir("COPILOT_HOME"), home_dir()),
            opencode_skills: vendor_opencode::global_skills_root(env_dir("OPENCODE_CONFIG_DIR"), xdg_config_dir()),
            agents_skills: vendor_codex::global_skills_root(home_dir()),
            codex_root: vendor_codex::codex_root(env_dir("CODEX_HOME"), home_dir()),
        }
    }
}

impl PathAnchor {
    /// The concrete on-disk root for this anchor — a pure lookup into the
    /// pre-resolved [`AnchorRoots`] (no env reads, no I/O). `None` when the
    /// anchor's vendor root is unresolvable.
    pub fn root(self, roots: &AnchorRoots) -> Option<PathBuf> {
        match self {
            Self::Workspace => Some(roots.workspace.clone()),
            Self::GrimHome => Some(roots.grim_home.clone()),
            Self::ClaudeRoot => roots.claude_root.clone(),
            Self::CopilotRoot => roots.copilot_root.clone(),
            Self::OpenCodeSkills => roots.opencode_skills.clone(),
            // The OpenCode config dir is the parent of the skills root; the
            // sibling `agents/` dir lives directly under it.
            Self::OpenCodeRoot => roots
                .opencode_skills
                .as_ref()
                .and_then(|s| s.parent())
                .map(std::path::Path::to_path_buf),
            Self::AgentsSkills => roots.agents_skills.clone(),
            Self::CodexRoot => roots.codex_root.clone(),
        }
    }
}

/// An install target stored as `(anchor, relative)` for portability.
///
/// The `relative` remainder is forward-slash UTF-8 and Normal-only —
/// no `CurDir` (`.`), `ParentDir` (`..`), `RootDir`, or `Prefix`
/// component, never absolute, never empty. The invariant is asserted at
/// store time ([`Self::from_target`]) and re-checked at first use
/// ([`Self::resolve`]). Deserialization does **not** re-validate (bare
/// `String` + `deny_unknown_fields`); the resolve-time guard catches a
/// tampered remainder that passes JSON parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchoredPath {
    /// The logical root this path is relative to.
    pub anchor: PathAnchor,
    /// Forward-slash UTF-8 remainder. Invariant: every component is
    /// `Normal` — see the type doc.
    pub relative: String,
}

impl AnchoredPath {
    /// Classify an absolute install target into `(anchor, relative)`.
    ///
    /// Tries the scope/client/kind candidate anchors longest-root-first and
    /// returns the first whose resolved root is a `Path`-level prefix of
    /// `abs`; the remainder is stored forward-slash with any `CurDir`
    /// component stripped and asserted Normal-only and non-empty.
    ///
    /// CALLER INVARIANT: `abs` MUST be the non-canonicalized (pre-symlink)
    /// form, built as `root.join(relative)` with no intervening
    /// canonicalize. Passing a canonicalized `abs` may yield
    /// [`AnchorError::UnknownAnchor`].
    ///
    /// # Errors
    ///
    /// [`AnchorError::UnknownAnchor`] when no candidate root prefixes `abs`.
    pub fn from_target(
        abs: &Path,
        scope: ConfigScope,
        client: ClientTarget,
        kind: ArtifactKind,
        roots: &AnchorRoots,
    ) -> Result<AnchoredPath, AnchorError> {
        // Build the closed candidate set for this (scope, client, kind) from
        // the §1.1 root/remainder table, then try them longest-root-first so
        // a more specific root (e.g. a vendor root nested under GrimHome in a
        // hermetic layout) wins over a shorter prefix.
        let mut candidates: Vec<(PathAnchor, PathBuf)> = candidate_anchors(scope, client, kind)
            .into_iter()
            .filter_map(|anchor| anchor.root(roots).map(|root| (anchor, root)))
            .collect();
        candidates.sort_by_key(|(_, root)| std::cmp::Reverse(root.components().count()));

        for (anchor, root) in candidates {
            if let Some(relative) = strip_prefix_relative(abs, &root) {
                return Ok(AnchoredPath { anchor, relative });
            }
        }

        Err(AnchorError::UnknownAnchor {
            path: abs.to_path_buf(),
        })
    }

    /// Re-join anchor + relative into an absolute on-disk path, guaranteed
    /// contained under the anchor root.
    ///
    /// Layer 1 (always): reject any component that is not `Normal`
    /// (`ParentDir`, `RootDir`, `Prefix`, `CurDir`) or an empty `relative`
    /// (the anchor root itself) → [`AnchorError::TraversalAttempt`]. Works
    /// for absent paths.
    /// Layer 2 (when the candidate exists OR is a symlink, dangling
    /// included): `dunce::canonicalize` both sides and assert
    /// `Path::starts_with` (component-granular, never str) →
    /// [`AnchorError::EscapedAnchor`]. When Layer 2 fires, the
    /// **canonicalized** path is returned (not the raw join), closing the
    /// TOCTOU window so callers always act on the validated, symlink-resolved
    /// path. A dangling symlink (target absent) fails canonicalize and
    /// surfaces as [`AnchorError::Io`] — never a silent `Ok` of the raw join.
    /// [`AnchorError::AnchorRootAbsent`] when `self.anchor.root(roots)` is
    /// `None`.
    ///
    /// # Residual TOCTOU
    ///
    /// Containment is validated at check time and the returned path is the
    /// canonicalized, check-time-contained form — callers always operate on
    /// that path, never the raw `relative`. A *fully* TOCTOU-proof guarantee
    /// against an intermediate-directory symlink swapped between this call
    /// and the caller's filesystem op would require handle-based resolution
    /// (`openat` / `O_NOFOLLOW` walking each component). That is out of scope
    /// for v1 given the threat model: grim manages the user's own config
    /// dirs, so an attacker who can swap a directory under those roots
    /// already has the privileges the guard protects. The two-layer guard
    /// addresses the realistic case (a tampered stored `relative` or a
    /// symlink already present at check time), not a same-uid racing
    /// adversary.
    ///
    /// # Errors
    ///
    /// See the variant list above.
    pub fn resolve(&self, roots: &AnchorRoots) -> Result<PathBuf, AnchorError> {
        let root = self
            .anchor
            .root(roots)
            .ok_or(AnchorError::AnchorRootAbsent { anchor: self.anchor })?;

        // Layer 1 (always, even for absent paths): the stored remainder is
        // untrusted at read time. Reject every component that is not
        // `Normal` — `ParentDir`/`RootDir`/`Prefix`/`CurDir` — so a tampered
        // `..`, leading `/`, `.`, or drive prefix can never escape the root.
        // Also reject an empty remainder (the root itself) — rooting at the
        // anchor itself is dangerous.
        let relative = Path::new(&self.relative);
        if self.relative.is_empty() {
            return Err(AnchorError::TraversalAttempt {
                relative: self.relative.clone(),
            });
        }
        for component in relative.components() {
            if !matches!(component, Component::Normal(_)) {
                return Err(AnchorError::TraversalAttempt {
                    relative: self.relative.clone(),
                });
            }
        }

        let candidate = root.join(relative);

        // Layer 2 (when the candidate exists OR is a symlink): a symlink in
        // the tree could still route a Normal-only path outside the root.
        // `exists()` is `false` for a DANGLING symlink (target absent), so we
        // also test `is_symlink()` — otherwise the guard would be skipped and
        // `Ok(root.join(symlink))` returned unvalidated. For a dangling
        // symlink the canonicalize below fails, yielding a safe `Io` error.
        // Canonicalize both sides (`dunce` avoids `\\?\` UNC false-negatives
        // on Windows) and assert containment component-by-component via
        // `Path::starts_with` — never a string prefix. Return the
        // canonicalized path so callers act on the validated, resolved path.
        if candidate.exists() || candidate.is_symlink() {
            let canon_root = dunce::canonicalize(&root).map_err(|source| AnchorError::Io {
                path: root.clone(),
                source,
            })?;
            let canon_candidate = dunce::canonicalize(&candidate).map_err(|source| AnchorError::Io {
                path: candidate.clone(),
                source,
            })?;
            if !canon_candidate.starts_with(&canon_root) {
                return Err(AnchorError::EscapedAnchor {
                    anchor: self.anchor,
                    resolved: canon_candidate,
                });
            }
            // Return the canonicalized path (not the raw join) to close the
            // TOCTOU window: the caller acts on the symlink-resolved path that
            // was verified to be within the anchor root.
            return Ok(canon_candidate);
        }

        Ok(candidate)
    }
}

/// The closed candidate anchor set for a `(scope, client, kind)` install
/// target, from the §1.1 root/remainder table.
///
/// Project scope is always `[Workspace]` — a project target that does not
/// fall under the workspace is an [`AnchorError::UnknownAnchor`], never a
/// silently absolute path. Global scope uses an explicit match over every
/// `(client, kind)` combination so that a future new `ClientTarget` or
/// `ArtifactKind` variant fails to compile here rather than silently
/// anchoring to `GrimHome`. `Bundle` arms are `unreachable!()` because
/// bundles are always expanded into members and never materialized. The
/// caller tries them longest-root-first so the more specific root wins.
fn candidate_anchors(scope: ConfigScope, client: ClientTarget, kind: ArtifactKind) -> Vec<PathAnchor> {
    match scope {
        ConfigScope::Project => vec![PathAnchor::Workspace],
        ConfigScope::Global => {
            let primary = match (client, kind) {
                // Claude: all three materializable kinds live under the Claude root.
                (ClientTarget::Claude, ArtifactKind::Skill)
                | (ClientTarget::Claude, ArtifactKind::Rule)
                | (ClientTarget::Claude, ArtifactKind::Agent) => PathAnchor::ClaudeRoot,

                // Copilot: skills and agents live under the native $COPILOT_HOME root.
                (ClientTarget::Copilot, ArtifactKind::Skill) | (ClientTarget::Copilot, ArtifactKind::Agent) => {
                    PathAnchor::CopilotRoot
                }

                // Copilot: rules (inert) live under $GRIM_HOME — no native user-level
                // instructions path for Copilot at global scope.
                (ClientTarget::Copilot, ArtifactKind::Rule) => PathAnchor::GrimHome,

                // OpenCode: skills live under the OpenCode skills root.
                (ClientTarget::OpenCode, ArtifactKind::Skill) => PathAnchor::OpenCodeSkills,

                // OpenCode: agents live in the sibling `agents/` dir under the OpenCode
                // config root (parent of the skills root).
                (ClientTarget::OpenCode, ArtifactKind::Agent) => PathAnchor::OpenCodeRoot,

                // OpenCode: rules live under $GRIM_HOME (loaded via the managed glob
                // in opencode.json — no native rules directory).
                (ClientTarget::OpenCode, ArtifactKind::Rule) => PathAnchor::GrimHome,

                // Codex: skills live under the cross-vendor $HOME/.agents/skills.
                (ClientTarget::Codex, ArtifactKind::Skill) => PathAnchor::AgentsSkills,

                // Codex: agents live in the sibling `agents/` dir under the Codex
                // config root ($CODEX_HOME|~/.codex).
                (ClientTarget::Codex, ArtifactKind::Agent) => PathAnchor::CodexRoot,

                // Codex: rules have no native target — the installer skips them at
                // the `supports_kind` gate, so `from_target` is never reached.
                (ClientTarget::Codex, ArtifactKind::Rule) => {
                    unreachable!("Codex declines rules; they are skipped before anchoring")
                }

                // Bundles are never materialized; they expand into members.
                (ClientTarget::Claude, ArtifactKind::Bundle)
                | (ClientTarget::Copilot, ArtifactKind::Bundle)
                | (ClientTarget::OpenCode, ArtifactKind::Bundle)
                | (ClientTarget::Codex, ArtifactKind::Bundle) => {
                    unreachable!("bundles are never materialized; they expand into members")
                }
            };
            // `GrimHome` is the universal fallback; deduplicate when the
            // primary already is `GrimHome`.
            if primary == PathAnchor::GrimHome {
                vec![PathAnchor::GrimHome]
            } else {
                vec![primary, PathAnchor::GrimHome]
            }
        }
    }
}

/// Lexically subtract `root` from `abs` and return the forward-slash,
/// Normal-only remainder. Purely lexical — **never** canonicalizes, so it is
/// existence-independent: a V1→V2 migration of a legacy record whose target
/// file is gone still classifies (no silent data loss). `None` when `root`
/// is not a component-level prefix of `abs`, when the remainder is empty
/// (abs equals root exactly — rooting at the anchor itself is rejected), or
/// when the remainder is not Normal-only after `CurDir` stripping (a guard
/// against a malformed candidate ever yielding a non-portable remainder).
///
/// A `Normal` component whose bytes are not valid UTF-8 also yields `None`
/// (the `relative` field is invariantly UTF-8).
///
/// The prefix match walks components: `root`'s components must each match the
/// corresponding `abs` component. `Normal` components compare
/// **case-insensitively on Windows (NTFS) and macOS (HFS+/APFS default)**,
/// where the filesystem is not case-sensitive, and **byte-exact on Linux**,
/// where it is. The comparison is per-component Unicode lowercase — the whole
/// path string is never lowercased (that would corrupt case-sensitive Linux
/// segments embedded in a portable record). The stored remainder always
/// preserves the ORIGINAL case of `abs`'s components.
fn strip_prefix_relative(abs: &Path, root: &Path) -> Option<String> {
    let mut abs_components = abs.components();

    // Consume `root` component-by-component; each must match the next `abs`
    // component. This is the lexical, existence-independent replacement for
    // `Path::strip_prefix` — needed so the per-component, case-insensitive
    // compare can run on platforms with a case-insensitive filesystem.
    for root_component in root.components() {
        let abs_component = abs_components.next()?;
        if !components_match(&abs_component, &root_component) {
            return None;
        }
    }

    // The remainder is whatever is left of `abs` after the root prefix is
    // consumed. Keep only `Normal` segments — strip any `CurDir` (`.`); a
    // remainder carrying any other non-`Normal` component is rejected (never
    // stored). The remainder preserves the original case of `abs`.
    let mut parts: Vec<&str> = Vec::new();
    for component in abs_components {
        match component {
            Component::Normal(os) => parts.push(os.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }

    // Reject an empty remainder (abs == root exactly): storing the anchor
    // root itself as a relative path would be dangerous.
    if parts.is_empty() {
        return None;
    }

    Some(parts.join("/"))
}

/// Compare two path components for the lexical store-time prefix match.
///
/// Two `Normal` components compare case-insensitively on Windows/macOS
/// (case-insensitive filesystems) and byte-exact on Linux. The comparison is
/// per-component Unicode lowercase, never on the whole path string. Any
/// other component pair (`RootDir`, `Prefix`, `CurDir`, `ParentDir`) compares
/// structurally for equality.
fn components_match(abs: &Component<'_>, root: &Component<'_>) -> bool {
    match (abs, root) {
        (Component::Normal(a), Component::Normal(b)) => normal_components_match(a, b),
        (a, b) => a == b,
    }
}

/// Case-insensitive (Windows/macOS) or byte-exact (Linux) compare of two
/// `Normal` component values. Returns `false` when either side is not valid
/// UTF-8 on the case-insensitive platforms (the `relative` invariant is
/// UTF-8, so a non-UTF-8 root segment cannot match portably).
#[cfg(any(windows, target_os = "macos"))]
fn normal_components_match(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    match (a.to_str(), b.to_str()) {
        (Some(a), Some(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => false,
    }
}

/// Byte-exact compare of two `Normal` component values on Linux, where the
/// filesystem is case-sensitive.
#[cfg(not(any(windows, target_os = "macos")))]
fn normal_components_match(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    a == b
}

/// An anchor resolution / containment failure.
///
/// `thiserror`, lowercase no-period messages (`quality-rust-errors.md`),
/// `#[non_exhaustive]` (error-enum convention).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AnchorError {
    /// Layer-1 rejection: a stored `relative` carried a non-`Normal`
    /// component (`..`, leading `/`, `.`, or a drive prefix), or was empty.
    #[error("path traversal rejected in stored relative path '{relative}'")]
    TraversalAttempt {
        /// The offending stored remainder.
        relative: String,
    },

    /// Layer-2 rejection: the canonicalized join escaped its anchor root
    /// (symlink tampering).
    ///
    /// The `resolved` field carries the absolute path for debug-level logging;
    /// it is intentionally not rendered in the `Display` message to avoid
    /// leaking full filesystem paths to unprivileged users (CWE-209).
    #[error("resolved path escapes its anchor root (anchor: {anchor})")]
    EscapedAnchor {
        /// The anchor whose root was escaped.
        anchor: PathAnchor,
        /// The resolved path that fell outside the root.
        /// Not rendered in `Display` — available for debug logging only.
        resolved: PathBuf,
    },

    /// Store-time ([`AnchoredPath::from_target`]) failure: no candidate
    /// anchor root prefixes the target.
    #[error("cannot classify install target '{path}' under any known anchor")]
    UnknownAnchor {
        /// The unclassifiable absolute target.
        path: PathBuf,
    },

    /// Resolve-time failure: the anchor's root is unresolvable (no env /
    /// home).
    #[error("anchor root '{anchor}' is unresolvable (no env / home)")]
    AnchorRootAbsent {
        /// The anchor whose root could not be resolved.
        anchor: PathAnchor,
    },

    /// A read / canonicalize I/O failure.
    #[error("I/O error at '{path}'")]
    Io {
        /// The path the failing operation acted on.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}

// ── T2: Specify resolve containment ─────────────────────────────────────────
// ── T4: Specify from_target classification ──────────────────────────────────
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::config::scope::ConfigScope;
    use crate::install::client_target::ClientTarget;
    use crate::oci::ArtifactKind;

    use super::{AnchorError, AnchorRoots, AnchoredPath, PathAnchor};

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Build an `AnchorRoots` with every field set to known paths.
    /// No environment is consulted — this is the "pure table lookup" setup.
    fn all_roots() -> AnchorRoots {
        AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: Some(PathBuf::from("/claude")),
            copilot_root: Some(PathBuf::from("/copilot")),
            opencode_skills: Some(PathBuf::from("/oc/skills")),
            agents_skills: Some(PathBuf::from("/agents/skills")),
            codex_root: Some(PathBuf::from("/codex")),
        }
    }

    // ── T2: PathAnchor::root — pure table lookup ──────────────────────────

    /// Workspace anchor returns the workspace field verbatim (no env).
    #[test]
    fn t2_workspace_root_returns_workspace() {
        let roots = all_roots();
        let got = PathAnchor::Workspace.root(&roots);
        assert_eq!(got, Some(PathBuf::from("/ws")));
    }

    /// GrimHome anchor returns the grim_home field verbatim (no env).
    #[test]
    fn t2_grim_home_root_returns_grim_home() {
        let roots = all_roots();
        let got = PathAnchor::GrimHome.root(&roots);
        assert_eq!(got, Some(PathBuf::from("/grim")));
    }

    /// ClaudeRoot anchor returns exactly the value stored in claude_root.
    #[test]
    fn t2_claude_root_returns_claude_root_field() {
        let roots = all_roots();
        let got = PathAnchor::ClaudeRoot.root(&roots);
        assert_eq!(got, Some(PathBuf::from("/claude")));
    }

    /// CopilotRoot anchor returns the copilot_root field.
    #[test]
    fn t2_copilot_root_returns_copilot_root_field() {
        let roots = all_roots();
        let got = PathAnchor::CopilotRoot.root(&roots);
        assert_eq!(got, Some(PathBuf::from("/copilot")));
    }

    /// OpenCodeSkills anchor returns the opencode_skills field.
    #[test]
    fn t2_opencode_skills_root_returns_opencode_skills_field() {
        let roots = all_roots();
        let got = PathAnchor::OpenCodeSkills.root(&roots);
        assert_eq!(got, Some(PathBuf::from("/oc/skills")));
    }

    /// When claude_root is None, ClaudeRoot::root returns None.
    #[test]
    fn t2_anchor_root_absent_when_option_is_none() {
        let roots = AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        assert!(PathAnchor::ClaudeRoot.root(&roots).is_none());
        assert!(PathAnchor::CopilotRoot.root(&roots).is_none());
        assert!(PathAnchor::OpenCodeSkills.root(&roots).is_none());
    }

    /// OpenCodeRoot anchor returns None when opencode_skills is None.
    /// The root is derived as the parent of opencode_skills; if that field
    /// is absent, OpenCodeRoot has no resolvable root.
    #[test]
    fn f06_opencode_root_is_none_when_opencode_skills_is_none() {
        let roots = AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        assert!(
            PathAnchor::OpenCodeRoot.root(&roots).is_none(),
            "OpenCodeRoot must be None when opencode_skills is None"
        );
    }

    // ── T2: AnchoredPath::resolve — Layer 1 rejections ───────────────────

    /// A normal relative path resolves to root.join(relative) without
    /// touching the filesystem (the candidate does not exist).
    #[test]
    fn t2_resolve_normal_path_returns_root_join_relative() {
        let roots = all_roots();
        let ap = AnchoredPath {
            anchor: PathAnchor::Workspace,
            relative: "skills/foo".to_string(),
        };
        let result = ap.resolve(&roots);
        assert_eq!(result.unwrap(), PathBuf::from("/ws/skills/foo"));
    }

    /// Layer 1 rejects a `..` component WITHOUT touching the filesystem.
    /// The candidate path need not exist for the rejection to fire.
    #[test]
    fn t2_resolve_parent_dir_component_returns_traversal_attempt() {
        let roots = all_roots();
        let ap = AnchoredPath {
            anchor: PathAnchor::Workspace,
            relative: "../secret".to_string(),
        };
        let err = ap.resolve(&roots).unwrap_err();
        assert!(
            matches!(err, AnchorError::TraversalAttempt { .. }),
            "expected TraversalAttempt, got {err:?}"
        );
    }

    /// Layer 1 rejects a leading `/` (RootDir component).
    #[test]
    fn t2_resolve_leading_slash_returns_traversal_attempt() {
        let roots = all_roots();
        let ap = AnchoredPath {
            anchor: PathAnchor::Workspace,
            relative: "/absolute/path".to_string(),
        };
        let err = ap.resolve(&roots).unwrap_err();
        assert!(
            matches!(err, AnchorError::TraversalAttempt { .. }),
            "expected TraversalAttempt, got {err:?}"
        );
    }

    /// Layer 1 rejects a CurDir (`.`) component — §1.2 decision: CurDir is
    /// rejected, not tolerated, to keep the invariant simple.
    #[test]
    fn t2_resolve_cur_dir_component_returns_traversal_attempt() {
        let roots = all_roots();
        let ap = AnchoredPath {
            anchor: PathAnchor::Workspace,
            relative: "./skills/foo".to_string(),
        };
        let err = ap.resolve(&roots).unwrap_err();
        assert!(
            matches!(err, AnchorError::TraversalAttempt { .. }),
            "expected TraversalAttempt for CurDir, got {err:?}"
        );
    }

    /// Layer 1 rejects an empty `relative` (F4): storing the anchor root
    /// itself is dangerous — the caller should always store a non-empty
    /// sub-path relative to the anchor.
    #[test]
    fn f4_empty_relative_returns_traversal_attempt() {
        let roots = all_roots();
        let ap = AnchoredPath {
            anchor: PathAnchor::Workspace,
            relative: String::new(),
        };
        let err = ap.resolve(&roots).unwrap_err();
        assert!(
            matches!(err, AnchorError::TraversalAttempt { .. }),
            "expected TraversalAttempt for empty relative, got {err:?}"
        );
    }

    /// A candidate that does not exist on disk skips Layer 2 and returns Ok.
    /// This proves Layer 1 works standalone (no canonicalize needed for absent paths).
    #[test]
    fn t2_resolve_absent_path_skips_layer2_returns_ok() {
        let roots = all_roots();
        // /ws/nonexistent/deeply/nested does not exist — Layer 2 is skipped.
        let ap = AnchoredPath {
            anchor: PathAnchor::Workspace,
            relative: "nonexistent/deeply/nested".to_string(),
        };
        let result = ap.resolve(&roots);
        assert!(result.is_ok(), "absent path should return Ok, got {result:?}");
        assert_eq!(result.unwrap(), PathBuf::from("/ws/nonexistent/deeply/nested"));
    }

    /// When the anchor's root is None, resolve returns AnchorRootAbsent.
    #[test]
    fn t2_resolve_anchor_root_none_returns_anchor_root_absent() {
        let roots = AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        let ap = AnchoredPath {
            anchor: PathAnchor::ClaudeRoot,
            relative: "skills/foo".to_string(),
        };
        let err = ap.resolve(&roots).unwrap_err();
        assert!(
            matches!(
                err,
                AnchorError::AnchorRootAbsent {
                    anchor: PathAnchor::ClaudeRoot
                }
            ),
            "expected AnchorRootAbsent(ClaudeRoot), got {err:?}"
        );
    }

    /// A forward-slash relative path resolves identically regardless of OS.
    /// The result is root.join("skills/foo") on all platforms.
    #[test]
    fn t2_forward_slash_relative_resolves_cross_platform() {
        let roots = all_roots();
        let ap = AnchoredPath {
            anchor: PathAnchor::ClaudeRoot,
            relative: "skills/my-skill".to_string(),
        };
        let result = ap.resolve(&roots).unwrap();
        // Must equal the anchor root with each segment appended.
        let expected = PathBuf::from("/claude/skills/my-skill");
        assert_eq!(result, expected);
    }

    // ── T4: AnchoredPath::from_target — classification ───────────────────

    /// Project-scope Claude install classifies to Workspace anchor with the
    /// sub-path as the remainder: `<ws>/.claude/rules/x.md` →
    /// `(Workspace, ".claude/rules/x.md")`.
    #[test]
    fn t4_project_claude_rule_classifies_to_workspace() {
        let roots = all_roots();
        let abs = PathBuf::from("/ws/.claude/rules/x.md");
        let result = AnchoredPath::from_target(
            &abs,
            ConfigScope::Project,
            ClientTarget::Claude,
            ArtifactKind::Rule,
            &roots,
        );
        let ap = result.unwrap();
        assert_eq!(ap.anchor, PathAnchor::Workspace);
        assert_eq!(ap.relative, ".claude/rules/x.md");
    }

    /// Global Claude skill → `(ClaudeRoot, "skills/<name>")`.
    #[test]
    fn t4_global_claude_skill_classifies_to_claude_root() {
        let roots = all_roots();
        // abs is built as root.join(relative) per §1.5 caller invariant.
        let abs = PathBuf::from("/claude/skills/my-skill");
        let result = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::Claude,
            ArtifactKind::Skill,
            &roots,
        );
        let ap = result.unwrap();
        assert_eq!(ap.anchor, PathAnchor::ClaudeRoot);
        assert_eq!(ap.relative, "skills/my-skill");
    }

    /// Global OpenCode skill → `(OpenCodeSkills, "<name>")`.
    /// The OpenCodeSkills root already ends in `/skills`, so the remainder
    /// is just the skill name with no prefix.
    #[test]
    fn t4_global_opencode_skill_classifies_to_opencode_skills() {
        let roots = all_roots();
        let abs = PathBuf::from("/oc/skills/my-skill");
        let result = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::OpenCode,
            ArtifactKind::Skill,
            &roots,
        );
        let ap = result.unwrap();
        assert_eq!(ap.anchor, PathAnchor::OpenCodeSkills);
        assert_eq!(ap.relative, "my-skill");
    }

    /// Global OpenCode rule → `(GrimHome, ".opencode/rules/<name>.md")`.
    /// OpenCode global rules live under grim_home (the global "workspace").
    #[test]
    fn t4_global_opencode_rule_classifies_to_grim_home() {
        let roots = all_roots();
        // For global scope the "workspace" passed to vendor is grim_home.
        let abs = PathBuf::from("/grim/.opencode/rules/my-rule.md");
        let result = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::OpenCode,
            ArtifactKind::Rule,
            &roots,
        );
        let ap = result.unwrap();
        assert_eq!(ap.anchor, PathAnchor::GrimHome);
        assert_eq!(ap.relative, ".opencode/rules/my-rule.md");
    }

    /// Global Copilot skill → `(CopilotRoot, "skills/<name>")`.
    #[test]
    fn t4_global_copilot_skill_classifies_to_copilot_root() {
        let roots = all_roots();
        let abs = PathBuf::from("/copilot/skills/my-skill");
        let result = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::Copilot,
            ArtifactKind::Skill,
            &roots,
        );
        let ap = result.unwrap();
        assert_eq!(ap.anchor, PathAnchor::CopilotRoot);
        assert_eq!(ap.relative, "skills/my-skill");
    }

    /// Global Copilot agent → `(CopilotRoot, "agents/<name>.md")` — agents
    /// live under the native `$COPILOT_HOME` root beside `skills/`.
    #[test]
    fn t4_global_copilot_agent_classifies_to_copilot_root() {
        let roots = all_roots();
        let abs = PathBuf::from("/copilot/agents/my-agent.md");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::Copilot,
            ArtifactKind::Agent,
            &roots,
        )
        .unwrap();
        assert_eq!(ap.anchor, PathAnchor::CopilotRoot);
        assert_eq!(ap.relative, "agents/my-agent.md");
    }

    /// Global OpenCode agent → `(OpenCodeRoot, "agents/<name>.md")` — agents
    /// live in the sibling `agents/` dir under the OpenCode config root
    /// (parent of the skills root), NOT under the skills root itself.
    #[test]
    fn t4_global_opencode_agent_classifies_to_opencode_root() {
        let roots = all_roots();
        let abs = PathBuf::from("/oc/agents/my-agent.md");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::OpenCode,
            ArtifactKind::Agent,
            &roots,
        )
        .unwrap();
        assert_eq!(ap.anchor, PathAnchor::OpenCodeRoot);
        assert_eq!(ap.relative, "agents/my-agent.md");
    }

    /// AgentsSkills / CodexRoot anchors return their stored fields verbatim.
    #[test]
    fn t2_codex_anchors_return_their_fields() {
        let roots = all_roots();
        assert_eq!(
            PathAnchor::AgentsSkills.root(&roots),
            Some(PathBuf::from("/agents/skills"))
        );
        assert_eq!(PathAnchor::CodexRoot.root(&roots), Some(PathBuf::from("/codex")));
    }

    /// Global Codex skill → `(AgentsSkills, "<name>")` — the root already
    /// ends in `/skills`, so the remainder is just the skill name.
    #[test]
    fn t4_global_codex_skill_classifies_to_agents_skills() {
        let roots = all_roots();
        let abs = PathBuf::from("/agents/skills/my-skill");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::Codex,
            ArtifactKind::Skill,
            &roots,
        )
        .unwrap();
        assert_eq!(ap.anchor, PathAnchor::AgentsSkills);
        assert_eq!(ap.relative, "my-skill");
    }

    /// Global Codex agent → `(CodexRoot, "agents/<name>.toml")`.
    #[test]
    fn t4_global_codex_agent_classifies_to_codex_root() {
        let roots = all_roots();
        let abs = PathBuf::from("/codex/agents/my-agent.toml");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::Codex,
            ArtifactKind::Agent,
            &roots,
        )
        .unwrap();
        assert_eq!(ap.anchor, PathAnchor::CodexRoot);
        assert_eq!(ap.relative, "agents/my-agent.toml");
    }

    /// A project target NOT under the workspace produces UnknownAnchor.
    #[test]
    fn t4_project_target_outside_workspace_returns_unknown_anchor() {
        let roots = all_roots();
        // /other/path is not under /ws.
        let abs = PathBuf::from("/other/path/.claude/rules/x.md");
        let result = AnchoredPath::from_target(
            &abs,
            ConfigScope::Project,
            ClientTarget::Claude,
            ArtifactKind::Rule,
            &roots,
        );
        let err = result.unwrap_err();
        assert!(
            matches!(err, AnchorError::UnknownAnchor { .. }),
            "expected UnknownAnchor, got {err:?}"
        );
    }

    /// The stored `relative` is forward-slash, Normal-only:
    /// no leading slash, no `..` segments, no `.` segments.
    #[test]
    fn t4_stored_relative_is_normal_only_no_leading_slash_no_dotdot_no_dot() {
        let roots = all_roots();
        let abs = PathBuf::from("/claude/skills/my-skill");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::Claude,
            ArtifactKind::Skill,
            &roots,
        )
        .unwrap();

        // No leading slash.
        assert!(
            !ap.relative.starts_with('/'),
            "relative must not start with '/': {}",
            ap.relative
        );
        // No ParentDir segments.
        assert!(
            !ap.relative.contains(".."),
            "relative must not contain '..': {}",
            ap.relative
        );
        // No CurDir segments (no bare '.' components).
        // A '.' followed by a non-dot char (like ".claude") is fine.
        for component in std::path::Path::new(&ap.relative).components() {
            assert!(
                matches!(component, std::path::Component::Normal(_)),
                "relative must contain only Normal components, found: {component:?}"
            );
        }
    }

    /// A remainder that would contain a CurDir component after strip_prefix
    /// must be stripped: the stored relative must have no `.` segments.
    /// We test by constructing an abs path whose relative portion after
    /// strip_prefix could be `./skills/foo` (hypothetical), confirming the
    /// output has no CurDir components.
    #[test]
    fn t4_cur_dir_stripped_from_remainder() {
        // This test documents the contract: even if a constructed path were
        // to yield a CurDir segment, from_target must strip it. We verify
        // by checking the returned relative is free of CurDir for a normal case.
        let roots = all_roots();
        let abs = PathBuf::from("/ws/.claude/rules/my-rule.md");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Project,
            ClientTarget::Claude,
            ArtifactKind::Rule,
            &roots,
        )
        .unwrap();

        for component in std::path::Path::new(&ap.relative).components() {
            assert!(
                matches!(component, std::path::Component::Normal(_)),
                "remainder has non-Normal component: {component:?}"
            );
        }
    }

    /// Non-canonicalized abs path built via root.join(relative) must
    /// classify successfully. The caller invariant (§1.5) states abs MUST
    /// be the pre-symlink form — strip_prefix against the (also
    /// non-canonicalized) root succeeds lexically.
    #[test]
    fn t4_non_canonicalized_abs_path_classifies_correctly() {
        let roots = all_roots();
        // Build abs as root.join(relative) — the caller invariant.
        let root = roots.claude_root.as_ref().unwrap();
        let abs = root.join("skills").join("my-skill");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::Claude,
            ArtifactKind::Skill,
            &roots,
        )
        .unwrap();
        assert_eq!(ap.anchor, PathAnchor::ClaudeRoot);
        assert_eq!(ap.relative, "skills/my-skill");
    }

    /// When GrimHome would be a prefix of a vendor root (hermetic test
    /// layout), longest-root-first ensures the more specific root wins.
    /// Layout: grim_home=/a, opencode_skills=/a/skills — a path under
    /// /a/skills matches OpenCodeSkills first (longer prefix).
    #[test]
    fn t4_longest_root_first_when_grim_home_prefixes_vendor_root() {
        // Hermetic layout where grim_home is an ancestor of opencode_skills.
        let roots = AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/a"),
            claude_root: Some(PathBuf::from("/a/claude")),
            copilot_root: Some(PathBuf::from("/a/copilot")),
            opencode_skills: Some(PathBuf::from("/a/skills")),
            agents_skills: None,
            codex_root: None,
        };
        let abs = PathBuf::from("/a/skills/my-skill");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::OpenCode,
            ArtifactKind::Skill,
            &roots,
        )
        .unwrap();
        // OpenCodeSkills has a longer root (/a/skills) than GrimHome (/a).
        assert_eq!(ap.anchor, PathAnchor::OpenCodeSkills);
        assert_eq!(ap.relative, "my-skill");
    }

    /// Global Copilot rule (inert) → `(GrimHome, ".github/instructions/…")`.
    /// The §1.1 row the path_anchor tester flagged as untested: Copilot has
    /// no native user-level instructions path, so global rules live under
    /// the `$GRIM_HOME` workspace layout, anchored to `GrimHome`.
    #[test]
    fn t4_global_copilot_rule_classifies_to_grim_home() {
        let roots = all_roots();
        let abs = PathBuf::from("/grim/.github/instructions/my-rule.instructions.md");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::Copilot,
            ArtifactKind::Rule,
            &roots,
        )
        .unwrap();
        assert_eq!(ap.anchor, PathAnchor::GrimHome);
        assert_eq!(ap.relative, ".github/instructions/my-rule.instructions.md");
    }

    /// F07: a global OpenCode skill path that falls under grim_home
    /// (because opencode_skills is None) must classify to GrimHome.
    ///
    /// When `opencode_skills` is None, the `OpenCodeSkills` anchor has no
    /// root. `GrimHome` is the fallback candidate, so a path under
    /// `grim_home/.opencode/skills/<name>` must still classify.
    #[test]
    fn f07_global_opencode_skill_falls_back_to_grim_home_when_opencode_skills_none() {
        let roots = AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        // When there is no opencode_skills root, the vendor falls back to
        // the workspace layout under grim_home.
        let abs = PathBuf::from("/grim/.opencode/skills/my-skill");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Global,
            ClientTarget::OpenCode,
            ArtifactKind::Skill,
            &roots,
        )
        .unwrap();
        assert_eq!(ap.anchor, PathAnchor::GrimHome);
        assert_eq!(ap.relative, ".opencode/skills/my-skill");
    }

    // ── T3: resolve Layer-2 symlink-escape acceptance ────────────────────

    /// A symlink inside the anchor pointing OUTSIDE it must be caught by
    /// Layer 2: the candidate exists, so `dunce::canonicalize` resolves the
    /// symlink and `Path::starts_with` rejects the escape.
    #[cfg(unix)]
    #[test]
    fn t3_resolve_symlink_escape_returns_escaped_anchor() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        // Anchor root and a sibling "outside" dir holding the secret.
        let anchor_root = tmp.path().join("anchor");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&anchor_root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let secret = outside.join("secret.txt");
        std::fs::write(&secret, "top secret").unwrap();

        // A symlink INSIDE the anchor whose name is Normal but whose target
        // escapes the anchor root.
        let link = anchor_root.join("escape");
        symlink(&secret, &link).unwrap();

        let roots = AnchorRoots {
            workspace: anchor_root.clone(),
            grim_home: PathBuf::from("/unused"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        let ap = AnchoredPath {
            anchor: PathAnchor::Workspace,
            relative: "escape".to_string(),
        };
        let err = ap.resolve(&roots).unwrap_err();
        assert!(
            matches!(err, AnchorError::EscapedAnchor { .. }),
            "expected EscapedAnchor for a symlink pointing outside the root, got {err:?}"
        );
    }

    /// W2: a DANGLING symlink under the anchor root (link present, target
    /// absent) must still trip the Layer-2 containment guard. `exists()` is
    /// `false` for a dangling symlink, so the guard also tests `is_symlink()`;
    /// canonicalize then fails (the target is gone) and resolve returns an
    /// error rather than `Ok(root.join(symlink))`. The documented "Layer 2
    /// catches symlink escape" invariant must hold even when the target is
    /// missing — returning `Ok` here would hand the caller an unvalidated path.
    #[cfg(unix)]
    #[test]
    fn w2_resolve_dangling_symlink_returns_err() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let anchor_root = tmp.path().join("anchor");
        std::fs::create_dir_all(&anchor_root).unwrap();

        // A symlink INSIDE the anchor whose target does not exist (dangling).
        let link = anchor_root.join("dangling");
        symlink(tmp.path().join("nonexistent-target"), &link).unwrap();

        let roots = AnchorRoots {
            workspace: anchor_root.clone(),
            grim_home: PathBuf::from("/unused"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        let ap = AnchoredPath {
            anchor: PathAnchor::Workspace,
            relative: "dangling".to_string(),
        };
        let result = ap.resolve(&roots);
        assert!(
            result.is_err(),
            "dangling symlink must trip Layer 2 (canonicalize fails) rather than return Ok, got {result:?}"
        );
    }

    // ── B2 / W5: store-time anchoring is existence-independent + lexical ───

    /// B2 proof: `from_target` must classify a target whose file does NOT
    /// exist on disk. Store-time anchoring is purely lexical — no canonicalize
    /// — so a V1→V2 migration of a legacy record whose target file is gone
    /// still classifies (no silent data loss). This must hold on EVERY
    /// platform, including macOS/Windows where the prior code canonicalized
    /// (and so dropped the record when the path was absent).
    #[test]
    fn b2_from_target_classifies_absent_target_path() {
        // A hermetic, non-existent workspace root and an absent sub-path under
        // it. Neither is created on disk.
        let roots = AnchorRoots {
            workspace: PathBuf::from("/definitely/not/on/disk/ws"),
            grim_home: PathBuf::from("/definitely/not/on/disk/grim"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        let abs = PathBuf::from("/definitely/not/on/disk/ws/.claude/rules/gone.md");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Project,
            ClientTarget::Claude,
            ArtifactKind::Rule,
            &roots,
        )
        .expect("absent target must still classify (existence-independent store-time anchoring)");
        assert_eq!(ap.anchor, PathAnchor::Workspace);
        assert_eq!(ap.relative, ".claude/rules/gone.md");
    }

    /// B2 (macOS): the per-component prefix match is case-insensitive on
    /// macOS. An `abs` built with a case-variant of a root path segment must
    /// still classify, and the stored remainder preserves the ORIGINAL case
    /// of the remainder components (not the root's).
    #[cfg(target_os = "macos")]
    #[test]
    fn b2_macos_case_insensitive_root_segment_classifies() {
        let roots = AnchorRoots {
            workspace: PathBuf::from("/Users/Alice/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        // abs uses a different case for the "Users"/"Alice"/"ws" segments —
        // on macOS (case-insensitive FS) this is the same path.
        let abs = PathBuf::from("/users/alice/WS/.claude/rules/MixedCase.md");
        let ap = AnchoredPath::from_target(
            &abs,
            ConfigScope::Project,
            ClientTarget::Claude,
            ArtifactKind::Rule,
            &roots,
        )
        .expect("case-variant root segment must classify on macOS");
        assert_eq!(ap.anchor, PathAnchor::Workspace);
        // Remainder preserves the ORIGINAL case of the abs components.
        assert_eq!(ap.relative, ".claude/rules/MixedCase.md");
    }

    /// W5: a non-UTF-8 `Normal` component in the remainder yields
    /// `UnknownAnchor` — the `relative` field is invariantly UTF-8, so a
    /// path that cannot round-trip through UTF-8 is unclassifiable.
    #[cfg(unix)]
    #[test]
    fn w5_non_utf8_component_returns_unknown_anchor() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let roots = AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        // Build /ws/<non-utf8> by appending a non-UTF-8 component.
        let mut abs = PathBuf::from("/ws");
        abs.push(OsStr::from_bytes(&[0x66, 0x80])); // "f" + lone continuation byte
        let err = AnchoredPath::from_target(
            &abs,
            ConfigScope::Project,
            ClientTarget::Claude,
            ArtifactKind::Rule,
            &roots,
        )
        .unwrap_err();
        assert!(
            matches!(err, AnchorError::UnknownAnchor { .. }),
            "non-UTF-8 component must yield UnknownAnchor, got {err:?}"
        );
    }

    /// W5: an empty remainder (`abs == root` exactly) yields `UnknownAnchor` —
    /// storing the anchor root itself as a relative path is rejected at store
    /// time.
    #[test]
    fn w5_empty_remainder_returns_unknown_anchor() {
        let roots = AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: None,
            copilot_root: None,
            opencode_skills: None,
            agents_skills: None,
            codex_root: None,
        };
        // abs equals the workspace root exactly.
        let abs = PathBuf::from("/ws");
        let err = AnchoredPath::from_target(
            &abs,
            ConfigScope::Project,
            ClientTarget::Claude,
            ArtifactKind::Rule,
            &roots,
        )
        .unwrap_err();
        assert!(
            matches!(err, AnchorError::UnknownAnchor { .. }),
            "abs == root must yield UnknownAnchor, got {err:?}"
        );
    }

    // ── ARCH-2: from_target / candidate_anchors / resolve coherence ──────
    //
    // This test locks the three-way coherence of `from_target`,
    // `candidate_anchors`, and `resolve`: for every (scope, client, kind)
    // triple the anchor + sub-path derived from the §1.1 anchor-remainder
    // table must survive a full classification → re-resolve round-trip
    // against a hermetic, env-free `AnchorRoots`.
    //
    // WHY HERMETIC: `path_for` reads the real environment ($HOME,
    // $CLAUDE_CONFIG_DIR, $XDG_CONFIG_HOME, …) and therefore produces a
    // path that does NOT fall under the fixed roots used here for global
    // scope.  Using `path_for` to build the input path and then trying
    // `from_target` causes every global vendor-anchor combo to silently
    // hit `UnknownAnchor`, hiding classification bugs.  Instead we build
    // the input path DIRECTLY from the expected (anchor, sub-path) pair
    // defined in the table below — same source of truth as §1.1 / the
    // subsystem-file-structure rule — which means no env is consulted at
    // any point.
    //
    // path_for end-to-end coherence (env-sensitive) is additionally covered
    // by the acceptance suite (test_agents / test_targets install via the
    // real `path_for`, then `status` resolves back — so the full pipeline
    // is exercised under a real env in CI).

    /// Hermetic anchor-remainder table: the expected `(anchor, relative)`
    /// for every materializable `(scope, client, kind)` triple, derived
    /// directly from §1.1 of the design record and
    /// `subsystem-file-structure.md`.  Any mismatch between this table and
    /// `candidate_anchors` / `AnchoredPath::from_target` / `resolve` is
    /// caught as a test failure.
    ///
    /// Rules for the table:
    /// - `relative` uses the same sub-path the vendor would produce, but
    ///   rooted at the anchor, not at the on-disk absolute path.
    /// - For OpenCode agents the anchor is `OpenCodeRoot` (the parent of the
    ///   skills dir) and the sub-path is `agents/<name>.md`.
    /// - GrimHome entries (inert Copilot rule, OpenCode rule) are still
    ///   classified — they must NOT become `UnknownAnchor`.
    fn expected_anchor_and_relative(
        scope: ConfigScope,
        client: ClientTarget,
        kind: ArtifactKind,
        name: &str,
    ) -> (PathAnchor, String) {
        match (scope, client, kind) {
            // ── Project scope ──────────────────────────────────────────────
            // All project targets land under Workspace; the sub-path is
            // the vendor's dot-dir relative to the workspace root.
            (ConfigScope::Project, ClientTarget::Claude, ArtifactKind::Skill) => {
                (PathAnchor::Workspace, format!(".claude/skills/{name}"))
            }
            (ConfigScope::Project, ClientTarget::Claude, ArtifactKind::Rule) => {
                (PathAnchor::Workspace, format!(".claude/rules/{name}.md"))
            }
            (ConfigScope::Project, ClientTarget::Claude, ArtifactKind::Agent) => {
                (PathAnchor::Workspace, format!(".claude/agents/{name}.md"))
            }
            (ConfigScope::Project, ClientTarget::Copilot, ArtifactKind::Skill) => {
                (PathAnchor::Workspace, format!(".github/skills/{name}"))
            }
            (ConfigScope::Project, ClientTarget::Copilot, ArtifactKind::Rule) => (
                PathAnchor::Workspace,
                format!(".github/instructions/{name}.instructions.md"),
            ),
            (ConfigScope::Project, ClientTarget::Copilot, ArtifactKind::Agent) => {
                (PathAnchor::Workspace, format!(".github/agents/{name}.md"))
            }
            (ConfigScope::Project, ClientTarget::OpenCode, ArtifactKind::Skill) => {
                (PathAnchor::Workspace, format!(".opencode/skills/{name}"))
            }
            (ConfigScope::Project, ClientTarget::OpenCode, ArtifactKind::Rule) => {
                (PathAnchor::Workspace, format!(".opencode/rules/{name}.md"))
            }
            (ConfigScope::Project, ClientTarget::OpenCode, ArtifactKind::Agent) => {
                (PathAnchor::Workspace, format!(".opencode/agents/{name}.md"))
            }

            // ── Global scope ───────────────────────────────────────────────
            // Claude: all three kinds → ClaudeRoot.
            (ConfigScope::Global, ClientTarget::Claude, ArtifactKind::Skill) => {
                (PathAnchor::ClaudeRoot, format!("skills/{name}"))
            }
            (ConfigScope::Global, ClientTarget::Claude, ArtifactKind::Rule) => {
                (PathAnchor::ClaudeRoot, format!("rules/{name}.md"))
            }
            (ConfigScope::Global, ClientTarget::Claude, ArtifactKind::Agent) => {
                (PathAnchor::ClaudeRoot, format!("agents/{name}.md"))
            }

            // Copilot skill / agent → CopilotRoot.
            (ConfigScope::Global, ClientTarget::Copilot, ArtifactKind::Skill) => {
                (PathAnchor::CopilotRoot, format!("skills/{name}"))
            }
            (ConfigScope::Global, ClientTarget::Copilot, ArtifactKind::Agent) => {
                (PathAnchor::CopilotRoot, format!("agents/{name}.md"))
            }

            // Copilot rule (inert) → GrimHome.
            (ConfigScope::Global, ClientTarget::Copilot, ArtifactKind::Rule) => (
                PathAnchor::GrimHome,
                format!(".github/instructions/{name}.instructions.md"),
            ),

            // OpenCode skill → OpenCodeSkills (root already ends in /skills).
            (ConfigScope::Global, ClientTarget::OpenCode, ArtifactKind::Skill) => {
                (PathAnchor::OpenCodeSkills, name.to_string())
            }

            // OpenCode agent → OpenCodeRoot (parent of the skills dir).
            (ConfigScope::Global, ClientTarget::OpenCode, ArtifactKind::Agent) => {
                (PathAnchor::OpenCodeRoot, format!("agents/{name}.md"))
            }

            // OpenCode rule → GrimHome.
            (ConfigScope::Global, ClientTarget::OpenCode, ArtifactKind::Rule) => {
                (PathAnchor::GrimHome, format!(".opencode/rules/{name}.md"))
            }

            // Codex project: skills in `.agents/skills`, agents as `.codex` TOML.
            (ConfigScope::Project, ClientTarget::Codex, ArtifactKind::Skill) => {
                (PathAnchor::Workspace, format!(".agents/skills/{name}"))
            }
            (ConfigScope::Project, ClientTarget::Codex, ArtifactKind::Agent) => {
                (PathAnchor::Workspace, format!(".codex/agents/{name}.toml"))
            }
            // Codex global: skill → AgentsSkills (root ends in /skills);
            // agent → CodexRoot + `agents/<name>.toml`.
            (ConfigScope::Global, ClientTarget::Codex, ArtifactKind::Skill) => {
                (PathAnchor::AgentsSkills, name.to_string())
            }
            (ConfigScope::Global, ClientTarget::Codex, ArtifactKind::Agent) => {
                (PathAnchor::CodexRoot, format!("agents/{name}.toml"))
            }
            // Codex rules are unsupported — excluded from the loop below.
            (_, ClientTarget::Codex, ArtifactKind::Rule) => {
                unreachable!("Codex rules are skipped, not classified")
            }

            // Bundles are never materialised — exclude from the test loop.
            (_, _, ArtifactKind::Bundle) => unreachable!("bundles excluded from this loop"),
        }
    }

    /// For every materializable (scope, client, kind) triple:
    ///
    /// 1. Derive the expected (anchor, relative) from the §1.1 table.
    /// 2. Build `dest = expected_anchor.root(&roots).join(expected_relative)`.
    /// 3. Assert `AnchoredPath::from_target(&dest, …)` classifies to the
    ///    expected (anchor, relative).
    /// 4. Assert `ap.resolve(&roots)` round-trips back to `dest`.
    ///
    /// No `continue` on `UnknownAnchor`: every combo MUST classify; a miss
    /// is a test failure.  The counter assertion at the end guarantees that
    /// adding a new client or kind without updating the table also fails.
    ///
    /// This locks `from_target` / `candidate_anchors` / `resolve` coherence.
    /// In particular, dropping `ClaudeRoot` from
    /// `candidate_anchors(Global, Claude, Skill)` would cause assertion (3)
    /// to return `GrimHome` instead, failing this test.
    #[test]
    fn arch2_from_target_and_resolve_are_coherent_for_all_scope_client_kind_triples() {
        // Hermetic roots: all fields set to fixed, non-overlapping paths so
        // no env variable is consulted during classification or resolution.
        // /oc is the OpenCode config root; /oc/skills is the skills root, so
        // OpenCodeRoot resolves to /oc (the parent of /oc/skills).
        let roots = AnchorRoots {
            workspace: PathBuf::from("/ws"),
            grim_home: PathBuf::from("/grim"),
            claude_root: Some(PathBuf::from("/claude")),
            copilot_root: Some(PathBuf::from("/copilot")),
            opencode_skills: Some(PathBuf::from("/oc/skills")),
            // /agents/skills is the Codex skills root; /codex its config root.
            agents_skills: Some(PathBuf::from("/agents/skills")),
            codex_root: Some(PathBuf::from("/codex")),
        };

        let name = "test-artifact";
        let scopes = [ConfigScope::Project, ConfigScope::Global];
        let clients = ClientTarget::ALL; // [Claude, OpenCode, Copilot, Codex]
        let kinds = [ArtifactKind::Skill, ArtifactKind::Rule, ArtifactKind::Agent];
        // ArtifactKind::Bundle is excluded — bundles are never materialised.

        let mut combo_count = 0usize;

        for scope in scopes {
            for client in clients {
                for kind in kinds {
                    // A vendor that declines a kind never reaches `from_target`
                    // (the installer skips it at the `supports_kind` gate), so
                    // it has no anchor-remainder entry — Codex rules here.
                    if !client.vendor().supports_kind(kind) {
                        continue;
                    }
                    combo_count += 1;

                    // Step 1: expected anchor + relative from the §1.1 table.
                    let (expected_anchor, expected_relative) = expected_anchor_and_relative(scope, client, kind, name);

                    // Step 2: build the absolute dest from the hermetic roots.
                    // The anchor root must resolve (all roots are Some in this
                    // fixture) — an unwrap failure here means the table entry
                    // references an anchor whose root is absent, which is a bug
                    // in the test table itself.
                    let anchor_root = expected_anchor.root(&roots).unwrap_or_else(|| {
                        panic!(
                            "anchor {expected_anchor:?} has no root in hermetic fixture \
                             for ({scope:?}, {client:?}, {kind:?})"
                        )
                    });
                    let dest = anchor_root.join(&expected_relative);

                    // Step 3: from_target must classify to the expected pair —
                    // NO silent skip on UnknownAnchor; every combo must match.
                    let ap = AnchoredPath::from_target(&dest, scope, client, kind, &roots).unwrap_or_else(|e| {
                        panic!(
                            "from_target returned {e:?} for ({scope:?}, {client:?}, {kind:?}): \
                             expected anchor={expected_anchor:?} relative={expected_relative:?}"
                        )
                    });

                    assert_eq!(
                        ap.anchor, expected_anchor,
                        "anchor mismatch for ({scope:?}, {client:?}, {kind:?}): \
                         expected {expected_anchor:?}, got {:?}",
                        ap.anchor
                    );
                    assert_eq!(
                        ap.relative, expected_relative,
                        "relative mismatch for ({scope:?}, {client:?}, {kind:?}): \
                         expected {expected_relative:?}, got {:?}",
                        ap.relative
                    );

                    // Step 4: resolve must round-trip back to dest (absent
                    // path → Layer 2 skipped → raw join == dest).
                    let resolved = ap
                        .resolve(&roots)
                        .unwrap_or_else(|e| panic!("resolve failed for ({scope:?}, {client:?}, {kind:?}): {e:?}"));
                    assert_eq!(
                        resolved, dest,
                        "resolve round-trip mismatch for ({scope:?}, {client:?}, {kind:?})"
                    );
                }
            }
        }

        // Exhaustiveness guard: 2 scopes × 4 clients × 3 kinds = 24, minus the
        // 2 Codex-rule combos the `supports_kind` gate skips = 22 combos.
        // If a new ClientTarget or ArtifactKind variant is added, this fails,
        // forcing the table to be extended.
        assert_eq!(
            combo_count, 22,
            "expected 22 (scope × client × kind) combos but counted {combo_count}; \
             update the table in expected_anchor_and_relative() and this assertion"
        );
    }
}
