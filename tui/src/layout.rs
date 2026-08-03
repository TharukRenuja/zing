use ratatui::layout::{Constraint, Layout, Rect};

pub const COLUMN_BREAK: u16 = 120;
/// Total height (border + content) of the bottom log strip. Shows the last
/// 3 log lines and auto-scrolls as new entries arrive.
pub const LOG_STRIP_HEIGHT: u16 = 5;
const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Stacked,
    Columns,
}

/// Rect for every region of the unified master-detail screen.
///
/// The top shows the selected task's progress/stats/connections/block map
/// with the task table directly below the block map; a fixed-height log strip
/// sits at the bottom; the footer holds the keymap.
pub struct UnifiedRects {
    pub title: Rect,
    pub header: Rect,
    pub stats: Rect,
    pub connections: Rect,
    pub blockmap: Rect,
    pub tasks: Rect,
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

/// Compute the unified layout, adapting to terminal size.
/// Returns `None` when the terminal is too small to render anything useful.
pub fn unified(area: Rect) -> Option<UnifiedRects> {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return None;
    }

    // The detail body (stats/connections/block map/tasks) takes all the space
    // above a fixed-height log strip at the bottom of the screen.
    let [title, header, body, logs, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(5),
        Constraint::Fill(1),
        Constraint::Length(LOG_STRIP_HEIGHT),
        Constraint::Length(2),
    ])
    .areas(area);

    match mode_for(area) {
        LayoutMode::Stacked => {
            let [stats, connections, blockmap, tasks] = Layout::vertical([
                Constraint::Length(4),
                Constraint::Fill(1),
                Constraint::Length(3),
                Constraint::Fill(1),
            ])
            .areas(body);

            Some(UnifiedRects {
                title,
                header,
                stats,
                connections,
                blockmap,
                tasks,
                logs,
                footer,
            })
        }
        LayoutMode::Columns => {
            // Left column: stats over the per-connection table. Right column:
            // the block map with the task table below it.
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Fill(1)])
                    .spacing(1)
                    .areas(body);

            let [stats, connections] =
                Layout::vertical([Constraint::Length(4), Constraint::Fill(1)]).areas(left);

            let [blockmap, tasks] =
                Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(right);

            Some(UnifiedRects {
                title,
                header,
                stats,
                connections,
                blockmap,
                tasks,
                logs,
                footer,
            })
        }
    }
}
