// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Authentication subsystem: the credential model, a docker-compatible
//! credential store, and the login/logout operations.
//!
//! `grim` already *reads* registry credentials in
//! [`crate::oci::access::registry_client`] (docker config + helpers, via
//! the `docker_credential` crate). This module adds the *write* side that
//! `grim login` / `grim logout` need: a [`CredentialStore`] over
//! `~/.docker/config.json`, backed by the patched `docker_credential`
//! fork's helper store/erase primitives. Read and write share one
//! registry-key normalization ([`canonicalize_registry`]) so a credential
//! written by `grim login` is found by a later resolve.

pub mod auth_error;
pub mod credential;
pub mod login;
pub mod prompt;
pub mod registry_url;
pub mod store;

// The only re-export with a cross-module consumer: the credential read path
// (`oci::access::registry_client`) and the store share one registry-key
// normalization. Other subsystem types are reached via their full module
// path (e.g. `auth::store::DockerCredentialStore`), matching the rest of the
// crate, so they are not re-exported here.
pub use registry_url::canonicalize_registry;
