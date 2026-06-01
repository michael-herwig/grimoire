// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! `grim login` — authenticate to a registry and persist the credential
//! through the docker-compatible store.
//!
//! Username is prompted on a TTY when `--username` is omitted; the
//! password is read via `--password-stdin` or a hidden TTY prompt. There
//! is intentionally **no** `--password VALUE` flag — an argv-visible
//! secret leaks through `ps` and shell history, so it is refused by
//! construction (clap has no such argument to parse).

use std::io::Read as _;

use clap::Args;
use secrecy::SecretString;
use secrecy::zeroize::Zeroizing;

use crate::api::login_report::LoginReport;
use crate::auth::credential::Credential;
use crate::auth::login as auth_login;
use crate::auth::prompt;
use crate::auth::store::{DockerCredentialStore, StoreOptions};
use crate::cli::exit_code::ExitCode;
use crate::context::Context;

/// `grim login` arguments.
#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Account name. Prompted on a TTY when omitted.
    #[arg(short = 'u', long)]
    pub username: Option<String>,

    /// Read the password / token from stdin instead of prompting.
    /// Required when stdin is not a terminal.
    #[arg(long)]
    pub password_stdin: bool,

    /// Store the credential as base64 plaintext in
    /// `~/.docker/config.json` when no credential helper is configured.
    /// Refused by default.
    #[arg(long)]
    pub allow_insecure_store: bool,

    /// Registry hostname (e.g. `ghcr.io`). Falls back to `--registry`,
    /// the `default_registry` option, or `GRIM_DEFAULT_REGISTRY`.
    pub registry: Option<String>,
}

/// Run `grim login`.
///
/// # Errors
///
/// A missing/empty credential input (usage error 64), a missing registry
/// (config error 78), or a credential-store failure (auth/I/O tiers).
pub async fn run(ctx: &Context, args: &LoginArgs) -> anyhow::Result<(LoginReport, ExitCode)> {
    let registry = super::resolve_login_registry(ctx, args.registry.as_deref())?;

    // Credential input blocks on a TTY / stdin read, so it runs on the
    // blocking pool — never parking an async worker thread (quality-rust.md).
    let username = match &args.username {
        Some(u) => u.clone(),
        None => {
            input_task(|| {
                prompt::prompt_username().map_err(|_| {
                    super::login_usage("non-interactive login requires --username (-u) when stdin is not a TTY")
                })
            })
            .await?
        }
    };
    if username.is_empty() {
        return Err(super::login_usage("username must not be empty"));
    }

    let password_stdin = args.password_stdin;
    let password = input_task(move || read_password(password_stdin)).await?;
    let cred = Credential::basic(username.clone(), password);

    let store = super::grim(DockerCredentialStore::new(StoreOptions {
        allow_plaintext_put: args.allow_insecure_store,
    }))?;
    super::grim(auth_login::login(&registry, &cred, &store).await)?;

    Ok((LoginReport::new(registry, username), ExitCode::Success))
}

/// Run a blocking credential-input closure on the blocking pool. A panic in
/// the closure is re-raised on the caller thread rather than swallowed.
async fn input_task<T, F>(f: F) -> anyhow::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(join) if join.is_panic() => std::panic::resume_unwind(join.into_panic()),
        Err(join) => Err(anyhow::anyhow!("credential input task failed: {join}")),
    }
}

/// Read the password from stdin (`--password-stdin`) or a hidden TTY
/// prompt. Empty input is rejected as a usage error in both modes. Called
/// inside [`input_task`] — blocking by design.
fn read_password(password_stdin: bool) -> anyhow::Result<SecretString> {
    if password_stdin {
        // Zeroized so the cleartext password does not linger on the heap
        // after it is wrapped in `SecretString`.
        let mut buf = Zeroizing::new(String::new());
        std::io::stdin().read_to_string(&mut buf).map_err(|source| {
            // I/O failure → IoError(74) via the classified store error, not
            // the generic Failure(1) a bare anyhow error would yield.
            anyhow::Error::from(crate::error::Error::from(crate::auth::auth_error::AuthError::StoreIo {
                path: std::path::PathBuf::from("<stdin>"),
                source,
            }))
        })?;
        // Strip a single trailing newline (the common `echo "$TOKEN" |`
        // case); a password is otherwise taken verbatim.
        let pass = buf.strip_suffix('\n').unwrap_or(&buf);
        let pass = pass.strip_suffix('\r').unwrap_or(pass);
        if pass.is_empty() {
            return Err(super::login_usage("--password-stdin received empty input"));
        }
        Ok(SecretString::from(pass.to_string()))
    } else {
        let pass = prompt::prompt_password()
            .map_err(|_| super::login_usage("non-interactive login requires --password-stdin (stdin is not a TTY)"))?;
        if secrecy::ExposeSecret::expose_secret(&pass).is_empty() {
            return Err(super::login_usage("password must not be empty"));
        }
        Ok(pass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    /// Helper harness so clap can parse the subcommand args in isolation.
    #[derive(clap::Parser)]
    struct Harness {
        #[command(subcommand)]
        cmd: Sub,
    }

    #[derive(clap::Subcommand)]
    enum Sub {
        Login(LoginArgs),
    }

    fn parse(args: &[&str]) -> Result<LoginArgs, clap::Error> {
        let mut argv = vec!["grim", "login"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).map(|h| match h.cmd {
            Sub::Login(a) => a,
        })
    }

    #[test]
    fn rejects_password_value_flag() {
        // CWE-214: there is no `--password VALUE`; clap must reject it.
        assert!(parse(&["--password", "x", "ghcr.io"]).is_err());
    }

    #[test]
    fn accepts_minimal_and_full_invocations() {
        parse(&["ghcr.io"]).expect("bare positional parses");
        parse(&["-u", "user", "--password-stdin", "--allow-insecure-store", "ghcr.io"]).expect("full flag set parses");
    }

    #[test]
    fn registry_is_optional_at_parse() {
        parse(&[]).expect("registry optional (resolved at runtime)");
    }
}
