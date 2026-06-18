// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Managed registration of grim's rule glob in OpenCode's `instructions`
//! config.
//!
//! OpenCode has no per-file rule scoping and no `rules/` directory of its
//! own: instruction files load through `AGENTS.md` or the `instructions`
//! array (paths / globs / URLs) in `opencode.json`. grim therefore writes
//! rules to `.opencode/rules/<name>.md` **and** keeps exactly one managed
//! glob entry in the vendor config pointing at that directory — added
//! when the first OpenCode rule installs, removed when the last one
//! uninstalls (the reversible config-registration pattern from the hooks
//! ADR).
//!
//! Config resolution mirrors OpenCode's own:
//! - **project** scope edits `<workspace>/opencode.jsonc` when present,
//!   else `<workspace>/opencode.json`, with a workspace-relative glob;
//! - **global** scope edits `$OPENCODE_CONFIG` when set, else
//!   `$XDG_CONFIG_HOME/opencode/opencode.json` (default
//!   `~/.config/opencode/opencode.json`), with an absolute glob rooted at
//!   `$GRIM_HOME` (the global install workspace).
//!
//! `$OPENCODE_CONFIG` (a config **file** path) and `$OPENCODE_CONFIG_DIR`
//! (OpenCode's additive skills/agents scan **directory**, honored by
//! [`super::vendor_opencode`]'s `skills_root`) are orthogonal variables —
//! only the former matters here.
//!
//! Edits are conservative: a config that does not parse (even after
//! JSONC comment / trailing-comma stripping) is **never** rewritten —
//! the sync fails rather than clobbering user content. A parseable JSONC
//! file is rewritten as plain JSON; its comments are not preserved (a
//! documented caveat — the write warns when that happens).

use std::io;
use std::path::{Path, PathBuf};

use crate::config::scope::ConfigScope;
use crate::install::install_state::InstallState;
use crate::oci::ArtifactKind;
use crate::store::atomic_write;

use super::client_target::ClientTarget;

/// The workspace-relative glob grim manages for project-scope installs.
pub const MANAGED_PROJECT_GLOB: &str = ".opencode/rules/*.md";

/// What a sync did to the vendor config.
///
/// Closed internal enum — matches stay total, no `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionsSync {
    /// The managed glob was appended to `instructions`.
    Added,
    /// The managed glob was removed (and an emptied `instructions` key
    /// dropped).
    Removed,
    /// The config already matched the desired state — no write.
    Unchanged,
}

/// The managed `instructions` entry for an install scope rooted at
/// `workspace`: workspace-relative for a project config (which sits at
/// the workspace root), absolute for the global config (which does not).
pub fn managed_entry(workspace: &Path, scope: ConfigScope) -> String {
    match scope {
        ConfigScope::Project => MANAGED_PROJECT_GLOB.to_string(),
        ConfigScope::Global => workspace.join(MANAGED_PROJECT_GLOB).display().to_string(),
    }
}

/// Resolve the OpenCode config file grim manages for `scope`, or `None`
/// when the global location cannot be determined (no `$OPENCODE_CONFIG`,
/// `$XDG_CONFIG_HOME`, or `$HOME`) — mirroring the other vendors'
/// no-`$HOME` handling, the sync is skipped rather than writing to a
/// CWD-relative path.
pub fn config_path_for_scope(workspace: &Path, scope: ConfigScope) -> Option<PathBuf> {
    match scope {
        ConfigScope::Project => Some(project_config_path(workspace)),
        // `env_dir` treats an empty value as unset — same convention as
        // every other vendor env override.
        ConfigScope::Global => global_config_path(
            super::vendor::env_dir("OPENCODE_CONFIG"),
            super::vendor::env_dir("XDG_CONFIG_HOME"),
            super::vendor::env_dir("HOME"),
        ),
    }
}

/// The project-scope config: `opencode.jsonc` when present (OpenCode
/// supports both spellings), else `opencode.json`.
fn project_config_path(workspace: &Path) -> PathBuf {
    let jsonc = workspace.join("opencode.jsonc");
    if jsonc.is_file() {
        jsonc
    } else {
        workspace.join("opencode.json")
    }
}

