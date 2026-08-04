//! IDM-style main window.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Button, Color32, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::client::{GuiClient, TaskInfo};

// ── Layout constants ──────────────────────────────────────────────

const CATEGORY_WIDTH: f32 = 180.0;
const DETAIL_HEIGHT: f32 = 200.0;
const TOOLBAR_HEIGHT: f32 = 36.0;

// ── Category filter ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    All,
    Downloading,
    Complete,
    Paused,
    Queued,
    Failed,
    Stopped,
}

impl Category {
    const ALL: &'static [Category] = &[
        Category::All,
        Category::Downloading,
        Category::Complete,
        Category::Paused,
        Category::Queued,
        Category::Failed,
        Category::Stopped,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All Downloads",
            Self::Downloading => "Downloading",
            Self::Complete => "Complete",
            Self::Paused => "Paused",
            Self::Queued => "Queued",
            Self::Failed => "Failed",
            Self::Stopped => "Stopped",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::All => "📁",
            Self::Downloading => "⬇",
            Self::Complete => "✅",
            Self::Paused => "⏸",
            Self::Queued => "⏳",
            Self::Failed => "❌",
            Self::Stopped => "⏹",
        }
    }

    fn matches(self, t: &TaskInfo) -> bool {
        match self {
            Self::All => true,
            Self::Downloading => !t.done && !t.paused && t.total_bytes > 0,
            Self::Complete => t.status == "Completed",
            Self::Paused => t.paused,
            Self::Queued => t.total_bytes == 0 && !t.done && !t.paused,
            Self::Failed => t.status.starts_with("Failed"),
            Self::Stopped => t.status == "Stopped",
        }
    }
}

// ── Detail panel tabs ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    General,
    Speed,
    Blocks,
}

// ── App state ─────────────────────────────────────────────────────

pub struct ZingApp {
    client: GuiClient,
    tasks: Vec<TaskInfo>,
    snapshot: Arc<Mutex<Vec<TaskInfo>>>,
    selected: Option<u64>,
    category: Category,
    detail_tab: DetailTab,
    show_add_dialog: bool,
    new_url: String,
    error: Option<String>,
    speed_history: VecDeque<(f64, f64)>,
    started: Instant,
    version: String,
}

impl ZingApp {
    pub fn new(client: GuiClient) -> Self {
        let snapshot = Arc::new(Mutex::new(Vec::new()));
        client.spawn_poller(Arc::clone(&snapshot));

        let version = client.version().unwrap_or_else(|_| "dev".into());

        Self {
            client,
            tasks: Vec::new(),
            snapshot,
            selected: None,
            category: Category::All,
            detail_tab: DetailTab::General,
            show_add_dialog: false,
            new_url: String::new(),
            error: None,
            speed_history: VecDeque::new(),
            started: Instant::now(),
            version,
        }
    }

    fn refresh(&mut self) {
        if let Ok(s) = self.snapshot.lock() {
            self.tasks = s.clone();
        }
        let now = self.started.elapsed().as_secs_f64();
        let total: u64 = self.tasks.iter().map(|t| t.speed).sum();
        self.speed_history.push_back((now, total as f64));
        if self.speed_history.len() > 300 {
            self.speed_history.pop_front();
        }
    }

    fn filtered(&self) -> Vec<&TaskInfo> {
        self.tasks
            .iter()
            .filter(|t| self.category.matches(t))
            .collect()
    }

    fn selected_task(&self) -> Option<&TaskInfo> {
        self.selected
            .and_then(|id| self.tasks.iter().find(|t| t.id == id))
    }

    fn category_count(&self, cat: Category) -> usize {
        self.tasks.iter().filter(|t| cat.matches(t)).count()
    }
}

// ── eframe::App ───────────────────────────────────────────────────

