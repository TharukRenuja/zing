use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, Paragraph, Row, Table};
use ratatui::Frame;
use zing_core::downloader::TaskSnapshot;

use crate::layout;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    snap: &TaskSnapshot,
    logs: &[String],
    scroll: usize,
    show_logs: bool,
) {
    match layout::compute(area, show_logs) {
        None => render_too_small(frame, area),
        Some(rects) => {
            render_header(frame, rects.header, snap);
            render_stats(frame, rects.stats, snap);
            render_connections(frame, rects.connections, snap, scroll);
            render_block_map(frame, rects.blockmap, snap);
            if rects.logs.height >= 2 {
                render_logs(frame, rects.logs, logs);
            }
            render_footer(frame, rects.footer, snap);
        }
    }
}

fn panel_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(format!(" {title} "))
}

fn render_too_small(frame: &mut Frame, area: Rect) {
    let text = Paragraph::new(" Terminal too small — enlarge the window ")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::Yellow));
    frame.render_widget(text, area);
}

fn render_header(frame: &mut Frame, area: Rect, snap: &TaskSnapshot) {
    let name = snap.filename.rsplit('/').next().unwrap_or(&snap.filename);
    let label = if name.is_empty() { "download" } else { name };

    let pct = if snap.total_bytes > 0 {
        ((snap.bytes_downloaded as f64 / snap.total_bytes as f64 * 100.0) as u16).min(100)
    } else {
        0
    };

    let downloaded = human_bytes(snap.bytes_downloaded);
    let total = human_bytes(snap.total_bytes);
    let speed = human_bytes(snap.speed);

    let status = if snap.done {
        "done"
    } else if snap.paused {
        "paused"
    } else {
        "downloading"
    };

    let title = Line::from(vec![
        Span::styled(
            " zing ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {label}")),
        Span::styled(format!(" ({status})"), Style::default().fg(Color::Gray)),
    ]);

    let info = Line::from(vec![
        Span::styled("Downloaded ", Style::default().fg(Color::Gray)),
        Span::styled(downloaded.as_str(), Style::default().fg(Color::White)),
        Span::styled("  /  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Total ", Style::default().fg(Color::Gray)),
        Span::styled(total.as_str(), Style::default().fg(Color::White)),
        Span::styled("  ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{speed}/s"), Style::default().fg(Color::Green)),
        Span::styled("  ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{pct}%"), Style::default().fg(Color::Yellow)),
    ]);

    let gauge_color = if snap.done { Color::Cyan } else { Color::Green };

    let gauge = Gauge::default()
        .block(panel_block("Progress"))
        .gauge_style(Style::default().fg(gauge_color).bg(Color::Reset))
        .label(format!("{pct}%  {downloaded} / {total}  {speed}/s"))
        .percent(pct);

    let [title_area, info_area, gauge_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);

    frame.render_widget(title, title_area);
    frame.render_widget(info, info_area);
    frame.render_widget(gauge, gauge_area);
}