/// The global-scope config: `$OPENCODE_CONFIG` wins (it is OpenCode's own
/// "custom config file path" override), else the XDG default. `None` when
/// no variable resolves a location — a relative fallback would silently
/// land the edit wherever the process happens to run.
fn global_config_path(
    env_override: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(path) = env_override {
        return Some(path);
    }
    let config_dir = xdg_config_home.or_else(|| home.map(|h| h.join(".config")))?;
    Some(config_dir.join("opencode").join("opencode.json"))
}

/// Converge the vendor config on the state's needs: ensure the managed
/// glob is present while any OpenCode rule is recorded for this scope,
/// absent otherwise. With no OpenCode rule left, the now-empty managed
/// `.opencode/rules/` directory is reaped too (best-effort — a non-empty
/// dir is never touched). Call after install/update/uninstall mutated
/// `state`.
///
/// # Errors
///
/// An I/O failure reading/writing the config, or `InvalidData` when the
/// existing config cannot be parsed (grim refuses to clobber it).
pub fn sync_for_state(state: &InstallState, workspace: &Path, scope: ConfigScope) -> io::Result<InstructionsSync> {
    let opencode = ClientTarget::OpenCode.to_string();
    let want = state
        .iter_records()
        .any(|r| r.kind == ArtifactKind::Rule && r.outputs.iter().any(|c| c.client == opencode));
    // The managed rules dir mirrors the managed glob: when the last
    // OpenCode rule for this scope is gone, reap the now-empty
    // `.opencode/rules/` directory (it exists only because a rule install
    // created it). `remove_dir` refuses a non-empty dir, so user files
    // are never touched; that refusal — and an already-absent dir — are
    // deliberately ignored (best-effort hygiene, never a sync failure).
    if !want {
        let _ = std::fs::remove_dir(workspace.join(".opencode").join("rules"));
    }
    // No resolvable config location (global scope without $OPENCODE_CONFIG,
    // $XDG_CONFIG_HOME, or $HOME): skip the sync rather than invent a
    // CWD-relative path — the same degradation as the install paths.
    let Some(config_path) = config_path_for_scope(workspace, scope) else {
        return Ok(InstructionsSync::Unchanged);
    };
    let entry = managed_entry(workspace, scope);
    sync_managed_instruction(&config_path, &entry, want)
}

/// Idempotently add (`want = true`) or remove (`want = false`) the managed
/// `entry` in the `instructions` array of the config at `config_path`.
///
/// - Adding creates the file (`{"instructions": [entry]}`) when absent.
/// - Removing an entry from an absent/never-registered config is a no-op.
/// - Other config keys and other `instructions` entries are preserved.
///
/// Removal (`want == false`) is tolerant: an absent, unparseable, or
/// wrong-typed (`instructions` not an array) config has nothing grim-managed
/// to remove, so it converges as [`InstructionsSync::Unchanged`] rather than
/// failing. Adding (`want == true`) stays strict — grim never rewrites a file
/// it cannot parse or whose `instructions` is an unexpected type.
///
/// # Errors
///
/// An I/O failure, or — **only when adding** (`want == true`) — `InvalidData`
/// when the existing content is not a JSON/JSONC object, or its `instructions`
/// key is not an array (grim never clobbers an unknown-schema file).
pub fn sync_managed_instruction(config_path: &Path, entry: &str, want: bool) -> io::Result<InstructionsSync> {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(with_path(config_path, e)),
    };

    let (mut doc, had_jsonc_extras) = match &raw {
        None => (serde_json::Map::new(), false),
        Some(raw) => match parse_object(raw, config_path) {
            Ok(parsed) => parsed,
            // Removal is tolerant (`want == false`): a config grim cannot parse
            // has nothing grim-managed to remove, so converge as `Unchanged`
            // rather than fail a command whose primary action already ran.
            // Adding stays strict (never rewrite an unknown-schema file).
            Err(_) if !want => return Ok(InstructionsSync::Unchanged),
            Err(e) => return Err(e),
        },
    };

    let instructions = doc.get("instructions");
    let mut entries: Vec<serde_json::Value> = match instructions {
        None => Vec::new(),
        Some(serde_json::Value::Array(items)) => items.clone(),
        // A non-array `instructions` is an unknown schema. On removal there is
        // nothing grim-managed to take out → `Unchanged`; on add, refuse to
        // edit rather than clobber the user's value.
        Some(_) if !want => return Ok(InstructionsSync::Unchanged),
        Some(_) => {
            return Err(invalid_data(format!(
                "'{}': 'instructions' is not an array; refusing to edit",
                config_path.display()
            )));
        }
    };

    let present = entries.iter().any(|v| v.as_str() == Some(entry));
    let outcome = match (want, present) {
        (true, true) | (false, false) => return Ok(InstructionsSync::Unchanged),
        (true, false) => {
            entries.push(serde_json::Value::String(entry.to_string()));
            InstructionsSync::Added
        }
        (false, true) => {
            entries.retain(|v| v.as_str() != Some(entry));
            InstructionsSync::Removed
        }
    };

    if entries.is_empty() {
        doc.remove("instructions");
    } else {
        doc.insert("instructions".to_string(), serde_json::Value::Array(entries));
    }

    if had_jsonc_extras {
        tracing::warn!(
            "rewriting '{}' drops its JSONC comments (grim writes plain JSON)",
            config_path.display()
        );
    }

    let mut bytes =
        serde_json::to_vec_pretty(&serde_json::Value::Object(doc)).map_err(|e| invalid_data(e.to_string()))?;
    bytes.push(b'\n');
    atomic_write(config_path, &bytes).map_err(|e| with_path(config_path, e))?;
    Ok(outcome)
}