impl eframe::App for ZingApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh();

        // ── Add URL dialog ────────────────────────────────────
        if self.show_add_dialog {
            self.render_add_dialog(ctx);
        }
        if let Some(err) = &self.error {
            let err = err.clone();
            let mut open = true;
            egui::Window::new("Error")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.colored_label(Color32::RED, &err);
                    if ui.button("OK").clicked() {
                        self.error = None;
                    }
                });
            if !open {
                self.error = None;
            }
        }

        // ── Menu bar ─────────────────────────────────────────
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Add URL…").clicked() {
                        self.show_add_dialog = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Exit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Downloads", |ui| {
                    let sel = self.selected;
                    ui.add_enabled_ui(sel.is_some(), |ui| {
                        if ui.button("Resume").clicked() {
                            if let Some(id) = sel {
                                let _ = self.client.resume(id);
                            }
                            ui.close_menu();
                        }
                        if ui.button("Pause").clicked() {
                            if let Some(id) = sel {
                                let _ = self.client.pause(id);
                            }
                            ui.close_menu();
                        }
                        if ui.button("Stop").clicked() {
                            if let Some(id) = sel {
                                let _ = self.client.stop(id);
                            }
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Remove").clicked() {
                            if let Some(id) = sel {
                                let _ = self.client.remove(id);
                                self.selected = None;
                            }
                            ui.close_menu();
                        }
                    });
                });
                ui.menu_button("Help", |ui| {
                    ui.label(format!("zing-gui {}", self.version));
                    ui.label("Desktop download manager");
                });
            });
        });

        // ── Toolbar ──────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar")
            .exact_height(TOOLBAR_HEIGHT)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(2.0, 0.0);

                    if ui
                        .add(Button::new(RichText::new("➕ Add URL").strong()))
                        .on_hover_text("Add a new download URL")
                        .clicked()
                    {
                        self.show_add_dialog = true;
                    }
                    ui.separator();

                    let has_sel = self.selected.is_some();
                    if ui
                        .add_enabled(has_sel, Button::new("▶ Resume"))
                        .on_hover_text("Resume selected")
                        .clicked()
                    {
                        if let Some(id) = self.selected {
                            let _ = self.client.resume(id);
                        }
                    }
                    if ui
                        .add_enabled(has_sel, Button::new("⏸ Pause"))
                        .on_hover_text("Pause selected")
                        .clicked()
                    {
                        if let Some(id) = self.selected {
                            let _ = self.client.pause(id);
                        }
                    }
                    if ui
                        .add_enabled(has_sel, Button::new("⏹ Stop"))
                        .on_hover_text("Stop selected")
                        .clicked()
                    {
                        if let Some(id) = self.selected {
                            let _ = self.client.stop(id);
                        }
                    }
                    if ui
                        .add_enabled(has_sel, Button::new("🗑 Delete"))
                        .on_hover_text("Remove selected from list")
                        .clicked()
                    {
                        if let Some(id) = self.selected {
                            let _ = self.client.remove(id);
                            self.selected = None;
                        }
                    }

                    ui.separator();

                    // Status bar on the right
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let total: u64 = self.tasks.iter().map(|t| t.speed).sum();
                        let dl = self.tasks.iter().filter(|t| !t.done && !t.paused).count();
                        ui.label(
                            RichText::new(format!(
                                "zing {} │ {} active │ {}",
                                self.version,
                                dl,
                                format_speed(total),
                            ))
                            .weak()
                            .small(),
                        );
                    });
                });
            });

        // ── Category sidebar ─────────────────────────────────
        egui::SidePanel::left("categories")
            .exact_width(CATEGORY_WIDTH)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("Categories").strong().small());
                ui.add_space(2.0);

                for &cat in Category::ALL {
                    let count = self.category_count(cat);
                    let label = format!("{}  {} ({})", cat.icon(), cat.label(), count);
                    if ui.selectable_label(self.category == cat, label).clicked() {
                        self.category = cat;
                    }
                }

                // Task count summary at bottom
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(4.0);
                    ui.separator();
                    let total = self.tasks.len();
                    let total_bytes: u64 = self.tasks.iter().map(|t| t.downloaded).sum();
                    let size_bytes: u64 = self.tasks.iter().map(|t| t.total_bytes).sum();
                    ui.label(
                        RichText::new(format!(
                            "{} tasks │ {} / {}",
                            total,
                            format_bytes(total_bytes),
                            format_bytes(size_bytes),
                        ))
                        .weak()
                        .small(),
                    );
                });
            });

        // ── Bottom detail panel ──────────────────────────────
        egui::TopBottomPanel::bottom("detail")
            .resizable(true)
            .default_height(DETAIL_HEIGHT)
            .show(ctx, |ui| {
                self.render_detail_panel(ui);
            });

        // ── Main task table ──────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_task_table(ui);
        });
    }
}

