// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The machine-written, committed `grimoire.lock`: resolved pins plus the
//! metadata that ties the lock to a specific declared state.
//!
//! Adapted from OCX `project::{lock,project_lock,error}`, trimmed to the
//! Grimoire scope: skills/rules split by kind, no `projects.json` GC
//! registry, `chrono` for RFC3339, and the atomic write delegated to the
//! shared `store::atomic_write` primitive.

pub mod effective_set;
pub mod file_lock;
pub mod grimoire_lock;
pub mod lock_error;
pub mod lock_io;
pub mod lock_version;
pub mod locked_artifact;
pub mod locked_bundle;

#[allow(unused_imports)]
pub use file_lock::ConfigFileLock;
#[allow(unused_imports)]
pub use grimoire_lock::{GrimoireLock, LockMetadata};
#[allow(unused_imports)]
pub use lock_error::{LockError, LockErrorKind};
#[allow(unused_imports)]
pub use lock_version::LockVersion;
#[allow(unused_imports)]
pub use locked_artifact::LockedArtifact;
