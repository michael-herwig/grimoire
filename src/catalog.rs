// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The registry catalog: a cached, searchable index of the skills and
//! rules a registry publishes.
//!
//! Built over the [`crate::oci::access::OciAccess`] seam only — list the
//! repository catalog, pick a "latest"-ish tag per repo, and read the
//! skill/rule metadata straight off the manifest annotations (no blob
//! pull). Persisted at `$GRIM_HOME/catalog.json`, version-enveloped and
//! atomically written, with a 1 hour TTL; offline degrades to whatever is
//! cached rather than failing.

pub mod catalog_error;
pub mod catalog_service;
pub mod registry_catalog;
pub mod search_match;

#[allow(unused_imports)]
pub use catalog_error::{CatalogError, CatalogErrorKind};
#[allow(unused_imports)]
pub use catalog_service::{BadgeContext, CatalogGroup, CatalogResults, CatalogRow, load_catalog};
#[allow(unused_imports)]
pub use registry_catalog::{Catalog, CatalogEntry};
#[allow(unused_imports)]
pub use search_match::SearchQuery;
