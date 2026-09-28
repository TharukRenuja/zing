//! Shared progress-bar rendering.
//!
//! Standalone downloads render [`zing_core::engine::EngineEvent`] directly,
//! while daemon mode receives the same events as JSON over a socket. Both feed
//! the same [`BarDisplay`] so the two paths cannot drift apart.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use indicatif::{MultiProgress, ProgressBar};

/// Registry for the active progress display.
///
/// Interactive prompts and log writes must suspend the bars: indicatif redraws
/// on a timer, so text written straight to stderr is erased within a frame.
static PROGRESS_DISPLAY: OnceLock<Mutex<Option<Arc<MultiProgress>>>> = OnceLock::new();

pub fn progress_display() -> &'static Mutex<Option<Arc<MultiProgress>>> {
    PROGRESS_DISPLAY.get_or_init(|| Mutex::new(None))
}

fn publish(mp: &Arc<MultiProgress>) {
    if let Ok(mut slot) = progress_display().lock() {
        *slot = Some(Arc::clone(mp));
    }
}

/// Run `f` with the progress bars suspended so its output stays visible.
pub fn without_progress<T>(f: impl FnOnce() -> T) -> T {
    // `try_lock` so a write racing the display registration cannot deadlock.
    let display = progress_display().try_lock().ok().and_then(|g| g.clone());
    match display {
        Some(mp) => mp.suspend(f),
        None => f(),
    }
}

/// Print a line beneath the progress bars, clearing their region first.
///
/// A finished bar is two lines tall, so a bare `println!` would land on top of
/// or below a leftover bar and read as a second progress bar. `MultiProgress`
/// does the cursor arithmetic under its own lock, which also avoids racing the
/// bar renderer.
pub fn print_below_bars(line: &str) {
    let display = progress_display().try_lock().ok().and_then(|g| g.clone());
    match display {
        Some(mp) => {
            let _ = mp.println(line);
        }
        None => println!("{line}"),
    }
}

/// How much of the layout fits in the current terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarLayout {
    /// Two full lines with every field.
    Wide,
    /// Two lines, smaller sizes and no block count.
    Medium,
    /// One line with the essentials only.
    Narrow,
}

impl BarLayout {
    pub fn for_width(width: usize) -> Self {
        match width {
            0..=59 => Self::Narrow,
            60..=95 => Self::Medium,
            _ => Self::Wide,
        }
    }
}

/// Right-hand status text for the first line: block count, connections, and
/// the end-game marker. Fields that do not apply are dropped so the line stays
/// readable instead of showing `0/0 blocks`.
pub fn status_text(
    connections: usize,
    completed_blocks: u32,
    total_blocks: u32,
    endgame: bool,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if total_blocks > 0 {
        parts.push(format!("{completed_blocks}/{total_blocks} blocks"));
    }
    if connections > 0 {
        parts.push(if connections == 1 {
            "1 conn".to_string()
        } else {
            format!("{connections} conns")
        });
    }
    if endgame {
        parts.push("endgame".to_string());
    }
    parts.join("  ·  ")
}

/// Bar glyph sets. The first is the default; the others are one line of config
/// away in [`bar_style_for`].
pub const BAR_GLYPHS: [(&str, &str, &str); 4] = [
    ("=", ">", "."), // classic: [========>......]
    ("█", "█", "░"), // solid blocks
    ("━", "━", "╌"), // rounded
    ("▰", "▰", "▱"), // thin blocks
];

/// Build the two-line style for a layout tier and glyph set.
fn bar_style_for(layout: BarLayout, glyphs: (&str, &str, &str)) -> indicatif::ProgressStyle {
    let (fill, head, empty) = glyphs;
    let line1 = match layout {
        BarLayout::Narrow => "{prefix:.bold.yellow} {wide_msg:>}",
        _ => "{prefix:.bold.yellow}{wide_msg:>}",
    };
    let line2 = match layout {
        BarLayout::Wide => {
            "  [{wide_bar:.cyan}] {percent:>3}%  {bytes}/{total_bytes}  \
             {bytes_per_sec}  eta {eta}  ⏱ {elapsed}"
        }
        BarLayout::Medium => {
            "  [{wide_bar:.cyan}] {percent:>3}%  {bytes}/{total_bytes}  \
             {bytes_per_sec}  {eta}"
        }
        BarLayout::Narrow => "  [{wide_bar:.cyan}] {percent:>3}%  {bytes_per_sec}",
    };
    indicatif::ProgressStyle::default_bar()
        .template(&format!("{line1}\n{line2}"))
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
        .progress_chars(&format!("{fill}{head}{empty}"))
}

