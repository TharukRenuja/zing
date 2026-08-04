//! IDM-style main window: toolbar, sidebar filters, task table, detail panel.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Button, Color32, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::client::{GuiClient, TaskInfo};

const SIDEBAR_WIDTH: f32 = 190.0;
const DETAIL_HEIGHT: f32 = 230.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Downloading,
    Paused,
    Queued,
    Completed,
    Failed,
    Stopped,
}

impl Filter {
    fn label(&self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Downloading => "Downloading",
            Filter::Paused => "Paused",
            Filter::Queued => "Queued",
            Filter::Completed => "Completed",
            Filter::Failed => "Failed",
            Filter::Stopped => "Stopped",
        }
    }
}

pub struct ZingApp {
    client: GuiClient,
    tasks: Vec<TaskInfo>,
    snapshot: Arc<Mutex<Vec<TaskInfo>>>,
    selected: Option<u64>,
    filter: Filter,
    show_add_dialog: bool,
    new_url: String,
    error: Option<String>,
    speed_history: VecDeque<(f64, f64)>,
    started: Instant,
    version: Option<String>,
}

impl ZingApp {
    pub fn new(client: GuiClient) -> Self {
        let snapshot = Arc::new(Mutex::new(Vec::new()));
        client.spawn_poller(Arc::clone(&snapshot));

        Self {
            version: client.version().ok(),
            client,
            tasks: Vec::new(),
            snapshot,
            selected: None,
            filter: Filter::All,
            show_add_dialog: false,
            new_url: String::new(),
            error: None,
            speed_history: VecDeque::new(),
            started: Instant::now(),
        }
    }

    fn refresh(&mut self) {
        if let Ok(s) = self.snapshot.lock() {
            self.tasks = s.clone();
        }
        let now = self.started.elapsed().as_secs_f64();
        let total_speed: u64 = self.tasks.iter().map(|t| t.speed).sum();
        self.speed_history.push_back((now, total_speed as f64));
        if self.speed_history.len() > 240 {
            self.speed_history.pop_front();
        }
    }

    fn filtered(&self) -> Vec<&TaskInfo> {
        self.tasks
            .iter()
            .filter(|t| match self.filter {
                Filter::All => true,
                Filter::Downloading => !t.done && !t.paused,
                Filter::Paused => t.paused,
                Filter::Queued => t.total_bytes == 0 && !t.done && !t.paused,
                Filter::Completed => t.status == "Completed",
                Filter::Failed => t.status.starts_with("Failed"),
                Filter::Stopped => t.status == "Stopped",
            })
            .collect()
    }

    fn selected_task(&self) -> Option<&TaskInfo> {
        self.selected
            .and_then(|id| self.tasks.iter().find(|t| t.id == id))
    }
}