fn stat_line(cells: Vec<(&str, String, Color)>, cell_width: usize) -> Line<'static> {
    let mut spans = Vec::new();
    for (label, value, color) in cells {
        let label_span = format!("{label} ");
        let cell = format!("{label_span}{value}");
        let pad = cell_width.saturating_sub(cell.chars().count());
        spans.push(Span::styled(label_span, Style::default().fg(Color::Gray)));
        spans.push(Span::styled(value, Style::default().fg(color)));
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

fn render_stats(frame: &mut Frame, area: Rect, snap: &TaskSnapshot) {
    let block = panel_block("Stats");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let in_flight = snap
        .connections
        .iter()
        .filter(|c| c.segment_id.is_some())
        .count();

    let eta = if snap.speed > 0 && !snap.done {
        let remaining = snap.total_bytes.saturating_sub(snap.bytes_downloaded);
        format!("{}s", remaining / snap.speed.max(1))
    } else if snap.done {
        "done".to_string()
    } else {
        "—".to_string()
    };

    let cell_width = (inner.width / 4).max(12) as usize;

    let line1 = stat_line(
        vec![
            (
                "Downloaded",
                human_bytes(snap.bytes_downloaded),
                Color::Green,
            ),
            ("Total", human_bytes(snap.total_bytes), Color::White),
            (
                "Speed",
                format!("{}/s", human_bytes(snap.speed)),
                Color::Green,
            ),
            (
                "Peak",
                format!("{}/s", human_bytes(snap.peak_speed)),
                Color::Cyan,
            ),
        ],
        cell_width,
    );
    let line2 = stat_line(
        vec![
            ("ETA", eta, Color::Yellow),
            (
                "Blocks",
                format!("{}/{}", snap.completed_blocks, snap.total_blocks),
                Color::White,
            ),
            ("In-flight", in_flight.to_string(), Color::Blue),
            (
                "Endgame",
                if snap.endgame { "ON" } else { "OFF" }.to_string(),
                if snap.endgame {
                    Color::Red
                } else {
                    Color::DarkGray
                },
            ),
        ],
        cell_width,
    );

    let para = Paragraph::new(vec![line1, line2]);
    frame.render_widget(para, inner);
}

fn render_connections(frame: &mut Frame, area: Rect, snap: &TaskSnapshot, scroll: usize) {
    let block = panel_block("Connections");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if snap.connections.is_empty() {
        let msg = if snap.total_bytes > 0 {
            " Starting download..."
        } else {
            " Probing server..."
        };
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(Color::Gray)),
            inner,
        );
        return;
    }

    let header_style = Style::default()
        .add_modifier(Modifier::BOLD)
        .fg(Color::Cyan);
    let header_cells = [" #", "ADDR", "SPEED", "BYTES", "TIME", "STATE"]
        .iter()
        .map(|h| Span::styled(*h, header_style));
    let header = Row::new(header_cells);

    let max_rows = (inner.height.saturating_sub(1)).max(1) as usize;
    let visible_connections: Vec<_> = snap
        .connections
        .iter()
        .skip(scroll)
        .take(max_rows)
        .collect();

    let rows: Vec<Row> = visible_connections
        .iter()
        .map(|c| {
            let addr_short = truncate_addr(&c.addr, 14);
            let speed_str = if c.speed_bytes_per_sec > 0.0 {
                format!("{}/s", human_bytes(c.speed_bytes_per_sec as u64))
            } else {
                "-".to_string()
            };
            let secs = c.started_at.elapsed().as_secs();
            let time = format!("{}s", secs);
            let (state_label, state_color) = match c.segment_id {
                Some(_) => ("● active", Color::Green),
                None => ("◐ idle", Color::Gray),
            };
            let byt = if c.bytes_downloaded > 0 {
                human_bytes(c.bytes_downloaded)
            } else {
                "-".to_string()
            };

            Row::new(vec![
                Span::raw(format!("{}", c.id)),
                Span::raw(addr_short),
                Span::styled(speed_str, Style::default().fg(Color::Green)),
                Span::raw(byt),
                Span::raw(time),
                Span::styled(state_label, Style::default().fg(state_color)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(3),
        Constraint::Length(15),
        Constraint::Length(12),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(8),
    ];

    let table = Table::new(rows, widths).header(header);
    frame.render_widget(table, inner);
}

fn render_block_map(frame: &mut Frame, area: Rect, snap: &TaskSnapshot) {
    if snap.total_bytes == 0 {
        frame.render_widget(panel_block("Blocks"), area);
        return;
    }

    let pct = if snap.total_blocks > 0 {
        ((snap.completed_blocks as f64 / snap.total_blocks as f64 * 100.0) as u16).min(100)
    } else {
        ((snap.bytes_downloaded as f64 / snap.total_bytes as f64 * 100.0) as u16).min(100)
    };

    let in_flight = snap
        .connections
        .iter()
        .filter(|c| c.segment_id.is_some())
        .count();

    let endgame_str = if snap.endgame { "ON" } else { "OFF" };
    let speed = human_bytes(snap.speed);

    let eta = if snap.speed > 0 && !snap.done {
        let remaining = snap.total_bytes.saturating_sub(snap.bytes_downloaded);
        let secs = remaining / snap.speed.max(1);
        format!(" ETA: {}s", secs)
    } else if snap.done {
        " Complete".to_string()
    } else {
        String::new()
    };

    let label = format!(
        "Blocks  {}/{}  {pct}%  {} in-flight  end-game: {}  {}/s{}",
        snap.completed_blocks, snap.total_blocks, in_flight, endgame_str, speed, eta,
    );

    let gauge = Gauge::default()
        .block(panel_block("Blocks"))
        .gauge_style(Style::default().fg(Color::Blue))
        .label(label)
        .percent(pct);

    frame.render_widget(gauge, area);
}

fn styled_log_line(text: &str) -> Line<'static> {
    let clean = clean_log_line(text);
    let (color, modifier) = if clean.contains(" ERROR ") {
        (Color::Red, Modifier::BOLD)
    } else if clean.contains(" WARN ") {
        (Color::Yellow, Modifier::empty())
    } else if clean.contains(" DEBUG ") {
        (Color::DarkGray, Modifier::empty())
    } else {
        (Color::Gray, Modifier::empty())
    };
    Line::styled(clean, Style::default().fg(color).add_modifier(modifier))
}

/// Remove ANSI escape sequences and the leading RFC3339 timestamp from a
/// tracing log line. The subscriber emits `compact()` formatted lines such as
/// `2026-08-01T12:34:56.123456Z  INFO target: message`; in the TUI the
/// timestamp and color codes are noise that eats horizontal space.
fn clean_log_line(text: &str) -> String {
    let without_ansi = strip_ansi(text);
    let level_start = without_ansi
        .find("DEBUG ")
        .or_else(|| without_ansi.find("INFO "))
        .or_else(|| without_ansi.find("WARN "))
        .or_else(|| without_ansi.find("ERROR "))
        .or_else(|| without_ansi.find("TRACE "));
    match level_start {
        Some(idx) => without_ansi[idx..].to_string(),
        None => without_ansi,
    }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Skip CSI sequence up to the terminating letter
            i += 2;
            while i < bytes.len() && !(b'@'..=b'~').contains(&bytes[i]) {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn render_logs(frame: &mut Frame, area: Rect, logs: &[String]) {
    let block = panel_block("Logs");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    let mut recent: Vec<String> = logs.iter().rev().take(height).cloned().collect();
    recent.reverse();

    let pad = height.saturating_sub(recent.len());
    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for _ in 0..pad {
        lines.push(Line::from(""));
    }
    for line in recent {
        lines.push(styled_log_line(&line));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_footer(frame: &mut Frame, area: Rect, snap: &TaskSnapshot) {
    let peak = human_bytes(snap.peak_speed);

    let pause_label = if snap.paused { "resume" } else { "pause" };
    let text = Line::from(vec![
        Span::styled(
            " q ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("quit", Style::default().fg(Color::Gray)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " p ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(pause_label, Style::default().fg(Color::Gray)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " x ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("stop", Style::default().fg(Color::Gray)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " j/k ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("scroll", Style::default().fg(Color::Gray)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " l ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("logs", Style::default().fg(Color::Gray)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" Peak: {peak}/s")),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title_bottom(text.centered());

    frame.render_widget(block, area);
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0B".to_string();
    }
    let mut b = bytes as f64;
    let mut unit = 0;
    while b > 1024.0 && unit < UNITS.len() - 1 {
        b /= 1024.0;
        unit += 1;
    }
    format!("{:.1}{}", b, UNITS[unit])
}

fn truncate_addr(addr: &str, max: usize) -> String {
    if addr.len() > max {
        format!("{}…", &addr[..max.saturating_sub(1)])
    } else {
        addr.to_string()
    }
}
