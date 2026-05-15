// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Pull → materialize → integrity-gate → record: the grimoire divergence
//! from a plain OCI pull.
//!
//! [`materializer`] is the editor-transform seam Phase 5 extends;
//! [`content_hash`] is the deterministic integrity anchor; [`target`]
//! resolves on-disk install paths; [`install_state`] persists what is
//! installed where; [`installer`] coordinates the per-artifact pass with
//! the local-modification refusal.

pub mod content_hash;
pub mod editor_target;
pub mod install_error;
pub mod install_state;
pub mod installer;
pub mod materializer;
pub mod status_badge;
pub mod target;
pub mod uninstall;

#[allow(unused_imports)]
pub use content_hash::content_hash;
#[allow(unused_imports)]
pub use editor_target::{EditorTarget, MaterializedFile};
#[allow(unused_imports)]
pub use install_error::{InstallError, InstallErrorKind};
#[allow(unused_imports)]
pub use install_state::{InstallRecord, InstallState};
#[allow(unused_imports)]
pub use installer::{ArtifactInstall, InstallOutcome, install_all};
#[allow(unused_imports)]
pub use materializer::{ArtifactMaterializer, DefaultMaterializer};
#[allow(unused_imports)]
pub use status_badge::{StatusBadge, derive_badge};
#[allow(unused_imports)]
pub use target::InstallTarget;
#[allow(unused_imports)]
pub use uninstall::{UninstallOutcome, UninstallResult, uninstall};
