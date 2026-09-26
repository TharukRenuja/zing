use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use ratatui::DefaultTerminal;
use tokio::sync::broadcast;
use zing_core::downloader::TaskSnapshot;

use crate::logs::LogBuffer;
use crate::task::{TaskControl, TaskUiStatus};
use crate::{widgets, TaskFactory};

/// One row in the TUI: a task plus its latest snapshot.
pub struct Entry {
    pub control: Arc<dyn TaskControl>,
    pub snapshot: Option<TaskSnapshot>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Entry {
    /// Display label: the resolved filename once known, otherwise a placeholder.
    pub fn label(&self) -> &str {
        match &self.snapshot {
            Some(s) if !s.filename.is_empty() => &s.filename,
            _ => "…",
        }
    }

    /// True while the task is waiting for a concurrency permit.
    pub fn queued(&self) -> bool {
        self.control.ui_status() == TaskUiStatus::Queued
    }

    pub fn running(&self) -> bool {
        self.control.ui_status() == TaskUiStatus::Downloading
    }

    pub fn status(&self) -> &'static str {
        match self.control.ui_status() {
            TaskUiStatus::Queued => "queued",
            TaskUiStatus::Downloading => "downloading",
            TaskUiStatus::Paused => "paused",
            TaskUiStatus::Done => "done",
            TaskUiStatus::Failed => "failed",
            TaskUiStatus::Stopped => "stopped",
        }
    }

    pub fn progress(&self) -> f64 {
        match &self.snapshot {
            Some(s) if s.total_bytes > 0 => {
                (s.bytes_downloaded as f64 / s.total_bytes as f64 * 100.0).clamp(0.0, 100.0)
            }
            Some(s) if s.done => 100.0,
            _ => 0.0,
        }
    }
}

/// How long to wait for tasks to finish after a shutdown request before
/// aborting them. Keeps `q` responsive even if a task is stuck.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Reject accidental prompt submissions: a bare single key is a mistyped
/// keypress, not a URL (`q` typed while trying to quit, for example).
fn is_bare_single_key(url: &str) -> bool {
    url.len() == 1 && url.chars().next().is_some_and(|c| !c.is_ascii_digit())
}

enum InputMode {
    None,
    AddUrl { buffer: String },
}

pub struct TuiApp {
    entries: Vec<Entry>,
    selected: usize,
    logs: LogBuffer,
    should_exit: bool,
    done_frames: u32,
    input: InputMode,
    pending_add: Option<String>,
    factory: Option<TaskFactory>,
    sem: Option<Arc<tokio::sync::Semaphore>>,
    shutdown_tx: broadcast::Sender<()>,
}

impl TuiApp {
    pub fn new(opts: crate::TuiOptions) -> Self {
        let (shutdown_tx, _shutdown_rx) = broadcast::channel::<()>(64);
        let sem = match opts.max_concurrent {
            0 => None,
            n => Some(Arc::new(tokio::sync::Semaphore::new(n))),
        };
        let mut app = Self {
            entries: Vec::new(),
            selected: 0,
            logs: opts.logs,
            should_exit: false,
            done_frames: 0,
            input: InputMode::None,
            pending_add: None,
            factory: opts.factory,
            sem,
            shutdown_tx,
        };
        for task in opts.tasks {
            app.spawn_entry(task);
        }
        app
    }

    fn spawn_entry(&mut self, task: Arc<dyn TaskControl>) {
        let sem = self.sem.clone();
        let rx = self.shutdown_tx.subscribe();
        let handle = task.start(rx, sem);
        self.entries.push(Entry {
            control: task,
            snapshot: None,
            handle,
        });
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
        let result = self.run_inner(terminal).await;
        crossterm::execute!(std::io::stdout(), DisableBracketedPaste)?;
        result
    }

    async fn run_inner(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_exit {
            self.refresh().await;

            terminal.draw(|frame| {
                let log_lines = self.logs.lines();
                widgets::render_unified(
                    frame,
                    frame.area(),
                    &self.entries,
                    self.selected,
                    &log_lines,
                    self.input_string(),
                );
            })?;

            let mut resized = false;
            let poll_timeout = if self.is_input_mode() {
                Duration::ZERO
            } else {
                Duration::from_millis(200)
            };
            if event::poll(poll_timeout)? {
                match event::read()? {
                    Event::Key(key) => {
                        // Accept auto-repeat so holding a key still acts on
                        // terminals that only emit repeat events for it.
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            self.handle_key(key.code, key.modifiers);
                        }
                    }
                    Event::Paste(text) => {
                        if let InputMode::AddUrl { buffer } = &mut self.input {
                            buffer.push_str(&text);
                        }
                    }
                    Event::Resize(_, _) => {
                        terminal.autoresize()?;
                        resized = true;
                    }
                    _ => {}
                }
            }

            if self.all_done() {
                self.done_frames += 1;
                if self.done_frames > 30 {
                    self.should_exit = true;
                }
            }

            if !resized {
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
        }
        self.shutdown_tasks().await;
        Ok(())
    }

