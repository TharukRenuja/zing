use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::DefaultTerminal;
use zing_core::downloader::{DownloadTask, TaskSnapshot};

use crate::logs::LogBuffer;
use crate::widgets;

pub struct TuiApp {
    task: Arc<DownloadTask>,
    logs: LogBuffer,
    snapshot: Option<TaskSnapshot>,
    should_exit: bool,
    scroll_offset: usize,
    show_logs: bool,
    done_display_frames: u32,
}

impl TuiApp {
    pub fn new(task: Arc<DownloadTask>, logs: LogBuffer) -> Self {
        Self {
            task,
            logs,
            snapshot: None,
            should_exit: false,
            scroll_offset: 0,
            show_logs: true,
            done_display_frames: 0,
        }
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_exit {
            self.snapshot = Some(self.task.snapshot().await);
            let log_lines = self.logs.lines();

            terminal.draw(|frame| {
                if let Some(ref snap) = self.snapshot {
                    widgets::render(
                        frame,
                        frame.area(),
                        snap,
                        &log_lines,
                        self.scroll_offset,
                        self.show_logs,
                    );
                }
            })?;

            let mut resized = false;
            if event::poll(Duration::from_millis(200))? {
                match event::read()? {
                    Event::Key(key) => {
                        if key.kind == KeyEventKind::Press {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => self.should_exit = true,
                                KeyCode::Char('p') | KeyCode::Char(' ') => {
                                    if self.task.is_paused() {
                                        self.task.resume();
                                    } else {
                                        self.task.pause();
                                    }
                                }
                                KeyCode::Char('x') | KeyCode::Char('s') => {
                                    self.task.stop();
                                    self.should_exit = true;
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    self.scroll_offset = self.scroll_offset.saturating_add(1);
                                }
                                KeyCode::Up | KeyCode::Char('k') => {
                                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                                }
                                KeyCode::Char('l') => self.show_logs = !self.show_logs,
                                _ => {}
                            }
                        }
                    }
                    Event::Resize(_, _) => {
                        terminal.autoresize()?;
                        resized = true;
                    }
                    _ => {}
                }
            }

            if let Some(ref snap) = self.snapshot {
                self.scroll_offset = self
                    .scroll_offset
                    .min(snap.connections.len().saturating_sub(1));
                if snap.done && snap.total_bytes > 0 {
                    self.done_display_frames += 1;
                    if self.done_display_frames > 15 {
                        self.should_exit = true;
                    }
                }
            }

            if !resized {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        Ok(())
    }
}
