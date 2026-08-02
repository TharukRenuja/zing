use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    List,
    Detail,
}

enum InputMode {
    None,
    AddUrl { buffer: String },
}

pub struct TuiApp {
    entries: Vec<Entry>,
    selected: usize,
    view: View,
    logs: LogBuffer,
    should_exit: bool,
    scroll_offset: usize,
    show_logs: bool,
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
            view: View::List,
            logs: opts.logs,
            should_exit: false,
            scroll_offset: 0,
            show_logs: true,
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
        while !self.should_exit {
            self.refresh().await;
            let log_lines = self.logs.lines();

            terminal.draw(|frame| match self.view {
                View::List => widgets::render_list(
                    frame,
                    frame.area(),
                    &self.entries,
                    self.selected,
                    &log_lines,
                    self.show_logs,
                    self.input_string(),
                ),
                View::Detail => {
                    if let Some(snap) = self.selected_snapshot() {
                        widgets::render_detail(
                            frame,
                            frame.area(),
                            snap,
                            &log_lines,
                            self.scroll_offset,
                            self.show_logs,
                        );
                    }
                }
            })?;

            let mut resized = false;
            if event::poll(Duration::from_millis(200))? {
                match event::read()? {
                    Event::Key(key) => {
                        if key.kind == KeyEventKind::Press {
                            self.handle_key(key.code, key.modifiers);
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
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        // Gracefully stop still-running tasks so `.zing` control files persist.
        let _ = self.shutdown_tx.send(());
        for entry in &mut self.entries {
            if let Some(h) = entry.handle.take() {
                let _ = h.await;
            }
        }
        Ok(())
    }

    async fn refresh(&mut self) {
        if let Some(url) = self.pending_add.take() {
            self.add_url(url).await;
        }
        for entry in &mut self.entries {
            entry.snapshot = Some(entry.control.snapshot().await);
        }
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
    }

    fn selected_snapshot(&self) -> Option<&TaskSnapshot> {
        self.entries
            .get(self.selected)
            .and_then(|e| e.snapshot.as_ref())
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

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if let InputMode::AddUrl { buffer } = &mut self.input {
            match code {
                KeyCode::Char(c) => buffer.push(c),
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Esc => self.input = InputMode::None,
                KeyCode::Enter => {
                    let url = buffer.trim().to_string();
                    self.input = InputMode::None;
                    if !url.is_empty() {
                        self.pending_add = Some(url);
                    }
                }
                _ => {}
            }
            return;
        }

        match code {
            KeyCode::Char('q') => self.should_exit = true,
            KeyCode::Esc => {
                if self.view == View::Detail {
                    self.view = View::List;
                } else {
                    self.should_exit = true;
                }
            }
            KeyCode::Char('a') if self.factory.is_some() => {
                self.input = InputMode::AddUrl {
                    buffer: String::new(),
                };
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.view = if self.view == View::List {
                    View::Detail
                } else {
                    View::List
                };
            }
            KeyCode::Char('p') | KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Char('x') | KeyCode::Char('s') => self.stop_selected(),
            KeyCode::Char('r') => self.remove_selected(),
            KeyCode::Char('l') => self.show_logs = !self.show_logs,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_exit = true
            }
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
