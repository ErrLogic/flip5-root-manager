//! Panel layout (CLAUDE.md section 9): a full-width header and footer,
//! with a two-column body in between — device/artifacts/workflow stacked
//! on the left, and the terminal output panel taking the whole right
//! column so it gets the most vertical room (CLAUDE.md section 11: live
//! output must not be hidden/cramped).

use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub struct Chunks {
    pub header: Rect,
    pub device: Rect,
    pub artifacts: Rect,
    pub workflow: Rect,
    pub terminal: Rect,
    pub footer: Rect,
}

/// Width of the left (device/artifacts/workflow) column. Fixed rather
/// than percentage-based so it stays a predictable, comfortably readable
/// size regardless of how wide the terminal is; the right column (and
/// its terminal output) absorbs all remaining width.
///
/// 64 is the smallest width at which the longest artifact row (KernelSU's
/// filename, at the preferred `label · filename · size · hash` format)
/// still fits on one line — see `tui::widgets::artifact_block`.
const LEFT_COLUMN_WIDTH: u16 = 64;

pub fn compute(area: Rect) -> Chunks {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // header (title/device line + mode line)
            Constraint::Min(10),   // body (two columns)
            Constraint::Length(3), // footer
        ])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(LEFT_COLUMN_WIDTH), Constraint::Min(30)])
        .split(outer[1]);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // device (4 lines in Dry Run: Serial/Model/Build/Status)
            // Artifacts is a compact status summary, not an inspector: 3
            // single-row artifact lines + 2 repo lines, top-aligned, no
            // trailing padding. Sized to the common case exactly so
            // there's no dead space below it; reclaimed height goes to
            // Workflow instead, which needs it more during execution.
            Constraint::Length(7),
            Constraint::Min(10), // workflow checklist fills the rest
        ])
        .split(body[0]);

    Chunks {
        header: outer[0],
        device: left[0],
        artifacts: left[1],
        workflow: left[2],
        terminal: body[1],
        footer: outer[2],
    }
}