    /// Stop still-running tasks so `.zing` control files persist.
    ///
    /// Bounded by [`SHUTDOWN_TIMEOUT`] and followed by an abort so a wedged or
    /// paused task can never leave the terminal stuck in raw/alternate-screen
    /// mode after the user presses `q`.
    async fn shutdown_tasks(&mut self) {
        let _ = self.shutdown_tx.send(());
        for entry in &mut self.entries {
            if let Some(mut h) = entry.handle.take() {
                match tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut h).await {
                    Ok(_) => {}
                    Err(_) => {
                        tracing::warn!("Task did not exit within shutdown timeout, aborting");
                        h.abort();
                    }
                }
            }
        }
    }

    async fn refresh(&mut self) {
        if let Some(url) = self.pending_add.take() {
            self.add_url(url).await;
        }
        if !self.is_input_mode() {
            for entry in &mut self.entries {
                entry.snapshot = Some(entry.control.snapshot().await);
            }
        }
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    fn all_done(&self) -> bool {
        !self.entries.is_empty()
            && self
                .entries
                .iter()
                .all(|e| e.control.ui_status() == TaskUiStatus::Done)
    }

    fn input_string(&self) -> Option<&str> {
        match &self.input {
            InputMode::AddUrl { buffer } => Some(buffer.as_str()),
            _ => None,
        }
    }

    fn is_input_mode(&self) -> bool {
        matches!(self.input, InputMode::AddUrl { .. })
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if let InputMode::AddUrl { buffer } = &mut self.input {
            match code {
                // `q` types a character here (URLs legitimately contain it), so
                // quitting from the prompt needs an unambiguous key. Ctrl+C is
                // matched first because crossterm reports it as `Char('c')`.
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input = InputMode::None
                }
                KeyCode::Char(c) => buffer.push(c),
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Esc => self.input = InputMode::None,
                KeyCode::Enter => {
                    let url = buffer.trim().to_string();
                    self.input = InputMode::None;
                    if !url.is_empty() && !is_bare_single_key(&url) {
                        self.pending_add = Some(url);
                    } else if !url.is_empty() {
                        self.logs.push(format!("Not a valid URL: {url}"));
                    }
                }
                _ => {}
            }
            return;
        }

        // Ctrl+C is checked first so it quits even with a modifier held on a
        // terminal that reports Ctrl+C as a plain `Char('c')` press.
        match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_exit = true
            }
            _ => {}
        }
        if self.should_exit {
            return;
        }

        match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => self.should_exit = true,
            KeyCode::Esc => self.should_exit = true,
            KeyCode::Char('a') if self.factory.is_some() => {
                self.input = InputMode::AddUrl {
                    buffer: String::new(),
                };
            }
            KeyCode::Char('p') | KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Char('P') => self.toggle_pause_all(),
            KeyCode::Char('x') | KeyCode::Char('s') => self.stop_selected(),
            KeyCode::Char('r') => self.remove_selected(),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let len = self.entries.len() as isize;
        let next = (self.selected as isize + delta).clamp(0, len - 1) as usize;
        self.selected = next;
    }

    fn toggle_pause(&self) {
        if let Some(e) = self.entries.get(self.selected) {
            if e.control.is_paused() {
                e.control.resume();
            } else {
                e.control.pause();
            }
        }
    }

    fn toggle_pause_all(&self) {
        // Only consider active tasks (downloading/paused/queued) for the toggle.
        // Done/failed/stopped tasks should not affect the decision.
        let any_active_not_paused = self.entries.iter().any(|e| {
            matches!(e.status(), "downloading" | "paused" | "queued") && !e.control.is_paused()
        });
        for e in &self.entries {
            if any_active_not_paused {
                e.control.pause();
            } else {
                e.control.resume();
            }
        }
    }

    fn stop_selected(&self) {
        if let Some(e) = self.entries.get(self.selected) {
            e.control.stop();
        }
    }

    fn remove_selected(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        if let Some(e) = self.entries.get_mut(self.selected) {
            e.control.remove();
            if let Some(h) = e.handle.take() {
                h.abort();
            }
        }
        self.entries.remove(self.selected);
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    async fn add_url(&mut self, url: String) {
        let Some(factory) = self.factory.clone() else {
            return;
        };
        match factory(&url).await {
            Ok(task) => self.spawn_entry(task),
            Err(e) => tracing::error!("Cannot add {url}: {e}"),
        }
    }
}