/// The style for downloads whose total size is unknown.
fn bar_style_unknown(layout: BarLayout) -> indicatif::ProgressStyle {
    let line1 = "{prefix:.bold.yellow}{wide_msg:>}";
    let line2 = match layout {
        BarLayout::Narrow => "  [{wide_bar:.cyan}] {bytes}  {bytes_per_sec}",
        _ => "  [{wide_bar:.cyan}] {bytes}  {bytes_per_sec}  ⏱ {elapsed}",
    };
    indicatif::ProgressStyle::default_bar()
        .template(&format!("{line1}\n{line2}"))
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar())
        .progress_chars("=>.")
}

#[cfg(unix)]
pub fn terminal_width() -> usize {
    use std::os::fd::AsRawFd;
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(std::io::stderr().as_raw_fd(), libc::TIOCGWINSZ, &mut ws) == 0
            && ws.ws_col > 0
        {
            ws.ws_col as usize
        } else {
            80
        }
    }
}

#[cfg(not(unix))]
pub fn terminal_width() -> usize {
    80
}

/// Everything the bar needs about one transfer, independent of where the
/// event came from.
#[derive(Debug, Clone, Default)]
pub struct ProgressView {
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    #[allow(dead_code)]
    pub speed_bytes_per_sec: f64,
    pub connections: usize,
    pub completed_blocks: u32,
    pub total_blocks: u32,
    pub endgame: bool,
}

/// Per-task render state.
struct Entry {
    bar: ProgressBar,
    total: Option<u64>,
    layout: BarLayout,
    /// Set while frozen by a pause, so the tick can be restarted on resume.
    paused: bool,
    /// Most recent pre-transfer stage, shown until real progress arrives.
    phase: Option<String>,
}

/// Renders one bar per task and adapts as the terminal changes size.
pub struct BarDisplay {
    mp: Arc<MultiProgress>,
    bars: HashMap<u64, Entry>,
    glyphs: (&'static str, &'static str, &'static str),
    width: usize,
    layout: BarLayout,
}

impl BarDisplay {
    pub fn new() -> Self {
        let mp = Arc::new(MultiProgress::new());
        publish(&mp);
        let width = terminal_width();
        Self {
            mp,
            bars: HashMap::new(),
            glyphs: BAR_GLYPHS[0],
            width,
            layout: BarLayout::for_width(width),
        }
    }

    /// Start (or restart) a task's bar.
    ///
    /// TaskCreated is re-emitted on each resume cycle, so an existing bar is
    /// reused rather than orphaned: a bar dropped from the map stays registered
    /// with the MultiProgress and keeps rendering, which is how one task came
    /// to look like two.
    pub fn on_created(&mut self, id: u64, name: &str) {
        let layout = self.layout;
        let entry = self.bars.entry(id).or_insert_with(|| {
            let bar = self.mp.add(ProgressBar::new(0));
            bar.enable_steady_tick(std::time::Duration::from_millis(100));
            Entry {
                bar,
                total: None,
                layout,
                paused: false,
                phase: None,
            }
        });
        entry.bar.set_prefix(format!(" {name}"));
        entry.bar.set_style(bar_style_unknown(entry.layout));
        entry.bar.set_length(0);
        entry.total = None;
        entry.paused = false;
        entry.phase = None;
    }

    /// Record a pre-transfer stage, shown until real progress arrives.
    pub fn on_phase(&mut self, id: u64, phase: &str) {
        if let Some(entry) = self.bars.get_mut(&id) {
            entry.phase = Some(phase.to_string());
            entry.bar.set_message(format!("{phase}…"));
        }
    }

    /// `name` is used only if the bar has to be created here, which happens
    /// when the daemon subscription opened after `TaskCreated` was emitted and
    /// so the name from that event was never seen.
    pub fn on_progress(&mut self, id: u64, name: Option<&str>, view: &ProgressView) {
        // Re-read the width every tick so a resize is picked up live.
        let now = terminal_width();
        if now != self.width {
            self.width = now;
            self.layout = BarLayout::for_width(now);
        }
        let layout = self.layout;

        if !self.bars.contains_key(&id) {
            // The daemon subscribe race can drop TaskCreated; create the bar now
            // rather than rendering nothing at all.
            self.on_created(id, name.unwrap_or_default());
        }
        let glyphs = self.glyphs;
        if let Some(entry) = self.bars.get_mut(&id) {
            Self::apply(entry, view, layout, glyphs);
        }
    }

