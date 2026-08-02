use ratatui::layout::{Constraint, Layout, Rect};

pub const COLUMN_BREAK: u16 = 120;
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Stacked,
    Columns,
}

pub struct PanelRects {
    pub header: Rect,
    pub stats: Rect,
    pub connections: Rect,
    pub blockmap: Rect,
    pub logs: Rect,
    pub footer: Rect,
}

pub fn mode_for(area: Rect) -> LayoutMode {
    if area.width >= COLUMN_BREAK {
        LayoutMode::Columns
    } else {
        LayoutMode::Stacked
    }
}

/// Compute the rect for every panel, adapting to terminal size.
/// Returns `None` when the terminal is too small to render anything useful.
pub fn compute(area: Rect, show_logs: bool) -> Option<PanelRects> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }

    // Auto-collapse the log panel on short terminals so core panels keep room.
    let logs_height = if show_logs && area.height >= 18 { 6 } else { 0 };

    match mode_for(area) {
        LayoutMode::Stacked => stacked(area, logs_height),
        LayoutMode::Columns => columns(area, logs_height),
    }
}

/// Rect layout for the batch/task-list view.
/// Returns `None` when the terminal is too small to render anything useful.
pub fn list(area: Rect, show_logs: bool, show_input: bool) -> Option<(Rect, Rect, Rect, Rect)> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }

    let logs_height = if show_input || (show_logs && area.height >= 18) {
        6
    } else {
        0
    };

    let [header, table, logs, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(logs_height),
        Constraint::Length(2),
    ])
    .areas(area);

    Some((header, table, logs, footer))
}

fn stacked(area: Rect, logs_height: u16) -> Option<PanelRects> {
    let [header, stats, connections, blockmap, logs, footer] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Fill(1),
        Constraint::Length(3),
        Constraint::Length(logs_height),
        Constraint::Length(2),
    ])
    .areas(area);

    Some(PanelRects {
        header,
        stats,
        connections,
        blockmap,
        logs,
        footer,
    })
}

fn columns(area: Rect, logs_height: u16) -> Option<PanelRects> {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(area);

    // Left column holds the per-connection table; give it a fixed-ish share
    // that keeps the table readable, right column fills the rest so the block
    // map and logs have room on wide terminals.
    let [left, right] = Layout::horizontal([Constraint::Percentage(50), Constraint::Fill(1)])
        .spacing(1)
        .areas(body);

    let [stats, connections] =
        Layout::vertical([Constraint::Length(4), Constraint::Fill(1)]).areas(left);

    // Logs fill all remaining height in the right column instead of a fixed
    // 6 rows, so a wide terminal shows far more history.
    let [blockmap, logs] = if logs_height > 0 {
        Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(right)
    } else {
        // Logs hidden: give the block map the full right column.
        Layout::vertical([Constraint::Fill(1), Constraint::Length(0)]).areas(right)
    };

    Some(PanelRects {
        header,
        stats,
        connections,
        blockmap,
        logs,
        footer,
    })
}