// ── Renderers ─────────────────────────────────────────────────────

impl ZingApp {
    fn render_add_dialog(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("Add URL")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("URL:");
                    let resp = ui.text_edit_singleline(&mut self.new_url);
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.submit_url();
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Add").clicked() {
                        self.submit_url();
                    }
                    if ui.button("Cancel").clicked() {
                        self.show_add_dialog = false;
                        self.new_url.clear();
                    }
                });
            });
        if !open {
            self.show_add_dialog = false;
            self.new_url.clear();
        }
    }

    fn submit_url(&mut self) {
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
            Err(e) => {
                self.error = Some(e);
                self.show_add_dialog = false;
            }
        }
    }

    fn render_task_table(&mut self, ui: &mut Ui) {
        let rows: Vec<u64> = self.filtered().iter().map(|t| t.id).collect();
        let tasks = &self.tasks;
        let selected = self.selected;

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(260.0).resizable(true)) // Name
            .column(Column::initial(85.0).resizable(true)) // Size
            .column(Column::initial(75.0).resizable(true)) // Status
            .column(Column::initial(90.0).resizable(true)) // Speed
            .column(Column::initial(80.0).resizable(true)) // Time Left
            .column(Column::initial(50.0).resizable(false)) // Conns
            .column(Column::remainder().at_least(100.0)) // Progress
            .header(24.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Name");
                });
                header.col(|ui| {
                    ui.strong("Size");
                });
                header.col(|ui| {
                    ui.strong("Status");
                });
                header.col(|ui| {
                    ui.strong("Speed");
                });
                header.col(|ui| {
                    ui.strong("Time Left");
                });
                header.col(|ui| {
                    ui.strong("Conns");
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
                    body.row(22.0, |mut row| {
                        row.set_selected(selected == Some(id));
                        if row.response().clicked() {
                            self.selected = Some(id);
                        }
                        row.col(|ui| {
                            ui.label(RichText::new(&task.filename).strong());
                        });
                        row.col(|ui| {
                            ui.label(format_bytes(task.total_bytes));
                        });
                        row.col(|ui| {
                            ui.label(status_text(task));
                        });
                        row.col(|ui| {
                            if task.paused || task.done || task.speed == 0 {
                                ui.label(RichText::new("—").weak());
                            } else {
                                ui.label(format_speed(task.speed));
                            }
                        });
                        row.col(|ui| {
                            ui.label(eta_text(task));
                        });
                        row.col(|ui| {
                            ui.label(task.connections.to_string());
                        });
                        row.col(|ui| {
                            let p = task.progress_fraction();
                            let bar = egui::ProgressBar::new(p)
                                .show_percentage()
                                .desired_width(ui.available_width());
                            ui.add(bar);
                        });
                    });
                }
            });
    }

    fn render_detail_panel(&mut self, ui: &mut Ui) {
        // Tab bar
        ui.horizontal(|ui| {
            for &(tab, label) in &[
                (DetailTab::General, "General"),
                (DetailTab::Speed, "Speed"),
                (DetailTab::Blocks, "Blocks"),
            ] {
                let selected = self.detail_tab == tab;
                if ui
                    .selectable_label(selected, RichText::new(label).strong())
                    .clicked()
                {
                    self.detail_tab = tab;
                }
            }
        });
        ui.separator();

        let task = self.selected_task().cloned();

        match self.detail_tab {
            DetailTab::General => self.render_general_tab(ui, task.as_ref()),
            DetailTab::Speed => self.render_speed_tab(ui),
            DetailTab::Blocks => self.render_blocks_tab(ui, task.as_ref()),
        }
    }

    fn render_general_tab(&self, ui: &mut Ui, task: Option<&TaskInfo>) {
        let Some(t) = task else {
            ui.weak("Select a task to view details");
            return;
        };

        ui.columns(2, |cols| {
            // Left column: file info
            cols[0].group(|ui| {
                ui.set_min_width(0.0);
                ui.label(RichText::new("File").strong().small());
                ui.separator();
                info_row(ui, "Name:", &t.filename);
                info_row(ui, "URL:", &t.url);
                info_row(ui, "Size:", &format_bytes(t.total_bytes));
                info_row(ui, "Downloaded:", &format_bytes(t.downloaded));
                info_row(ui, "Status:", &t.status);
                if let Some(err) = &t.error {
                    info_row(ui, "Error:", err);
                }
            });

            // Right column: transfer info
            cols[1].group(|ui| {
                ui.set_min_width(0.0);
                ui.label(RichText::new("Transfer").strong().small());
                ui.separator();
                info_row(ui, "Speed:", &format_speed(t.speed));
                info_row(ui, "Peak:", &format_speed(t.peak_speed));
                info_row(ui, "Connections:", &t.connections.to_string());
                info_row(
                    ui,
                    "Blocks:",
                    &format!("{}/{}", t.completed_blocks, t.total_blocks),
                );
                info_row(ui, "ETA:", &eta_text(t));

                ui.add_space(4.0);
                let p = t.progress_fraction();
                let bar = egui::ProgressBar::new(p)
                    .show_percentage()
                    .desired_width(ui.available_width());
                ui.add(bar);
            });
        });
    }

    fn render_speed_tab(&self, ui: &mut Ui) {
        if self.speed_history.len() < 2 {
            ui.weak("Collecting speed data…");
            return;
        }
        let points: Vec<[f64; 2]> = self.speed_history.iter().map(|&(x, y)| [x, y]).collect();
        Plot::new("speed_plot")
            .height(ui.available_height() - 8.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .legend(Legend::default())
            .show(ui, |plot_ui| {
                plot_ui.line(
                    Line::new(PlotPoints::from(points))
                        .color(Color32::from_rgb(80, 160, 255))
                        .name("Speed"),
                );
            });
    }

    fn render_blocks_tab(&self, ui: &mut Ui, task: Option<&TaskInfo>) {
        let Some(t) = task else {
            ui.weak("Select a task to view blocks");
            return;
        };
        if t.total_blocks == 0 {
            ui.weak("No block data available");
            return;
        }

        let completed = t.completed_blocks;
        let total = t.total_blocks;
        let pct = (completed as f64 / total as f64 * 100.0) as u32;

        ui.label(
            RichText::new(format!("Block Map — {}/{} ({}%)", completed, total, pct))
                .strong()
                .small(),
        );
        ui.add_space(4.0);

        // Grid of blocks
        let side = 10.0;
        let gap = 2.0;
        let avail = ui.available_width();
        let per_row = ((avail - gap) / (side + gap)).floor().max(1.0) as u32;
        let rows = total.div_ceil(per_row).max(1);
        let height = rows as f32 * (side + gap);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(avail, height), Sense::hover());

        let painter = ui.painter();
        let mut done_left = completed;
        for r in 0..rows {
            for c in 0..per_row {
                let idx = r * per_row + c;
                if idx >= total {
                    break;
                }
                let x = rect.left() + c as f32 * (side + gap);
                let y = rect.top() + r as f32 * (side + gap);
                let color = if done_left > 0 {
                    done_left -= 1;
                    Color32::from_rgb(60, 163, 86) // green
                } else {
                    Color32::from_rgb(55, 57, 63) // dark gray
                };
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(side, side)),
                    2.0,
                    color,
                );
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────

fn info_row(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).weak().small());
        ui.label(RichText::new(value).small());
    });
}

fn status_text(t: &TaskInfo) -> RichText {
    let (text, color) = if t.status == "Completed" {
        ("Complete", Color32::from_rgb(60, 163, 86))
    } else if t.status.starts_with("Failed") {
        ("Failed", Color32::RED)
    } else if t.paused {
        ("Paused", Color32::from_rgb(230, 160, 50))
    } else if t.status == "Stopped" {
        ("Stopped", Color32::from_rgb(140, 140, 140))
    } else if t.total_bytes == 0 {
        ("Queued", Color32::from_rgb(100, 160, 220))
    } else {
        ("Downloading", Color32::from_rgb(80, 160, 255))
    };
    RichText::new(text).color(color).small()
}

fn eta_text(t: &TaskInfo) -> String {
    if t.done || t.paused || t.speed == 0 || t.total_bytes == 0 {
        return "—".into();
    }
    let remaining = t.total_bytes.saturating_sub(t.downloaded);
    let secs = remaining / t.speed;
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn format_bytes(n: u64) -> String {
    if n == 0 {
        return "—".into();
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
    if s == 0 {
        return "—".into();
    }
    format!("{}/s", format_bytes(s))
}