    fn apply(
        entry: &mut Entry,
        view: &ProgressView,
        layout: BarLayout,
        glyphs: (&'static str, &'static str, &'static str),
    ) {
        let stats = status_text(
            view.connections,
            view.completed_blocks,
            view.total_blocks,
            view.endgame,
        );
        // Before any bytes move there is nothing to report, so show the current
        // stage instead of an empty right-hand side.
        let mut newly_sized = false;
        if entry.total.is_none() {
            if let Some(t) = view.total_bytes.filter(|t| *t > 0) {
                entry.total = Some(t);
                // Without this the bar length stays 0 and indicatif reports 100%
                // at every position.
                entry.bar.set_length(t);
                newly_sized = true;
            }
        }

        let status = if entry.total.is_none() && view.bytes_downloaded == 0 {
            match entry.phase.clone() {
                Some(phase) => format!("{phase}…"),
                None => stats,
            }
        } else {
            stats
        };
        entry.phase = None;

        if entry.paused {
            entry
                .bar
                .enable_steady_tick(std::time::Duration::from_millis(100));
            entry.paused = false;
        }
        entry.bar.set_position(view.bytes_downloaded);
        entry.bar.set_message(status);

        if entry.layout != layout || newly_sized {
            entry.layout = layout;
            entry.bar.set_style(match entry.total {
                Some(_) => bar_style_for(layout, glyphs),
                None => bar_style_unknown(layout),
            });
        }
    }

    /// Clear the bar rather than finishing it: `finish()` forces the position
    /// to the bar length, and a two-line leftover reads as a second bar. The
    /// per-file summary line is printed separately.
    pub fn on_completed(&mut self, id: u64) {
        if let Some(entry) = self.bars.remove(&id) {
            entry.bar.finish_and_clear();
        }
    }

    /// Freeze the bar where it actually stopped. `finish()` would render an
    /// interrupted download as 100%.
    pub fn on_paused(&mut self, id: u64) {
        if let Some(entry) = self.bars.get_mut(&id) {
            entry.bar.set_message("paused".to_string());
            entry.bar.disable_steady_tick();
            entry.paused = true;
        }
    }

    pub fn on_failed(&mut self, id: u64) {
        if let Some(entry) = self.bars.remove(&id) {
            entry.bar.finish_and_clear();
        }
    }

    pub fn finish_all(&mut self) {
        for (_, entry) in self.bars.drain() {
            entry.bar.finish_and_clear();
        }
    }
}

impl Default for BarDisplay {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_picks_layout_tier() {
        assert_eq!(BarLayout::for_width(0), BarLayout::Narrow);
        assert_eq!(BarLayout::for_width(40), BarLayout::Narrow);
        assert_eq!(BarLayout::for_width(59), BarLayout::Narrow);
        assert_eq!(BarLayout::for_width(60), BarLayout::Medium);
        assert_eq!(BarLayout::for_width(95), BarLayout::Medium);
        assert_eq!(BarLayout::for_width(96), BarLayout::Wide);
        assert_eq!(BarLayout::for_width(200), BarLayout::Wide);
    }

    #[test]
    fn status_hides_fields_that_do_not_apply() {
        // Streaming has no block map: blocks are omitted, not shown as 0/0.
        assert_eq!(status_text(1, 0, 0, false), "1 conn");
        assert_eq!(
            status_text(4, 846, 3200, false),
            "846/3200 blocks  ·  4 conns"
        );
        assert_eq!(
            status_text(4, 3199, 3200, true),
            "3199/3200 blocks  ·  4 conns  ·  endgame"
        );
    }

    #[test]
    fn status_pluralises_connections() {
        assert_eq!(status_text(1, 0, 0, false), "1 conn");
        assert_eq!(status_text(2, 0, 0, false), "2 conns");
    }

    #[test]
    fn every_layout_builds_all_glyph_sets() {
        for layout in [BarLayout::Wide, BarLayout::Medium, BarLayout::Narrow] {
            for glyphs in BAR_GLYPHS {
                let _ = bar_style_for(layout, glyphs);
                let _ = bar_style_unknown(layout);
            }
        }
    }

    #[test]
    fn display_handles_progress_without_task_created() {
        // The daemon subscribe race can drop TaskCreated; progress must still
        // produce a bar rather than nothing.
        let mut d = BarDisplay::new();
        d.on_progress(7, Some("late.bin"), &ProgressView::default());
        assert!(d.bars.contains_key(&7));
        assert_eq!(
            d.bars.get(&7).unwrap().bar.prefix().to_string().trim(),
            "late.bin"
        );
    }

    #[test]
    fn recreate_reuses_one_bar_per_task() {
        let mut d = BarDisplay::new();
        d.on_created(1, "a.iso");
        d.on_created(1, "a.iso");
        assert_eq!(d.bars.len(), 1, "re-creating must not orphan a bar");
    }

    #[test]
    fn completed_clears_the_bar() {
        let mut d = BarDisplay::new();
        d.on_created(2, "b.iso");
        d.on_completed(2);
        assert!(d.bars.is_empty());
    }
}
