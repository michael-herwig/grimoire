// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! The catalog browser TUI.
//!
//! Decision logic is split out of the terminal so it is unit-testable
//! without a TTY: [`state`] holds the screen model and pure transitions
//! (no ratatui / crossterm / I/O imports), [`event`] maps inputs to
//! actions (pure), [`render`] is a thin pure projection of state into a
//! plain [`render::RenderModel`] plus the only ratatui-specific draw
//! function, and [`app`] owns the terminal, the async catalog load, and
//! the event loop — the one place the render loop and raw mode live, and
//! the one place excluded from acceptance tests.

pub mod app;
pub mod event;
pub mod render;
pub mod state;

#[allow(unused_imports)]
pub use event::{TuiAction, TuiInput, handle};
#[allow(unused_imports)]
pub use render::RenderModel;
#[allow(unused_imports)]
pub use state::{Mode, TuiState};
