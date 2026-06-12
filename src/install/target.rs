// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The set of AI client targets an install/update writes to.
//!
//! A list of [`ClientTarget`]s rooted at a workspace. The installer
//! iterates the targets, materializing each artifact into every selected
//! client's layout, so one install can generate for several clients at
//! once (e.g. Claude and OpenCode).
//!
//! When neither `--client` nor the config `[options].clients` selects a
//! client, the set defaults to **all detected clients** — those whose
//! vendor directory / marker is present for the scope (see
//! [`detect_clients`]). Detection finding nothing falls back to **all**
//! clients so an install never silently targets zero clients.

use std::path::{Path, PathBuf};

use crate::config::scope::ConfigScope;
use crate::oci::ArtifactKind;

use super::client_target::ClientTarget;
use super::install_error::InstallError;

/// One or more AI client targets rooted at a workspace.
#[derive(Debug, Clone)]
pub struct InstallTarget {
    workspace: PathBuf,
    scope: ConfigScope,
    clients: Vec<ClientTarget>,
}

impl InstallTarget {
    /// Build a target for the given clients rooted at `workspace` for
    /// `scope` (global scope resolves vendor-native user-level paths).
    ///
    /// An empty `clients` list defaults to the **detected** clients for
    /// `scope` (see [`detect_clients`]); when none are detected it falls
    /// back to **all** clients, so call sites with no `--client` and no
    /// config default never produce an empty (silent no-op) target.
    pub fn new(workspace: &Path, scope: ConfigScope, clients: Vec<ClientTarget>) -> Self {
        let clients = if clients.is_empty() {
            detect_clients(workspace, scope)
        } else {
            clients
        };
        Self {
            workspace: workspace.to_path_buf(),
            scope,
            clients,
        }
    }

    /// Parse a comma-separated / repeated `--client` list into an
    /// [`InstallTarget`]. An empty flag list falls back to the config
    /// `clients` default; when that is also empty, the detected clients for
    /// `scope` are used (see [`Self::new`]). Each value (flag or config) may
    /// itself be a comma list.
    ///
    /// # Errors
    ///
    /// [`super::install_error::InstallErrorKind::UnsupportedClient`] for
    /// an unknown client name.
    pub fn parse(
        workspace: &Path,
        scope: ConfigScope,
        flag_values: &[String],
        config_default: &[String],
    ) -> Result<Self, InstallError> {
        let source: &[String] = if flag_values.is_empty() {
            config_default
        } else {
            flag_values
        };
        // Both flag and config empty ⇒ reach `new` with an empty list so
        // detection runs (do not inject the literal "claude").
        let raw: Vec<String> = source
            .iter()
            .flat_map(|v| v.split(',').map(|s| s.trim().to_string()))
            .collect();

        let mut clients = Vec::new();
        for name in raw {
            if name.is_empty() {
                continue;
            }
            let client: ClientTarget = name.parse()?;
            if !clients.contains(&client) {
                clients.push(client);
            }
        }
        Ok(Self::new(workspace, scope, clients))
    }

    /// The client targets, in declared order (deduplicated).
    pub fn clients(&self) -> &[ClientTarget] {
        &self.clients
    }

    /// The workspace root the client roots sit under.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The scope this target installs for.
    pub fn scope(&self) -> ConfigScope {
        self.scope
    }

    /// The install path for `(kind, name)` under `client`.
    pub fn path_for(&self, client: ClientTarget, kind: ArtifactKind, name: &str) -> PathBuf {
        client.path_for(&self.workspace, self.scope, kind, name)
    }
}