/// Attach `path` to a bare read/write error — `std::fs` errors do not
/// embed the path on stable Rust — preserving the source chain (never
/// stringify a structured error).
fn with_path(path: &Path, source: io::Error) -> io::Error {
    io::Error::new(
        source.kind(),
        PathIo {
            path: path.to_path_buf(),
            source,
        },
    )
}

/// A path-attributed I/O failure on the vendor config file.
#[derive(Debug, thiserror::Error)]
#[error("{path}")]
struct PathIo {
    path: PathBuf,
    #[source]
    source: io::Error,
}

/// Parse `raw` as a JSON object, falling back to a JSONC sanitization pass
/// (comments, trailing commas). Returns the object and whether the
/// sanitization changed anything (⇒ rewriting loses comments).
fn parse_object(raw: &str, path: &Path) -> io::Result<(serde_json::Map<String, serde_json::Value>, bool)> {
    let refused = || {
        invalid_data(format!(
            "'{}' is not a JSON object grim can edit; refusing to touch it",
            path.display()
        ))
    };
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(raw) {
        return Ok((map, false));
    }
    let sanitized = sanitize_jsonc(raw);
    match serde_json::from_str::<serde_json::Value>(&sanitized) {
        Ok(serde_json::Value::Object(map)) => Ok((map, true)),
        _ => Err(refused()),
    }
}

