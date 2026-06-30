// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! TUI install progress modal.
//!
//! The TUI awaits an install/update/uninstall action inline in its event
//! loop, so the screen is otherwise frozen for the whole operation.
//! [`InstallModal`] is an [`InstallProgress`] sink that repaints a centered
//! gauge dialog on the TUI terminal as each acted-on item is processed,
//! giving `n/total` feedback (or an indeterminate "working…" frame for a
//! single action) during that blocking window. It draws a standalone frame
//! (a dimmed backdrop plus the dialog); the event loop repaints the full UI
//! once the action returns.

use std::cell::{Cell, RefCell};
use std::io::Stdout;

use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph};

use crate::cli::printer::truncate_ellipsis;
use crate::install::progress::InstallProgress;

/// The concrete terminal the TUI owns (`run()` in `app.rs`).
type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// An [`InstallProgress`] sink that redraws a modal gauge on the TUI terminal.
///
/// Built in the event-loop arm that triggers the install (it borrows the
/// terminal), then passed down as `&dyn InstallProgress`. The whole TUI runs
/// single-threaded under one runtime, so the interior mutability is sound.
pub struct InstallModal<'t> {
    terminal: RefCell<&'t mut TuiTerminal>,
    /// Dialog title — the operation verb ("Installing", "Updating",
    /// "Uninstalling") so the same modal serves every batch/member action.
    title: &'static str,
    total: Cell<usize>,
}

impl<'t> InstallModal<'t> {
    /// Wrap the TUI terminal for the duration of one install/update action.
    /// `title` is the operation verb shown in the dialog header.
    pub fn new(terminal: &'t mut TuiTerminal, title: &'static str) -> Self {
        Self {
            terminal: RefCell::new(terminal),
            title,
            total: Cell::new(0),
        }
    }

    /// Draw an indeterminate "working" frame before the artifact count is
    /// known — covers the lock-resolution phase that precedes `install_all`,
    /// which would otherwise show a frozen UI.
    pub fn working(&self, label: &str) {
        self.render(0, 0, label);
    }

    fn render(&self, position: usize, total: usize, label: &str) {
        // Best-effort: a draw failure must never abort the install.
        let _ = self
            .terminal
            .borrow_mut()
            .draw(|f| draw_install_progress(f, position, total, label, self.title));
    }
}

impl InstallProgress for InstallModal<'_> {
    fn start(&self, total: usize) {
        self.total.set(total);
        self.render(0, total, "preparing…");
    }

    fn advance(&self, position: usize, label: &str) {
        self.render(position, self.total.get(), label);
    }

    fn finish(&self) {
        // Leave the last frame up; the event loop repaints the full UI next.
    }
}

/// Draw a centered modal progress dialog over a dimmed backdrop.
///
/// `total == 0` renders an indeterminate "working…" gauge (the artifact count
/// is not yet known, e.g. while the lock resolves). `title` is the operation
/// verb ("Installing", "Updating").
pub fn draw_install_progress(f: &mut Frame, position: usize, total: usize, label: &str, title: &str) {
    // Dimmed backdrop so the box reads as a modal, not a blank screen.
    f.render_widget(Block::default().style(Style::default().bg(Color::Black)), f.area());

    // Reuse the overflow-safe centering helper (ratatui Layout, u32 math)
    // rather than a raw u16 multiply that overflows past ~1092 columns.
    let area = super::render::centered_area_rows(f.area(), 60, 5);
    f.render_widget(Clear, area);

    let (ratio, gauge_label) = if total == 0 {
        (0.0, "working…".to_string())
    } else {
        let ratio = (position as f64 / total as f64).clamp(0.0, 1.0);
        (ratio, format!("{position}/{total}"))
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(
            // Magenta title over a Cyan border matches the picker/help modal
            // overlays (Cyan titles are reserved for persistent panels).
            format!(" {title} "),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate_ellipsis(label, inner.width as usize).into_owned(),
            Style::default().fg(Color::White),
        ))
        .alignment(Alignment::Center),
        rows[0],
    );
    f.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(Color::Cyan).bg(Color::Indexed(236)))
            .ratio(ratio)
            .label(gauge_label),
        rows[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn rendered(position: usize, total: usize, label: &str, title: &str) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal
            .draw(|f| draw_install_progress(f, position, total, label, title))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn draws_title_counter_and_label() {
        let content = rendered(1, 2, "skill code-review", "Installing");
        assert!(content.contains("Installing"), "modal title present");
        assert!(content.contains("1/2"), "counter present");
        assert!(content.contains("code-review"), "artifact label present");
    }

    #[test]
    fn title_reflects_the_operation_verb() {
        // The same modal serves updates — the title must follow the verb.
        let content = rendered(1, 1, "skill foo", "Updating");
        assert!(content.contains("Updating"), "update title present, got Installing?");
        assert!(
            !content.contains("Installing"),
            "install verb must not leak into an update"
        );
    }

    #[test]
    fn wide_terminal_does_not_overflow() {
        // Regression: a raw `area.width * width_pct` u16 multiply overflowed
        // past ~1092 columns (debug panic mid-draw / zero-width modal in
        // release). 1200 columns must render cleanly.
        let content = rendered(1, 2, "x", "Installing");
        assert!(content.contains("Installing"));
        let mut wide = Terminal::new(TestBackend::new(1200, 12)).unwrap();
        wide.draw(|f| draw_install_progress(f, 1, 2, "x", "Installing"))
            .unwrap();
        let wide_content: String = wide
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            wide_content.contains("Installing"),
            "modal renders on a very wide terminal"
        );
    }

    #[test]
    fn indeterminate_when_total_zero() {
        let content = rendered(0, 0, "resolving", "Installing");
        assert!(content.contains("working"), "zero total renders an indeterminate gauge");
        assert!(content.contains("resolving"), "label still shown");
    }
}
