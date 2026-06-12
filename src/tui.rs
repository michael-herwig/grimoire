// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The catalog browser TUI.
//!
//! Decision logic is split out of the terminal so it is unit-testable
//! without a TTY: [`state`] holds the screen model and pure transitions
//! (no ratatui / crossterm / I/O imports), [`event`] maps inputs to
//! actions (pure), [`detail`] builds the detail pane's semantic content
//! and scroll geometry (pure, shared by state and render), [`render`] is
//! a thin pure projection of state into a plain [`render::RenderModel`]
//! plus the only ratatui-specific draw
//! function, and [`app`] owns the terminal, the async catalog load, and
//! the event loop — the one place the render loop and raw mode live, and
//! the one place excluded from acceptance tests.
//!
//! [`update_check`] adds background async update checks: its decisions
//! (eligibility, outdated derivation, debounce) are pure and headlessly
//! tested, while its spawn helpers — the only impure surface besides
//! [`app`] — run the bounded background tasks.
//!
//! [`init_dialog`] is the missing-config init prompt: a small popup-style
//! modal session (confirm + registry input) that runs before the main
//! browser when the scope has no `grimoire.toml` yet. Its state machine
//! is pure; its runner shares the raw-mode [`terminal_guard`] with
//! [`app`].

pub mod app;
pub mod detail;
pub mod event;
pub mod init_dialog;
pub mod render;
pub mod state;
pub mod terminal_guard;
pub mod update_check;

#[allow(unused_imports)]
pub use event::{TuiAction, TuiInput, handle};
#[allow(unused_imports)]
pub use render::RenderModel;
#[allow(unused_imports)]
pub use state::{Mode, TuiState};
#[allow(unused_imports)]
pub use update_check::{CheckMsg, UpdateChecker};
