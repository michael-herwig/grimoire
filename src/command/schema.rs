// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim schema` — emit a JSON Schema for an author-facing TOML format.
//!
//! Generates a JSON Schema (draft 2020-12) from the real `Deserialize`
//! structs — `RawConfig` for `grimoire.toml`, `PublishManifest` for
//! `publish.toml` — via `schemars`, so the published schema can never
//! describe a shape the parser does not accept. The generated document is
//! decorated with the published `$id`, a friendly `title`, and the draft
//! `$schema` keyword, then pretty-printed to stdout.
//!
//! Like `tui`, this command emits a document rather than a `Printable`
//! report, so (per subsystem-cli-api.md "Commands That Exec a Child
//! Process") it is wired directly in `app.rs` without an `api/` report
//! module — the JSON it prints is the payload, not a table.

use clap::{Args, ValueEnum};

use crate::cli::exit_code::ExitCode;

/// Base URL the published schemas are hosted at (the GitHub Pages docs
/// site). Joined with each kind's filename to form the `$id`.
const SCHEMA_BASE_URL: &str = "https://michael-herwig.github.io/grimoire/schemas";

/// The JSON Schema draft the generated documents declare via `$schema`.
const SCHEMA_DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Published filename of the `grimoire.toml` schema.
const CONFIG_SCHEMA_FILE: &str = "grimoire-config.schema.json";

/// Published filename of the `publish.toml` schema.
const PUBLISH_SCHEMA_FILE: &str = "grim-publish.schema.json";

/// Which author-facing TOML format to emit a JSON Schema for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SchemaKind {
    /// The `grimoire.toml` declaration file (project and global scope share
    /// the same shape).
    Config,
    /// The `publish.toml` batch-release manifest.
    Publish,
}

/// `grim schema` arguments.
#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// The TOML format to emit a JSON Schema for.
    #[arg(long)]
    pub kind: SchemaKind,
}

impl SchemaKind {
    /// The published `$id` URL for this kind's schema.
    fn id(self) -> String {
        let file = match self {
            SchemaKind::Config => CONFIG_SCHEMA_FILE,
            SchemaKind::Publish => PUBLISH_SCHEMA_FILE,
        };
        format!("{SCHEMA_BASE_URL}/{file}")
    }

    /// The human-facing schema `title`.
    fn title(self) -> &'static str {
        match self {
            SchemaKind::Config => "grimoire.toml — Grimoire declaration file",
            SchemaKind::Publish => "publish.toml — Grimoire publish manifest",
        }
    }
}

/// Generate the decorated JSON Schema document for `kind`.
///
/// Pure and unit-testable: builds the schema from the real parse struct,
/// injects `$schema`/`$id`/`title`, and pretty-prints. No trailing newline
/// — the caller (and the committed file) add one.
///
/// # Errors
///
/// Returns an error only if the generated schema fails to serialize — an
/// unreachable case for these fixed, derive-generated structs, surfaced as
/// a `Result` rather than a panic so the command path stays panic-free.
pub fn generate(kind: SchemaKind) -> anyhow::Result<String> {
    let schema = match kind {
        SchemaKind::Config => crate::config::project_config::config_json_schema(),
        SchemaKind::Publish => schemars::schema_for!(crate::command::publish::PublishManifest),
    };
    decorate(&schema, &kind.id(), kind.title())
}

/// Inject the published `$id`, friendly `title`, and the draft `$schema`
/// keyword into a generated schema, then pretty-print it.
///
/// Works on the serialized `serde_json::Value` (rather than `schemars`
/// mutators) so the decoration uses only stable serde_json APIs; `schemars`
/// already emits a draft-2020-12 `$schema` and a type-name `title`, both of
/// which this overwrites with the published values.
fn decorate(schema: &schemars::Schema, id: &str, title: &str) -> anyhow::Result<String> {
    let mut value = serde_json::to_value(schema)?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("generated JSON Schema is not an object"))?;
    obj.insert(
        "$schema".to_string(),
        serde_json::Value::String(SCHEMA_DRAFT.to_string()),
    );
    obj.insert("$id".to_string(), serde_json::Value::String(id.to_string()));
    obj.insert("title".to_string(), serde_json::Value::String(title.to_string()));
    Ok(serde_json::to_string_pretty(&value)?)
}

/// Run `grim schema`: print the JSON Schema for the requested kind to
/// stdout, followed by a trailing newline.
///
/// # Errors
///
/// Propagates a schema-serialization failure from [`generate`] (unreachable
/// in practice).
pub fn run(args: &SchemaArgs) -> anyhow::Result<ExitCode> {
    let json = generate(args.kind)?;
    println!("{json}");
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a kind's generated schema into a JSON value for inspection.
    fn parsed(kind: SchemaKind) -> serde_json::Value {
        serde_json::from_str(&generate(kind).expect("schema generates")).expect("generated schema is valid JSON")
    }

    #[test]
    fn config_schema_carries_id_title_draft_and_strict_object() {
        let v = parsed(SchemaKind::Config);
        assert_eq!(
            v["$id"],
            "https://michael-herwig.github.io/grimoire/schemas/grimoire-config.schema.json"
        );
        assert_eq!(v["$schema"], SCHEMA_DRAFT);
        assert_eq!(v["title"], "grimoire.toml — Grimoire declaration file");
        // `deny_unknown_fields` on RawConfig ⇒ additionalProperties:false, so
        // an editor flags a typo'd table key instead of silently dropping it.
        assert_eq!(v["additionalProperties"], serde_json::Value::Bool(false));
    }

    #[test]
    fn publish_schema_carries_id_title_and_requires_registry() {
        let v = parsed(SchemaKind::Publish);
        assert_eq!(
            v["$id"],
            "https://michael-herwig.github.io/grimoire/schemas/grim-publish.schema.json"
        );
        assert_eq!(v["$schema"], SCHEMA_DRAFT);
        assert_eq!(v["title"], "publish.toml — Grimoire publish manifest");
        assert_eq!(v["additionalProperties"], serde_json::Value::Bool(false));
        // `registry` is the one required top-level field.
        let required = v["required"].as_array().expect("required is an array");
        assert!(
            required.iter().any(|r| r == "registry"),
            "publish schema must require `registry`, got: {required:?}"
        );
    }

    #[test]
    fn each_kind_emits_a_distinct_id() {
        assert_ne!(SchemaKind::Config.id(), SchemaKind::Publish.id());
    }

    #[test]
    fn committed_config_schema_is_current() {
        assert_committed_matches(SchemaKind::Config, CONFIG_SCHEMA_FILE);
    }

    #[test]
    fn committed_publish_schema_is_current() {
        assert_committed_matches(SchemaKind::Publish, PUBLISH_SCHEMA_FILE);
    }

    /// Staleness gate: the file committed under `docs/src/schemas/` must
    /// equal the freshly generated schema (plus the trailing newline the
    /// `schema:generate` task writes). It only changes when the parse
    /// structs change, so a drift means the task was not re-run.
    fn assert_committed_matches(kind: SchemaKind, file: &str) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/src/schemas")
            .join(file);
        let committed =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read committed schema {}: {e}", path.display()));
        let expected = format!("{}\n", generate(kind).expect("schema generates"));
        assert_eq!(
            committed,
            expected,
            "{} is stale; regenerate it with `task schema:generate`",
            path.display()
        );
    }
}