/// The detected AI clients for `workspace` at `scope`, in
/// [`ClientTarget::ALL`] order: every client whose vendor directory /
/// marker is present (see [`super::vendor::Vendor::detect`]). When none are
/// detected the result falls back to **all** clients so the install /
/// update / TUI default set is never empty and no client is silently
/// preferred over another.
pub fn detect_clients(workspace: &Path, scope: ConfigScope) -> Vec<ClientTarget> {
    let detected: Vec<ClientTarget> = ClientTarget::ALL
        .into_iter()
        .filter(|c| c.vendor().detect(workspace, scope))
        .collect();
    if detected.is_empty() {
        ClientTarget::ALL.to_vec()
    } else {
        detected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_defaults_to_all_clients_when_nothing_detected() {
        // A bare workspace (no vendor dirs) detects nothing ⇒ all clients.
        let tmp = tempfile::tempdir().unwrap();
        let t = InstallTarget::new(tmp.path(), ConfigScope::Project, vec![]);
        assert_eq!(t.clients(), &ClientTarget::ALL);
    }

    #[test]
    fn empty_targets_detected_clients_in_all_order() {
        // `.opencode` + `.github/instructions` present, no `.claude` ⇒ the
        // detected set is [OpenCode, Copilot] in ClientTarget::ALL order.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".opencode")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".github").join("instructions")).unwrap();
        let t = InstallTarget::new(tmp.path(), ConfigScope::Project, vec![]);
        assert_eq!(t.clients(), &[ClientTarget::OpenCode, ClientTarget::Copilot]);
        // The same set reaches detection through `parse` (empty flag+config).
        let p = InstallTarget::parse(tmp.path(), ConfigScope::Project, &[], &[]).unwrap();
        assert_eq!(p.clients(), &[ClientTarget::OpenCode, ClientTarget::Copilot]);
    }

    #[test]
    fn explicit_config_overrides_detection() {
        // Even with `.opencode` present, an explicit config `clients`
        // declaration wins over detection.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".opencode")).unwrap();
        let t = InstallTarget::parse(tmp.path(), ConfigScope::Project, &[], &["claude".to_string()]).unwrap();
        assert_eq!(t.clients(), &[ClientTarget::Claude]);
    }

    #[test]
    fn detect_clients_fallback_is_all_clients() {
        // Project scope on a bare workspace is hermetic (global detection
        // reads the developer's real `~/.claude` etc.): nothing detected ⇒
        // every client, so no client is silently preferred.
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_clients(tmp.path(), ConfigScope::Project),
            ClientTarget::ALL.to_vec()
        );
    }

    #[test]
    fn parse_comma_list_dedups_and_orders() {
        let t = InstallTarget::parse(
            Path::new("/w"),
            ConfigScope::Project,
            &["claude,copilot".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(t.clients(), &[ClientTarget::Claude, ClientTarget::Copilot]);
        // Repeated flag values merge.
        let t2 = InstallTarget::parse(
            Path::new("/w"),
            ConfigScope::Project,
            &["copilot".to_string(), "copilot".to_string(), "claude".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(t2.clients(), &[ClientTarget::Copilot, ClientTarget::Claude]);
    }

    #[test]
    fn parse_falls_back_to_config_default() {
        // A config `clients` list (here two entries) is used when no flag.
        let t = InstallTarget::parse(
            Path::new("/w"),
            ConfigScope::Project,
            &[],
            &["opencode".to_string(), "claude".to_string()],
        )
        .unwrap();
        assert_eq!(t.clients(), &[ClientTarget::OpenCode, ClientTarget::Claude]);
        // `/w` does not exist ⇒ nothing detected ⇒ the all-clients fallback.
        let t2 = InstallTarget::parse(Path::new("/w"), ConfigScope::Project, &[], &[]).unwrap();
        assert_eq!(t2.clients(), &ClientTarget::ALL);
        // A flag list overrides the config default entirely.
        let t3 = InstallTarget::parse(
            Path::new("/w"),
            ConfigScope::Project,
            &["copilot".to_string()],
            &["claude".to_string()],
        )
        .unwrap();
        assert_eq!(t3.clients(), &[ClientTarget::Copilot]);
    }

    #[test]
    fn parse_rejects_unknown_client() {
        assert!(InstallTarget::parse(Path::new("/w"), ConfigScope::Project, &["vscode".to_string()], &[]).is_err());
    }

    #[test]
    fn path_for_delegates_to_client() {
        let t = InstallTarget::new(Path::new("/w"), ConfigScope::Project, vec![ClientTarget::Copilot]);
        assert_eq!(
            t.path_for(ClientTarget::Copilot, ArtifactKind::Rule, "rust-style"),
            PathBuf::from("/w/.github/instructions/rust-style.instructions.md")
        );
    }
}
