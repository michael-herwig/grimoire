// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Floating-tag → pinned-digest resolution.
//!
//! Adapted from OCX `project::resolve`, trimmed to the Grimoire scope:
//! skills/rules instead of tools/groups, the single [`crate::oci::access`]
//! seam instead of a chained index, and a declaration-hash staleness gate
//! on the partial path that fires before any I/O. Resolution is fully
//! transactional — the first failure aborts every sibling and no partial
//! lock is produced.

pub mod resolve_error;
pub mod resolve_options;
pub mod resolver;

#[allow(unused_imports)]
pub use resolve_error::{ResolveError, ResolveErrorKind};
#[allow(unused_imports)]
pub use resolve_options::ResolveOptions;
#[allow(unused_imports)]
pub use resolver::{resolve_lock, resolve_lock_partial};
