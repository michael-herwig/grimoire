// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Pull → materialize → integrity-gate → record: the grimoire divergence
//! from a plain OCI pull.
//!
//! [`materializer`] is the client-transform seam;
//! [`content_hash`] is the deterministic integrity anchor; [`target`]
//! resolves on-disk install paths; [`install_state`] persists what is
//! installed where; [`installer`] coordinates the per-artifact pass with
//! the local-modification refusal.

pub mod client_target;
pub mod content_hash;
pub mod install_error;
pub mod install_state;
pub mod installer;
pub mod materializer;
pub mod prune;
pub mod status_badge;
pub mod target;
pub mod uninstall;

#[allow(unused_imports)]
pub use client_target::{ClientTarget, MaterializedFile};
#[allow(unused_imports)]
pub use content_hash::{content_hash, footprint_hash};
#[allow(unused_imports)]
pub use install_error::{InstallError, InstallErrorKind};
#[allow(unused_imports)]
pub use install_state::{InstallRecord, InstallState};
#[allow(unused_imports)]
pub use installer::{ArtifactInstall, InstallOutcome, install_all};
#[allow(unused_imports)]
pub use materializer::{ArtifactMaterializer, DefaultMaterializer};
#[allow(unused_imports)]
pub use prune::{PruneError, PruneOutcome, PrunedArtifact, prune_orphans};
#[allow(unused_imports)]
pub use status_badge::{StatusBadge, derive_badge};
#[allow(unused_imports)]
pub use target::InstallTarget;
#[allow(unused_imports)]
pub use uninstall::{UninstallOutcome, UninstallResult, uninstall};
