//! Precision Instrument — design system for the compas TUI dashboard.
//!
//! Single source of truth for colors, borders, and panel constructors.
//! All dashboard views import from this module instead of defining ad-hoc styles.

use ratatui::{
    style::{Color, Style, Stylize},
    symbols::border,
    text::Line,
    widgets::{Block, Padding},
};

// ── Backgrounds ──────────────────────────────────────────────────────────────

/// Primary background — deep near-black.
pub const BG_PRIMARY: Color = Color::Rgb(12, 12, 16);

/// Panel background — slightly lifted from primary.
pub const BG_PANEL: Color = Color::Rgb(20, 20, 26);

/// Card background — for nested elements within panels.
pub const BG_CARD: Color = Color::Rgb(24, 24, 30);

/// Highlight/selection background.
pub const BG_HIGHLIGHT: Color = Color::Rgb(35, 35, 45);

/// Status bar background.
pub const BG_STATUS_BAR: Color = Color::Rgb(25, 25, 32);

// ── Borders ──────────────────────────────────────────────────────────────────

/// Dim border for unfocused panels.
pub const BORDER_DIM: Color = Color::Rgb(50, 50, 58);

/// Slightly brighter border for focused/active panels.
pub const BORDER_FOCUS: Color = Color::Rgb(80, 80, 90);

// ── Text ─────────────────────────────────────────────────────────────────────

/// Muted text — labels, timestamps, secondary information.
pub const TEXT_MUTED: Color = Color::Rgb(100, 100, 112);

/// Normal readable text — body content.
pub const TEXT_NORMAL: Color = Color::Rgb(180, 180, 190);

/// Bright text — emphasized values, selected items.
pub const TEXT_BRIGHT: Color = Color::Rgb(220, 220, 230);

/// Very dim text — decorative, batch IDs, tertiary info.
pub const TEXT_DIM: Color = Color::Rgb(70, 70, 80);

/// Inline code — soft blue-gray for `code` spans inside prose.
pub const TEXT_CODE: Color = Color::Rgb(140, 180, 220);

// ── Accent ───────────────────────────────────────────────────────────────────

/// Primary accent — warm amber. Active items, keybindings, focus indicators.
pub const ACCENT: Color = Color::Rgb(232, 135, 75);

/// Dimmed accent — scrollbar thumbs, unfocused active markers.
pub const ACCENT_DIM: Color = Color::Rgb(140, 70, 40);

// ── Semantic ─────────────────────────────────────────────────────────────────

/// Success — completed items, healthy status.
pub const SUCCESS: Color = Color::Rgb(80, 200, 120);

/// Dimmed success — completed items in lists.
pub const SUCCESS_DIM: Color = Color::Rgb(50, 130, 75);

/// Failure — failed, crashed, timed out.
pub const FAILURE: Color = Color::Rgb(200, 60, 60);

/// Warning — queued items, stale indicators.
pub const WARNING: Color = Color::Rgb(200, 170, 50);

// ── Progress bar characters ──────────────────────────────────────────────────

/// Filled segment for batch progress bars (thin line).
pub const BATCH_PROGRESS_FILLED: &str = "━";

/// Unfilled segment for batch progress bars (thin dashed line).
pub const BATCH_PROGRESS_EMPTY: &str = "╌";

// ── Status markers ───────────────────────────────────────────────────────────

/// Running / executing.
pub const MARKER_RUNNING: &str = "▸";

/// Queued / waiting.
pub const MARKER_QUEUED: &str = "◌";

/// Completed successfully.
pub const MARKER_COMPLETED: &str = "✓";

/// Failed / error.
pub const MARKER_FAILED: &str = "✗";

/// Timed out.
pub const MARKER_TIMEOUT: &str = "⏱";

/// Inactive / idle.
pub const MARKER_IDLE: &str = "·";

// ── Panel constructors ───────────────────────────────────────────────────────

/// Standard panel — ultra-thin borders, muted title, proportional padding.
pub fn panel(title: &str) -> Block<'_> {
    Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_DIM))
        .title_top(Line::from(format!(" {title} ")).left_aligned())
        .title_style(Style::new().fg(TEXT_MUTED))
        .padding(Padding::proportional(1))
        .style(Style::new().bg(BG_PANEL))
}

/// Focused panel — slightly brighter border, accent-colored title.
pub fn panel_focused(title: &str) -> Block<'_> {
    Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_FOCUS))
        .title_top(Line::from(format!(" {title} ")).left_aligned())
        .title_style(Style::new().fg(ACCENT))
        .padding(Padding::proportional(1))
        .style(Style::new().bg(BG_PANEL))
}

/// Card block — for nested elements (agent cards, etc.).
pub fn card(title: &str, health_color: Color) -> Block<'_> {
    Block::bordered()
        .border_set(border::ONE_EIGHTH_WIDE)
        .border_style(Style::new().fg(BORDER_DIM))
        .title_top(Line::from(format!(" ● {title} ").fg(health_color).bold()).left_aligned())
        .padding(Padding::new(2, 2, 0, 0))
        .style(Style::new().bg(BG_CARD))
}

// ── Semantic color helpers ───────────────────────────────────────────────────

/// Color for thread status values (Precision Instrument palette).
pub fn thread_status_color(status: &str) -> Color {
    match status {
        "Active" | "active" => ACCENT,
        "Completed" | "completed" => SUCCESS_DIM,
        "Failed" | "failed" => FAILURE,
        "Abandoned" | "abandoned" => TEXT_DIM,
        _ => TEXT_NORMAL,
    }
}

/// Color for execution status values (Precision Instrument palette).
pub fn exec_status_color(status: &str) -> Color {
    match status {
        "completed" => SUCCESS_DIM,
        "failed" | "crashed" | "timed_out" => FAILURE,
        "executing" | "picked_up" | "claimed" => ACCENT,
        "queued" => WARNING,
        "cancelled" => TEXT_DIM,
        _ => TEXT_NORMAL,
    }
}

/// Icon and color for a thread's status in list rows.
pub fn thread_status_icon(status: &str, exec_status: Option<&str>) -> (&'static str, Color) {
    match exec_status {
        Some("executing") | Some("picked_up") | Some("queued") => (MARKER_RUNNING, ACCENT),
        Some("failed") | Some("crashed") => (MARKER_FAILED, FAILURE),
        Some("timed_out") => (MARKER_TIMEOUT, FAILURE),
        Some("completed") => (MARKER_COMPLETED, SUCCESS_DIM),
        _ => match status {
            "Completed" => (MARKER_COMPLETED, SUCCESS_DIM),
            "Failed" => (MARKER_FAILED, FAILURE),
            "Abandoned" => (MARKER_IDLE, TEXT_DIM),
            _ => (" ", TEXT_NORMAL),
        },
    }
}

// ── Scrollbar style ──────────────────────────────────────────────────────────

/// Consistent scrollbar styling across all views.
pub fn scrollbar_thumb_style() -> Style {
    Style::new().fg(ACCENT_DIM)
}

pub fn scrollbar_track_style() -> Style {
    Style::new().fg(Color::Rgb(35, 35, 42))
}
