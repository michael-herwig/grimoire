// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The developer-editable `grimoire.toml` declaration: the desired set of
//! skills and rules, plus the canonical declaration hash that pins the
//! lock to a specific declared state.
//!
//! Adapted from OCX `project::{config,hash,error}`, renamed and trimmed:
//! Grimoire has two independent scopes (global / project, never merged),
//! `skills`/`rules` tables instead of `tools`/`groups`, and the JCS
//! canonicalization is implemented in-tree (no extra crate).

pub mod config_error;
pub mod declaration;
pub mod global_config;
pub mod hash;
pub mod project_config;
pub mod registry_resolve;
pub mod scope;

#[allow(unused_imports)]
pub use config_error::{ConfigError, ConfigErrorKind};
#[allow(unused_imports)]
pub use declaration::{ConfigOptions, DesiredSet, RegistryConfig};
#[allow(unused_imports)]
pub use global_config::GlobalConfig;
#[allow(unused_imports)]
pub use hash::{DECLARATION_HASH_VERSION, declaration_hash};
#[allow(unused_imports)]
pub use project_config::{DiscoveredConfig, ProjectConfig};
#[allow(unused_imports)]
pub use registry_resolve::{ResolvedRegistry, primary_registry, resolve_reference, resolve_registries};
#[allow(unused_imports)]
pub use scope::ConfigScope;

/// Maximum size of a `grimoire.toml` / `grimoire.lock` file. A larger
/// file is a sanity failure (pathological input in CI), surfaced as a
/// structured error rather than a degenerate parse.
pub const FILE_SIZE_LIMIT_BYTES: u64 = 64 * 1024;

use std::path::Path;

/// Read a config-tier file at `path`, enforcing the 64 KiB size cap.
///
/// `metadata().len()` fast-paths a normal oversized file without reading
/// any bytes; the post-read length re-check guards synthetic files
/// (procfs, pipes) whose metadata reports 0 but whose read is unbounded.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::Io`] on open/read failure (including
/// not-found) and [`ConfigErrorKind::FileTooLarge`] when the cap is
/// exceeded — both with `path` context.
pub fn read_capped(path: &Path) -> Result<String, ConfigError> {
    use std::io::Read;

    let file = std::fs::File::open(path).map_err(|e| ConfigError::new(path, ConfigErrorKind::Io(e)))?;
    let len = file
        .metadata()
        .map_err(|e| ConfigError::new(path, ConfigErrorKind::Io(e)))?
        .len();
    if len > FILE_SIZE_LIMIT_BYTES {
        return Err(ConfigError::new(
            path,
            ConfigErrorKind::FileTooLarge {
                size: len,
                limit: FILE_SIZE_LIMIT_BYTES,
            },
        ));
    }

    let mut content = String::new();
    file.take(FILE_SIZE_LIMIT_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|e| ConfigError::new(path, ConfigErrorKind::Io(e)))?;
    if content.len() as u64 > FILE_SIZE_LIMIT_BYTES {
        return Err(ConfigError::new(
            path,
            ConfigErrorKind::FileTooLarge {
                size: content.len() as u64,
                limit: FILE_SIZE_LIMIT_BYTES,
            },
        ));
    }
    Ok(content)
}