/// Strip `//` and `/* */` comments plus trailing commas — the JSONC
/// extensions OpenCode accepts — while leaving string contents intact.
fn sanitize_jsonc(input: &str) -> String {
    // Pass 1: comments.
    let chars: Vec<char> = input.chars().collect();
    let mut no_comments = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            no_comments.push(c);
            if c == '\\' && i + 1 < chars.len() {
                no_comments.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                no_comments.push(c);
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(chars.len());
            }
            _ => {
                no_comments.push(c);
                i += 1;
            }
        }
    }

    // Pass 2: trailing commas.
    let chars: Vec<char> = no_comments.chars().collect();
    let mut out = String::with_capacity(no_comments.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if matches!(chars.get(j), Some('}') | Some(']')) {
                i += 1; // drop the trailing comma
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn invalid_data(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_creates_file_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("opencode.json");

        let first = sync_managed_instruction(&cfg, ".opencode/rules/*.md", true).unwrap();
        assert_eq!(first, InstructionsSync::Added);
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(doc["instructions"][0], ".opencode/rules/*.md");

        let second = sync_managed_instruction(&cfg, ".opencode/rules/*.md", true).unwrap();
        assert_eq!(second, InstructionsSync::Unchanged);
    }

    #[test]
    fn remove_preserves_other_entries_and_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("opencode.json");
        std::fs::write(
            &cfg,
            r#"{"model": "anthropic/claude", "instructions": ["CONTRIBUTING.md", ".opencode/rules/*.md"]}"#,
        )
        .unwrap();

        let out = sync_managed_instruction(&cfg, ".opencode/rules/*.md", false).unwrap();
        assert_eq!(out, InstructionsSync::Removed);
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(doc["model"], "anthropic/claude", "unrelated keys preserved");
        assert_eq!(doc["instructions"], serde_json::json!(["CONTRIBUTING.md"]));
    }

    #[test]
    fn remove_last_entry_drops_the_key_and_absent_file_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("opencode.json");
        std::fs::write(&cfg, r#"{"instructions": [".opencode/rules/*.md"]}"#).unwrap();

        let out = sync_managed_instruction(&cfg, ".opencode/rules/*.md", false).unwrap();
        assert_eq!(out, InstructionsSync::Removed);
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(doc.get("instructions").is_none(), "emptied key dropped");

        // Remove against a config that never existed: converges, no file.
        let missing = tmp.path().join("never.json");
        let out = sync_managed_instruction(&missing, "x", false).unwrap();
        assert_eq!(out, InstructionsSync::Unchanged);
        assert!(!missing.exists());
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_parse_but_unparseable_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("opencode.jsonc");
        std::fs::write(
            &cfg,
            "{\n  // the model\n  \"model\": \"a/b\", /* block */\n  \"instructions\": [\"x.md\",],\n}\n",
        )
        .unwrap();
        let out = sync_managed_instruction(&cfg, "g", true).unwrap();
        assert_eq!(out, InstructionsSync::Added);
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(doc["model"], "a/b");
        assert_eq!(doc["instructions"], serde_json::json!(["x.md", "g"]));

        let broken = tmp.path().join("broken.json");
        std::fs::write(&broken, "not json at all {{{").unwrap();
        let err = sync_managed_instruction(&broken, "g", true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(&broken).unwrap(),
            "not json at all {{{",
            "unparseable config must never be rewritten"
        );
    }

    // ── C6/C7: tolerant removal, strict add ─────────────────────────────────

    /// C6: removing the managed glob from an unparseable config converges as
    /// `Unchanged` (nothing grim-managed to remove) and never rewrites it.
    #[test]
    fn remove_tolerates_unparseable_opencode_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("opencode.json");
        let garbage = "not json at all {{{";
        std::fs::write(&cfg, garbage).unwrap();

        let out = sync_managed_instruction(&cfg, ".opencode/rules/*.md", false).unwrap();
        assert_eq!(out, InstructionsSync::Unchanged);
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            garbage,
            "an unparseable config must never be rewritten, even on removal"
        );
    }

    /// C7: removing the managed glob when `instructions` is not an array
    /// converges as `Unchanged` rather than hard-failing.
    #[test]
    fn remove_tolerates_non_array_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("opencode.json");
        let body = r#"{"instructions": "x"}"#;
        std::fs::write(&cfg, body).unwrap();

        let out = sync_managed_instruction(&cfg, ".opencode/rules/*.md", false).unwrap();
        assert_eq!(out, InstructionsSync::Unchanged);
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            body,
            "a non-array instructions value is left untouched on removal"
        );
    }

    /// C6/C7 guard: adding stays strict — an unparseable config is refused
    /// (never clobbered) so an unknown schema is preserved.
    #[test]
    fn add_rejects_unparseable_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("opencode.json");
        let garbage = "not json at all {{{";
        std::fs::write(&cfg, garbage).unwrap();

        let err = sync_managed_instruction(&cfg, ".opencode/rules/*.md", true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            garbage,
            "adding must never clobber an unparseable config"
        );
    }

    #[test]
    fn sanitize_leaves_string_contents_alone() {
        let s = r#"{"a": "url://x", "b": "has // no comment", "c": "star /* kept */"}"#;
        assert_eq!(sanitize_jsonc(s), s);
    }

    #[test]
    fn managed_entry_is_relative_for_project_absolute_for_global() {
        let ws = Path::new("/data/grim-home");
        assert_eq!(managed_entry(ws, ConfigScope::Project), ".opencode/rules/*.md");
        assert_eq!(
            managed_entry(ws, ConfigScope::Global),
            "/data/grim-home/.opencode/rules/*.md"
        );
    }

    #[test]
    fn global_config_path_resolution_order() {
        assert_eq!(
            global_config_path(Some(PathBuf::from("/custom/oc.json")), None, None),
            Some(PathBuf::from("/custom/oc.json")),
            "OPENCODE_CONFIG wins"
        );
        assert_eq!(
            global_config_path(None, Some(PathBuf::from("/xdg")), Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/xdg/opencode/opencode.json"))
        );
        assert_eq!(
            global_config_path(None, None, Some(PathBuf::from("/home/u"))),
            Some(PathBuf::from("/home/u/.config/opencode/opencode.json"))
        );
        assert_eq!(
            global_config_path(None, None, None),
            None,
            "no variable at all: skip the sync, never a CWD-relative path"
        );
    }

    #[test]
    fn project_config_prefers_existing_jsonc() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(project_config_path(tmp.path()), tmp.path().join("opencode.json"));
        std::fs::write(tmp.path().join("opencode.jsonc"), "{}\n").unwrap();
        assert_eq!(project_config_path(tmp.path()), tmp.path().join("opencode.jsonc"));
    }

    #[test]
    fn sync_for_state_adds_only_when_an_opencode_rule_is_recorded() {
        use crate::install::install_state::{ClientOutput, InstallRecord};
        use crate::install::path_anchor::{AnchoredPath, PathAnchor};
        use crate::oci::pinned_identifier::PinnedIdentifier;
        use crate::oci::{Digest, Identifier};

        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("acme/r", "localhost:5000").clone_with_digest(Digest::Sha256("a".repeat(64))),
        )
        .unwrap();

        let mut state = InstallState::empty(&ws.join("state.json"));
        // No opencode rule yet ⇒ no write.
        assert_eq!(
            sync_for_state(&state, ws, ConfigScope::Project).unwrap(),
            InstructionsSync::Unchanged
        );
        assert!(!ws.join("opencode.json").exists());

        // Record an opencode rule using `outputs` (the V2 field; no denorm fields).
        state.record(InstallRecord {
            kind: ArtifactKind::Rule,
            name: "r".to_string(),
            pinned,
            outputs: vec![ClientOutput {
                client: "opencode".to_string(),
                target: AnchoredPath {
                    anchor: PathAnchor::Workspace,
                    relative: ".opencode/rules/r.md".to_string(),
                },
                content_hash: Digest::Sha256("b".repeat(64)),
                support_dir: None,
            }],
        });
        assert_eq!(
            sync_for_state(&state, ws, ConfigScope::Project).unwrap(),
            InstructionsSync::Added
        );

        state.remove(ArtifactKind::Rule, "r");
        assert_eq!(
            sync_for_state(&state, ws, ConfigScope::Project).unwrap(),
            InstructionsSync::Removed
        );
    }

    #[test]
    fn sync_for_state_reaps_empty_rules_dir_but_never_user_files() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        let rules_dir = ws.join(".opencode").join("rules");

        // Empty managed dir + no opencode rule recorded ⇒ reaped.
        std::fs::create_dir_all(&rules_dir).unwrap();
        let state = InstallState::empty(&ws.join("state.json"));
        sync_for_state(&state, ws, ConfigScope::Project).unwrap();
        assert!(!rules_dir.exists(), "empty rules dir is reaped");
        assert!(ws.join(".opencode").exists(), "only the rules dir itself goes");

        // A dir holding user files is never touched.
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("mine.md"), "user content\n").unwrap();
        sync_for_state(&state, ws, ConfigScope::Project).unwrap();
        assert!(rules_dir.join("mine.md").is_file(), "non-empty dir is preserved");

        // An absent dir stays a silent no-op (idempotent).
        std::fs::remove_file(rules_dir.join("mine.md")).unwrap();
        std::fs::remove_dir(&rules_dir).unwrap();
        sync_for_state(&state, ws, ConfigScope::Project).unwrap();
        assert!(!rules_dir.exists());
    }

    #[test]
    fn written_config_is_always_pretty_printed_valid_json() {
        // Contract pin: every write goes through serde's pretty printer and
        // ends with a newline — never a hand-assembled (and breakable)
        // JSON string.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("opencode.json");
        std::fs::write(&cfg, "{\"$schema\": \"https://opencode.ai/config.json\"}").unwrap();

        sync_managed_instruction(&cfg, ".opencode/rules/*.md", true).unwrap();
        let added = std::fs::read_to_string(&cfg).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&added).unwrap();
        assert_eq!(doc["$schema"], "https://opencode.ai/config.json");
        assert_eq!(
            added,
            serde_json::to_string_pretty(&doc).unwrap() + "\n",
            "output is pretty-printed and newline-terminated"
        );

        // The remove round-trip stays valid pretty JSON too (the shape the
        // user-reported breakage would have violated).
        sync_managed_instruction(&cfg, ".opencode/rules/*.md", false).unwrap();
        let removed = std::fs::read_to_string(&cfg).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&removed).unwrap();
        assert!(doc.get("instructions").is_none());
        assert_eq!(removed, serde_json::to_string_pretty(&doc).unwrap() + "\n");
    }
}
