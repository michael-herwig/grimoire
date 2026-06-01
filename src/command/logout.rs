// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim logout` — remove a registry credential from the
//! docker-compatible store.
//!
//! Idempotent: logging out when nothing is stored (or when no store can
//! even be located) exits `Success(0)`, matching `docker logout` /
//! `oras logout` so CI cleanup never fails.

use clap::Args;

use crate::api::login_report::LogoutReport;
use crate::auth::login as auth_login;
use crate::auth::store::{DockerCredentialStore, StoreOptions};
use crate::cli::exit_code::ExitCode;
use crate::context::Context;

/// `grim logout` arguments.
#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// Registry hostname (e.g. `ghcr.io`). Falls back to `--registry`,
    /// the `default_registry` option, or `GRIM_DEFAULT_REGISTRY`.
    pub registry: Option<String>,
}

/// Run `grim logout`.
///
/// # Errors
///
/// A missing registry (config error 78) or a genuine credential-store
/// failure during erase (auth/I/O tiers). An absent credential is not an
/// error.
pub async fn run(ctx: &Context, args: &LogoutArgs) -> anyhow::Result<(LogoutReport, ExitCode)> {
    let registry = super::resolve_login_registry(ctx, args.registry.as_deref())?;

    // When no store location can be resolved (no $HOME, no $DOCKER_CONFIG)
    // there is nothing to log out from — a true no-op, exit 0.
    match DockerCredentialStore::new(StoreOptions::default()) {
        Ok(store) => super::grim(auth_login::logout(&registry, &store).await)?,
        Err(err) => tracing::debug!(%err, "logout: no credential store to act on; treating as no-op"),
    }

    Ok((LogoutReport::new(registry), ExitCode::Success))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: Sub,
    }

    #[derive(clap::Subcommand)]
    enum Sub {
        Logout(LogoutArgs),
    }

    fn parse(args: &[&str]) -> Result<LogoutArgs, clap::Error> {
        let mut argv = vec!["grim", "logout"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).map(|h| match h.cmd {
            Sub::Logout(a) => a,
        })
    }

    #[test]
    fn registry_is_optional_and_positional() {
        parse(&[]).expect("registry optional");
        let a = parse(&["ghcr.io"]).expect("positional registry parses");
        assert_eq!(a.registry.as_deref(), Some("ghcr.io"));
    }
}
