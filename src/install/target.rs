// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The set of AI client targets an install/update writes to.
//!
//! A list of [`ClientTarget`]s (default `[claude]`) rooted at a workspace.
//! The installer iterates the targets, materializing each artifact into
//! every selected client's layout, so one install can generate for several
//! clients at once (e.g. Claude and OpenCode). The Claude-only default path
//! behaves identically to a single-client install.

use std::path::{Path, PathBuf};

use crate::oci::ArtifactKind;

use super::client_target::ClientTarget;
use super::install_error::InstallError;

/// One or more AI client targets rooted at a workspace.
#[derive(Debug, Clone)]
pub struct InstallTarget {
    workspace: PathBuf,
    clients: Vec<ClientTarget>,
}

impl InstallTarget {
    /// Build a target for the given clients rooted at `workspace`.
    ///
    /// `clients` defaults to `[Claude]` when empty so call sites with no
    /// `--client` and no config default keep the single-client behavior.
    pub fn new(workspace: &Path, clients: Vec<ClientTarget>) -> Self {
        let clients = if clients.is_empty() {
            vec![ClientTarget::Claude]
        } else {
            clients
        };
        Self {
            workspace: workspace.to_path_buf(),
            clients,
        }
    }

    /// Parse a comma-separated / repeated `--client` list into an
    /// [`InstallTarget`]. An empty flag list falls back to the config
    /// `clients` default, then `claude`. Each value (flag or config) may
    /// itself be a comma list.
    ///
    /// # Errors
    ///
    /// [`super::install_error::InstallErrorKind::UnsupportedClient`] for
    /// an unknown client name.
    pub fn parse(workspace: &Path, flag_values: &[String], config_default: &[String]) -> Result<Self, InstallError> {
        let source: &[String] = if flag_values.is_empty() {
            config_default
        } else {
            flag_values
        };
        let raw: Vec<String> = if source.is_empty() {
            vec!["claude".to_string()]
        } else {
            source
                .iter()
                .flat_map(|v| v.split(',').map(|s| s.trim().to_string()))
                .collect()
        };

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
        Ok(Self::new(workspace, clients))
    }

    /// The client targets, in declared order (deduplicated).
    pub fn clients(&self) -> &[ClientTarget] {
        &self.clients
    }

    /// The workspace root the client roots sit under.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The install path for `(kind, name)` under `client`.
    pub fn path_for(&self, client: ClientTarget, kind: ArtifactKind, name: &str) -> PathBuf {
        client.path_for(&self.workspace, kind, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_defaults_to_claude() {
        let t = InstallTarget::new(Path::new("/w"), vec![]);
        assert_eq!(t.clients(), &[ClientTarget::Claude]);
    }

    #[test]
    fn parse_comma_list_dedups_and_orders() {
        let t = InstallTarget::parse(Path::new("/w"), &["claude,copilot".to_string()], &[]).unwrap();
        assert_eq!(t.clients(), &[ClientTarget::Claude, ClientTarget::Copilot]);
        // Repeated flag values merge.
        let t2 = InstallTarget::parse(
            Path::new("/w"),
            &["copilot".to_string(), "copilot".to_string(), "claude".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(t2.clients(), &[ClientTarget::Copilot, ClientTarget::Claude]);
    }

    #[test]
    fn parse_falls_back_to_config_default() {
        // A config `clients` list (here two entries) is used when no flag.
        let t = InstallTarget::parse(Path::new("/w"), &[], &["opencode".to_string(), "claude".to_string()]).unwrap();
        assert_eq!(t.clients(), &[ClientTarget::OpenCode, ClientTarget::Claude]);
        let t2 = InstallTarget::parse(Path::new("/w"), &[], &[]).unwrap();
        assert_eq!(t2.clients(), &[ClientTarget::Claude]);
        // A flag list overrides the config default entirely.
        let t3 = InstallTarget::parse(Path::new("/w"), &["copilot".to_string()], &["claude".to_string()]).unwrap();
        assert_eq!(t3.clients(), &[ClientTarget::Copilot]);
    }

    #[test]
    fn parse_rejects_unknown_client() {
        assert!(InstallTarget::parse(Path::new("/w"), &["vscode".to_string()], &[]).is_err());
    }

    #[test]
    fn path_for_delegates_to_client() {
        let t = InstallTarget::new(Path::new("/w"), vec![ClientTarget::Copilot]);
        assert_eq!(
            t.path_for(ClientTarget::Copilot, ArtifactKind::Rule, "rust-style"),
            PathBuf::from("/w/.github/instructions/rust-style.instructions.md")
        );
    }
}
