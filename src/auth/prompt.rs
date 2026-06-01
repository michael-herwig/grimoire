// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! Interactive terminal prompts for `grim login`.
//!
//! Prompts are written to **stderr** so stdout stays a clean machine
//! interface (a `--format json` `LoginReport` is the only thing on stdout).
//! Both prompts refuse a non-terminal stdin with [`io::ErrorKind::Unsupported`]
//! so the command can map that to an actionable usage error instead of a
//! confusing read failure.

use std::io::{self, IsTerminal as _, Write as _};

use secrecy::SecretString;

/// Prompt for and read a username line from the terminal.
///
/// # Errors
///
/// [`io::ErrorKind::Unsupported`] when stdin is not a TTY; any other I/O
/// error from writing the prompt or reading the line.
pub fn prompt_username() -> io::Result<String> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "stdin is not a terminal"));
    }
    let mut stderr = io::stderr();
    write!(stderr, "Username: ")?;
    stderr.flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Prompt for and read a password from the terminal without echoing it.
///
/// # Errors
///
/// [`io::ErrorKind::Unsupported`] when stdin is not a TTY; any other I/O
/// error from writing the prompt or reading the hidden input.
pub fn prompt_password() -> io::Result<SecretString> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "stdin is not a terminal"));
    }
    let mut stderr = io::stderr();
    write!(stderr, "Password: ")?;
    stderr.flush()?;
    // `read_password` reads hidden input from the terminal; the prompt was
    // already emitted on stderr above so stdout stays clean.
    Ok(SecretString::from(rpassword::read_password()?))
}