impl eframe::App for ZingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh();

        if self.show_add_dialog {
            let mut open = true;
            egui::Window::new("Add URL")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 120.0])
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("URL:");
                        let response = ui.text_edit_singleline(&mut self.new_url);
                        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            self.add_url();
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Add").clicked() {
                            self.add_url();
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_add_dialog = false;
                        }
                    });
                });
            if !open {
                self.show_add_dialog = false;
            }
        }

        if let Some(err) = &self.error {
            let err = err.clone();
            let mut open = true;
            egui::Window::new("Error")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 120.0])
                .show(ctx, |ui| {
                    ui.colored_label(Color32::RED, err);
                    ui.add_space(6.0);
                    if ui.button("OK").clicked() {
                        self.error = None;
                    }
                });
            if !open {
                self.error = None;
            }
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("+ Add URL").clicked() {
                    self.show_add_dialog = true;
                }
                ui.separator();
                if ui
                    .add_enabled(self.selected_task().is_some(), Button::new("▶ Resume"))
                    .on_hover_text("Resume selected task")
                    .clicked()
                {
                    if let Some(id) = self.selected {
                        let _ = self.client.resume(id);
                    }
                }
                if ui
                    .add_enabled(self.selected_task().is_some(), Button::new("⏸ Pause"))
                    .on_hover_text("Pause selected task")
                    .clicked()
                {
                    if let Some(id) = self.selected {
                        let _ = self.client.pause(id);
                    }
                }
                if ui
                    .add_enabled(self.selected_task().is_some(), Button::new("■ Stop"))
                    .on_hover_text("Stop selected task")
                    .clicked()
                {
                    if let Some(id) = self.selected {
                        let _ = self.client.stop(id);
                    }
                }
                if ui
                    .add_enabled(self.selected_task().is_some(), Button::new("🗑 Remove"))
                    .on_hover_text("Remove selected task from the list")
                    .clicked()
                {
                    if let Some(id) = self.selected {
                        match self.client.remove(id) {
                            Ok(()) => self.selected = None,
                            Err(e) => self.error = Some(e),
                        }
                    }
                }
                ui.separator();
                let total: u64 = self.tasks.iter().map(|t| t.speed).sum();
                ui.label(
                    RichText::new(format!("{:.1} MB/s", total as f64 / 1_048_576.0))
                        .strong()
                        .color(Color32::LIGHT_GREEN),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(
                            self.version
                                .clone()
                                .map(|v| format!("zing {v}"))
                                .unwrap_or_else(|| "zing".to_string()),
                        )
                        .weak(),
                    );
                });
            });
        });

        egui::SidePanel::left("sidebar")
            .exact_width(SIDEBAR_WIDTH)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                let filters = [
                    (Filter::All, self.tasks.len()),
                    (
                        Filter::Downloading,
                        self.tasks.iter().filter(|t| !t.done && !t.paused).count(),
                    ),
                    (
                        Filter::Paused,
                        self.tasks.iter().filter(|t| t.paused).count(),
                    ),
                    (
                        Filter::Queued,
                        self.tasks
                            .iter()
                            .filter(|t| t.total_bytes == 0 && !t.done && !t.paused)
                            .count(),
                    ),
                    (
                        Filter::Completed,
                        self.tasks
                            .iter()
                            .filter(|t| t.status == "Completed")
                            .count(),
                    ),
                    (
                        Filter::Failed,
                        self.tasks
                            .iter()
                            .filter(|t| t.status.starts_with("Failed"))
                            .count(),
                    ),
                    (
                        Filter::Stopped,
                        self.tasks.iter().filter(|t| t.status == "Stopped").count(),
                    ),
                ];
                for (filter, count) in filters {
                    let label = format!("{}  ({count})", filter.label());
                    if ui.selectable_label(self.filter == filter, label).clicked() {
                        self.filter = filter;
                    }
                }
            });

        egui::TopBottomPanel::bottom("detail")
            .resizable(true)
            .default_height(DETAIL_HEIGHT)
            .show(ctx, |ui| {
                self.detail_panel(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.task_table(ui);
        });
    }
}

impl ZingApp {
    fn add_url(&mut self) {
        let url = self.new_url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let params = serde_json::json!({ "url": url });
        match self.client.add_uri(params) {
            Ok(_) => {
                self.new_url.clear();
                self.show_add_dialog = false;
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn task_table(&mut self, ui: &mut egui::Ui) {
        let rows: Vec<u64> = self.filtered().iter().map(|t| t.id).collect();
        let tasks = &self.tasks;
        let selected = self.selected;

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(280.0).resizable(true))
            .column(Column::initial(90.0).resizable(true))
            .column(Column::initial(80.0).resizable(true))
            .column(Column::initial(90.0).resizable(true))
            .column(Column::remainder().at_least(80.0))
            .header(26.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Name");
                });
                header.col(|ui| {
                    ui.strong("Size");
                });
                header.col(|ui| {
                    ui.strong("Speed");
                });
                header.col(|ui| {
                    ui.strong("Status");
                });
                header.col(|ui| {
                    ui.strong("Progress");
                });
            })
            .body(|mut body| {
                for id in rows {
                    let Some(task) = tasks.iter().find(|t| t.id == id) else {
                        continue;
                    };
                    body.row(26.0, |mut row| {
                        row.set_selected(selected == Some(id));
                        if row.response().clicked() {
                            self.selected = Some(id);
                        }
                        row.col(|ui| {
                            ui.label(RichText::new(&task.filename).color(Color32::WHITE));
                        });
                        row.col(|ui| {
                            ui.label(format_bytes(task.total_bytes));
                        });
                        row.col(|ui| {
                            if task.paused {
                                ui.label(RichText::new("paused").weak());
                            } else if task.done {
                                ui.label(RichText::new("–").weak());
                            } else {
                                ui.label(format_speed(task.speed));
                            }
                        });
                        row.col(|ui| {
                            ui.label(status_color(&task.status, &task.error));
                        });
                        row.col(|ui| {
                            let p = task.progress_fraction();
                            let pb = egui::ProgressBar::new(p)
                                .show_percentage()
                                .desired_width(ui.available_width());
                            ui.add(pb);
                        });
                    });
                }
            });
    }

    fn detail_panel(&mut self, ui: &mut egui::Ui) {
        let Some(task) = self.selected_task().cloned() else {
            ui.weak("No task selected");
            return;
        };

        ui.columns(3, |cols| {
            cols[0].group(|ui| {
                ui.set_min_width(0.0);
                ui.label(RichText::new("Info").strong());
                ui.separator();
                ui.label(format!("File: {}", task.filename));
                ui.label(format!("URL: {}", task.url));
                ui.label(format!("Size: {}", format_bytes(task.total_bytes)));
                ui.label(format!("Downloaded: {}", format_bytes(task.downloaded)));
                ui.label(format!("Speed: {}", format_speed(task.speed)));
                ui.label(format!("Peak: {}", format_speed(task.peak_speed)));
                ui.label(format!("Connections: {}", task.connections));
            });
            cols[1].group(|ui| {
                ui.set_min_width(0.0);
                ui.label(RichText::new("Speed").strong());
                ui.separator();
                if self.speed_history.len() < 2 {
                    ui.weak("Collecting data…");
                } else {
                    let points: Vec<[f64; 2]> =
                        self.speed_history.iter().map(|&(x, y)| [x, y]).collect();
                    Plot::new("speed_plot")
                        .height(140.0)
                        .allow_drag(false)
                        .allow_zoom(false)
                        .allow_scroll(false)
                        .legend(Legend::default())
                        .show(ui, |plot_ui| {
                            plot_ui.line(
                                Line::new(PlotPoints::from(points)).color(Color32::LIGHT_BLUE),
                            );
                        });
                }
            });
            cols[2].group(|ui| {
                ui.set_min_width(0.0);
                ui.label(RichText::new("Blocks").strong());
                ui.separator();
                draw_block_grid(ui, task.completed_blocks, task.total_blocks);
            });
        });
    }
}

fn draw_block_grid(ui: &mut egui::Ui, completed: u32, total: u32) {
    if total == 0 {
        ui.weak("No block data yet");
        return;
    }
    let side = 8.0;
    let spacing = 2.0;
    let per_row = ((ui.available_width() - spacing) / (side + spacing)) as u32;
    let per_row = per_row.max(1);
    let rows = total.div_ceil(per_row).max(1);
    let height = rows as f32 * (side + spacing);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    let mut completed_left = completed;
    for r in 0..rows {
        for c in 0..per_row {
            let index = r * per_row + c;
            if index >= total {
                break;
            }
            let x = rect.left() + c as f32 * (side + spacing);
            let y = rect.top() + r as f32 * (side + spacing);
            let color = if completed_left > 0 {
                completed_left -= 1;
                Color32::from_rgb(52, 168, 83)
            } else {
                Color32::from_rgb(60, 62, 68)
            };
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(side, side)),
                1.0,
                color,
            );
        }
    }
}

fn status_color(status: &str, error: &Option<String>) -> RichText {
    let text = error.as_deref().map_or(status, |e| &e[..e.len().min(24)]);
    let color = match status {
        "Completed" => Color32::from_rgb(52, 168, 83),
        s if s.starts_with("Failed") => Color32::RED,
        "Paused" => Color32::from_rgb(242, 153, 74),
        "Stopped" => Color32::from_rgb(158, 158, 158),
        _ => Color32::LIGHT_BLUE,
    };
    RichText::new(text).color(color)
}

fn format_bytes(n: u64) -> String {
    if n == 0 {
        return "–".to_string();
    }
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", units[i])
}

fn format_speed(s: u64) -> String {
    format!("{}/s", format_bytes(s))
}
