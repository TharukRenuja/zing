//! IDM-style main window.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Button, Color32, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Legend, Line, Plot, PlotPoints};

use crate::client::{GuiClient, TaskInfo};
use crate::notify as zing_notify;

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
    error: Option<String>,
    speed_history: VecDeque<(f64, f64)>,
    started: Instant,
    version: String,
    quitting: bool,
    /// Task whose standalone IDM-style progress window is open.
    progress_id: Option<u64>,
    /// Task ids we already notified "started" for (to avoid duplicate popups).
    notified_start: std::collections::HashSet<u64>,
    /// Task ids we already notified "done" for.
    notified_done: std::collections::HashSet<u64>,
    /// Task ids the user added through the GUI's own Add URL dialog.
    user_initiated: std::collections::HashSet<u64>,
    last_seen_ids: Vec<u64>,
    // ── Add URL dialog state ──
    show_add_dialog: bool,
    add_url: String,
    add_filename: String,
    add_dir: String,
    add_connections: usize,
    add_speed_limit: String,
    add_user_agent: String,
    add_referer: String,
    add_custom_headers: String,
    add_proxy: String,
    add_mirror: String,
    add_max_filesize: String,
    add_insecure: bool,
    add_auto_rename: bool,
    add_allow_overwrite: bool,
    add_advanced_open: bool,
    prev_add_url: String,
    add_filename_auto: bool,
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
            error: None,
            speed_history: VecDeque::new(),
            started: Instant::now(),
            version,
            quitting: false,
            progress_id: None,
            notified_start: std::collections::HashSet::new(),
            notified_done: std::collections::HashSet::new(),
            user_initiated: std::collections::HashSet::new(),
            last_seen_ids: Vec::new(),
            show_add_dialog: false,
            add_url: String::new(),
            add_filename: String::new(),
            add_dir: dirs::download_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
            add_connections: 0,
            add_speed_limit: String::new(),
            add_user_agent: String::new(),
            add_referer: String::new(),
            add_custom_headers: String::new(),
            add_proxy: String::new(),
            add_mirror: String::new(),
            add_max_filesize: String::new(),
            add_insecure: false,
            add_auto_rename: false,
            add_allow_overwrite: false,
            add_advanced_open: false,
            prev_add_url: String::new(),
            add_filename_auto: false,
        }
    }

    fn refresh(&mut self) {
        if let Ok(s) = self.snapshot.lock() {
            self.tasks = s.clone();
        }
        let now = self.started.elapsed().as_secs_f64();
        let total: f64 = self.tasks.iter().map(|t| t.speed).sum();
        self.speed_history.push_back((now, total));
        if self.speed_history.len() > 300 {
            self.speed_history.pop_front();
        }
        self.check_transitions();
    }

    /// Detects tasks that started or completed and fires desktop
    /// notifications. Only new tasks (not seen in the previous poll) are
    /// treated as "started"; those the user added via the GUI's own Add URL
    /// dialog are not "silent" so no start popup is shown.
    fn check_transitions(&mut self) {
        let current: Vec<u64> = self.tasks.iter().map(|t| t.id).collect();
        let new_ids: Vec<u64> = current
            .iter()
            .copied()
            .filter(|id| !self.last_seen_ids.contains(id))
            .collect();
        self.last_seen_ids = current;

        for id in new_ids {
            let Some(task) = self.tasks.iter().find(|t| t.id == id) else {
                continue;
            };
            if task.done {
                self.notified_done.insert(id);
                continue;
            }
            if !task.paused && !self.user_initiated.contains(&id) {
                zing_notify::started(&task.filename, &task.url);
                self.notified_start.insert(id);
            }
        }

        for task in &self.tasks {
            if task.done && !self.notified_done.contains(&task.id) {
                zing_notify::completed(&task.filename, task.total_bytes);
                self.notified_done.insert(task.id);
            }
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
        // ── Standalone progress window ────────────────────────
        if self.progress_id.is_some() {
            self.render_progress_window(ctx);
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
                        self.quitting = true;
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
                        let total: f64 = self.tasks.iter().map(|t| t.speed).sum();
                        let dl = self.tasks.iter().filter(|t| !t.done && !t.paused).count();
                        ui.label(
                            RichText::new(format!(
                                "zing {} | {} active | {}",
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
                            "{} tasks | {} / {}",
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
            .default_width(440.0)
            .max_width(440.0)
            .show(ctx, |ui| {
                ui.set_min_width(400.0);
                ui.add_space(4.0);

                // ── URL (large, prominent) ──
                ui.label(RichText::new("URL").strong().small());
                let url_resp = ui.add(
                    egui::TextEdit::multiline(&mut self.add_url)
                        .desired_width(ui.available_width()),
                );
                // Auto-fill filename from URL when URL changes
                let url_changed = self.add_url != self.prev_add_url;
                if url_changed {
                    self.prev_add_url = self.add_url.clone();
                    if self.add_filename.is_empty() || self.add_filename_auto {
                        if let Some(name) = filename_from_url(&self.add_url) {
                            self.add_filename = name;
                            self.add_filename_auto = true;
                        }
                    }
                }
                if url_resp.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && !self.add_url.trim().is_empty()
                {
                    self.submit_add_url();
                }

                ui.add_space(6.0);

                // ── Save As / Save To row ──
                ui.columns(2, |cols| {
                    cols[0].label(RichText::new("Save As").strong().small());
                    let fname_resp = cols[0]
                        .text_edit_singleline(&mut self.add_filename)
                        .on_hover_text("Leave blank to use the filename from the URL");
                    if fname_resp.changed() {
                        self.add_filename_auto = false;
                    }
                    cols[1].label(RichText::new("Save To").strong().small());
                    cols[1].horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.add_dir)
                                .desired_width(ui.available_width() - 60.0),
                        );
                        if ui.button("Browse").clicked() {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.add_dir = dir.to_string_lossy().to_string();
                            }
                        }
                    });
                });

                ui.add_space(6.0);

                // ── Segments slider ──
                ui.label(RichText::new("Segments (connections)").strong().small());
                ui.horizontal(|ui| {
                    let presets = [0usize, 1, 2, 4, 8, 16, 32];
                    for &n in &presets {
                        let label = if n == 0 {
                            "Auto".to_string()
                        } else {
                            n.to_string()
                        };
                        let selected = self.add_connections == n;
                        if ui.selectable_label(selected, &label).clicked() {
                            self.add_connections = n;
                        }
                    }
                });

                ui.add_space(6.0);

                // ── Advanced options (collapsible) ──
                let header = if self.add_advanced_open {
                    "Advanced Options  [click to collapse]"
                } else {
                    "Advanced Options  [click to expand]"
                };
                if ui
                    .selectable_label(
                        self.add_advanced_open,
                        RichText::new(header).strong().small(),
                    )
                    .clicked()
                {
                    self.add_advanced_open = !self.add_advanced_open;
                }

                if self.add_advanced_open {
                    ui.add_space(4.0);
                    ui.group(|ui| {
                        ui.set_min_width(ui.available_width());

                        // Speed limit
                        ui.horizontal(|ui| {
                            ui.label("Speed limit:");
                            ui.text_edit_singleline(&mut self.add_speed_limit)
                                .on_hover_text("e.g. 1 MB/s, 500 KB/s, 0 = unlimited");
                        });

                        // User-Agent
                        ui.horizontal(|ui| {
                            ui.label("User-Agent:");
                            ui.text_edit_singleline(&mut self.add_user_agent)
                                .on_hover_text("Custom User-Agent header");
                        });

                        // Referer
                        ui.horizontal(|ui| {
                            ui.label("Referer:");
                            ui.text_edit_singleline(&mut self.add_referer)
                                .on_hover_text("Referer URL header");
                        });

                        // Custom headers
                        ui.horizontal(|ui| {
                            ui.label("Extra headers:");
                            ui.text_edit_singleline(&mut self.add_custom_headers)
                                .on_hover_text("One per line, format: Key: Value");
                        });

                        // Proxy
                        ui.horizontal(|ui| {
                            ui.label("Proxy:");
                            ui.text_edit_singleline(&mut self.add_proxy)
                                .on_hover_text("HTTP/HTTPS proxy URL");
                        });

                        // Mirror
                        ui.horizontal(|ui| {
                            ui.label("Mirror URL:");
                            ui.text_edit_singleline(&mut self.add_mirror)
                                .on_hover_text("Mirror URL for multi-source download");
                        });

                        // Max filesize
                        ui.horizontal(|ui| {
                            ui.label("Max filesize:");
                            ui.text_edit_singleline(&mut self.add_max_filesize)
                                .on_hover_text("e.g. 100 MB, 1 GB. Skips download if larger.");
                        });

                        ui.add_space(4.0);

                        // Checkboxes row
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.add_insecure, "Skip TLS verification");
                            ui.checkbox(&mut self.add_auto_rename, "Auto-rename if exists");
                            ui.checkbox(&mut self.add_allow_overwrite, "Allow overwrite");
                        });
                    });
                }

                ui.add_space(8.0);

                // ── Action buttons ──
                ui.horizontal(|ui| {
                    let can_add = !self.add_url.trim().is_empty();
                    if ui
                        .add_enabled(can_add, Button::new(RichText::new("Download").strong()))
                        .clicked()
                    {
                        self.submit_add_url();
                    }
                    if ui.button("Cancel").clicked() {
                        self.clear_add_dialog();
                    }
                });
            });
        if !open {
            self.clear_add_dialog();
        }
    }

    fn clear_add_dialog(&mut self) {
        self.show_add_dialog = false;
        self.add_url.clear();
        self.add_filename.clear();
        self.add_dir = dirs::download_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        self.add_connections = 0;
        self.add_speed_limit.clear();
        self.add_user_agent.clear();
        self.add_referer.clear();
        self.add_custom_headers.clear();
        self.add_proxy.clear();
        self.add_mirror.clear();
        self.add_max_filesize.clear();
        self.add_insecure = false;
        self.add_auto_rename = false;
        self.add_allow_overwrite = false;
        self.add_advanced_open = false;
        self.prev_add_url.clear();
        self.add_filename_auto = false;
    }

    fn submit_add_url(&mut self) {
        let url = self.add_url.trim().to_string();
        if url.is_empty() {
            return;
        }

        let mut params = serde_json::json!({ "url": url });

        if !self.add_filename.is_empty() {
            params["filename"] = serde_json::json!(self.add_filename);
        }
        if !self.add_dir.is_empty() {
            params["dir"] = serde_json::json!(self.add_dir);
        }
        if self.add_connections > 0 {
            params["connections"] = serde_json::json!(self.add_connections);
        }
        if !self.add_speed_limit.is_empty() {
            if let Some(bytes) = parse_speed(&self.add_speed_limit) {
                params["max_download_rate"] = serde_json::json!(bytes);
            }
        }
        if !self.add_user_agent.is_empty() {
            params["headers"] = serde_json::json!([format!("User-Agent: {}", self.add_user_agent)]);
        }
        if !self.add_referer.is_empty() {
            let h = params["headers"].as_array().cloned().unwrap_or_default();
            let mut headers: Vec<serde_json::Value> = h;
            headers.push(serde_json::json!(format!("Referer: {}", self.add_referer)));
            params["headers"] = serde_json::json!(headers);
        }
        if !self.add_custom_headers.is_empty() {
            let h = params["headers"].as_array().cloned().unwrap_or_default();
            let mut headers: Vec<serde_json::Value> = h;
            for line in self.add_custom_headers.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    headers.push(serde_json::json!(line.to_string()));
                }
            }
            params["headers"] = serde_json::json!(headers);
        }
        if !self.add_proxy.is_empty() {
            params["proxy"] = serde_json::json!(self.add_proxy);
        }
        if !self.add_mirror.is_empty() {
            params["mirror"] = serde_json::json!([self.add_mirror.clone()]);
        }
        if !self.add_max_filesize.is_empty() {
            if let Some(bytes) = parse_filesize(&self.add_max_filesize) {
                params["max_filesize"] = serde_json::json!(bytes);
            }
        }
        if self.add_insecure {
            params["insecure"] = serde_json::json!(true);
        }
        if self.add_auto_rename {
            params["auto_file_renaming"] = serde_json::json!(true);
        }
        if self.add_allow_overwrite {
            params["allow_overwrite"] = serde_json::json!(true);
        }

        match self.client.add_uri(params) {
            Ok(id) => {
                self.user_initiated.insert(id);
                self.clear_add_dialog();
            }
            Err(e) => {
                self.error = Some(e);
                self.show_add_dialog = false;
            }
        }
    }

    fn render_progress_window(&mut self, ctx: &egui::Context) {
        let id = match self.progress_id {
            Some(id) => id,
            None => return,
        };
        let task = self.tasks.iter().find(|t| t.id == id).cloned();
        let Some(t) = task else {
            self.progress_id = None;
            return;
        };

        let mut open = true;
        let title = format!("{} - Progress", t.filename);
        egui::Window::new(&title)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .min_width(440.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);

                // ── Filename + status badge ──
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&t.filename).strong());
                    ui.separator();
                    ui.label(status_text(&t));
                });

                ui.add_space(6.0);

                // ── Big progress bar ──
                let p = t.progress_fraction();
                let pct = (p * 100.0) as u32;
                let bar = egui::ProgressBar::new(p)
                    .text(format!(
                        "{}%  |  {} / {}",
                        pct,
                        format_bytes(t.downloaded),
                        format_bytes(t.total_bytes)
                    ))
                    .desired_width(ui.available_width());
                ui.add(bar);

                ui.add_space(6.0);

                // ── Stats row ──
                ui.columns(5, |cols| {
                    stat_cell(&mut cols[0], "Speed", &format_speed(t.speed));
                    stat_cell(&mut cols[1], "Peak", &format_speed(t.peak_speed));
                    stat_cell(&mut cols[2], "ETA", &eta_text(&t));
                    stat_cell(&mut cols[3], "Conns", &t.connections.len().to_string());
                    stat_cell(
                        &mut cols[4],
                        "Blocks",
                        &format!("{}/{}", t.completed_blocks, t.total_blocks),
                    );
                });

                ui.add_space(4.0);

                // ── Block map ──
                if t.total_blocks > 0 {
                    ui.label(RichText::new("Block Map").strong().small());
                    ui.add_space(2.0);
                    render_block_map(ui, &t);
                }

                ui.add_space(6.0);

                // ── Controls ──
                ui.horizontal(|ui| {
                    if t.done {
                        ui.label(RichText::new("Completed").strong());
                    } else if t.paused {
                        if ui.button(RichText::new("Resume")).clicked() {
                            let _ = self.client.resume(id);
                        }
                        if ui.button(RichText::new("Stop")).clicked() {
                            let _ = self.client.stop(id);
                        }
                    } else {
                        if ui.button(RichText::new("Pause")).clicked() {
                            let _ = self.client.pause(id);
                        }
                        if ui.button(RichText::new("Stop")).clicked() {
                            let _ = self.client.stop(id);
                        }
                    }
                    ui.separator();
                    if ui.button(RichText::new("Remove")).clicked() {
                        let _ = self.client.remove(id);
                        self.progress_id = None;
                        self.selected = None;
                    }
                    if ui.button(RichText::new("Open folder")).clicked() {
                        if let Some(parent) = std::path::Path::new(&t.filename).parent() {
                            let _ = open::that(parent);
                        }
                    }
                });
            });
        if !open {
            self.progress_id = None;
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
                        row.col(|ui| {
                            let resp = ui.label(RichText::new(&task.filename).strong());
                            if resp.clicked() {
                                self.selected = Some(id);
                            }
                            if resp.double_clicked() {
                                self.selected = Some(id);
                                self.progress_id = Some(id);
                            }
                        });
                        row.col(|ui| {
                            ui.label(format_bytes(task.total_bytes));
                        });
                        row.col(|ui| {
                            ui.label(status_text(task));
                        });
                        row.col(|ui| {
                            if task.paused || task.done || task.speed == 0.0 {
                                ui.label(RichText::new("—").weak());
                            } else {
                                ui.label(format_speed(task.speed));
                            }
                        });
                        row.col(|ui| {
                            ui.label(eta_text(task));
                        });
                        row.col(|ui| {
                            ui.label(task.connections.len().to_string());
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
                info_row(ui, "Connections:", &t.connections.len().to_string());
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
    if t.done || t.paused || t.speed == 0.0 || t.total_bytes == 0 {
        return "—".into();
    }
    let remaining = t.total_bytes.saturating_sub(t.downloaded);
    let secs = remaining as f64 / t.speed;
    let secs = secs as u64;
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub fn format_bytes(n: u64) -> String {
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

fn format_speed(s: f64) -> String {
    if s == 0.0 {
        return "—".into();
    }
    format!("{}/s", format_bytes(s as u64))
}

fn stat_cell(ui: &mut Ui, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).weak().small());
        ui.label(RichText::new(value).strong());
    });
}

fn render_block_map(ui: &mut Ui, t: &TaskInfo) {
    let total = t.total_blocks;
    let completed = t.completed_blocks;
    let side = 8.0;
    let gap = 1.5;
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
                Color32::from_rgb(60, 163, 86)
            } else {
                Color32::from_rgb(55, 57, 63)
            };
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(side, side)),
                2.0,
                color,
            );
        }
    }
}

/// Extract a filename from a URL path, stripping query params and fragments.
fn filename_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    // Strip scheme + authority
    let path = url
        .find("://")
        .and_then(|i| url[i + 3..].find('/'))
        .map(|i| &url[url.find("://").unwrap() + 3 + i..])
        .unwrap_or(url);
    // Strip query and fragment
    let path = path.split('?').next().unwrap_or(path);
    let path = path.split('#').next().unwrap_or(path);
    // Get last segment
    let name = path.rsplit('/').next()?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    // URL-decode common escapes
    let name = name.replace("%20", " ");
    Some(name)
}

/// Parse a human-readable speed string like "1 MB/s" or "500KB/s" into bytes/sec.
fn parse_speed(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace("/s", "").replace(' ', "");
    parse_size_bytes(&s)
}

/// Parse a human-readable size string like "100 MB" or "1.5GB" into bytes.
fn parse_filesize(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase().replace(' ', "");
    parse_size_bytes(&s)
}

fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() || s == "0" {
        return Some(0);
    }
    let units = [
        ("gb", 1_073_741_824u64),
        ("mb", 1_048_576),
        ("kb", 1024),
        ("b", 1),
    ];
    for &(suffix, multiplier) in &units {
        if let Some(num_str) = s.strip_suffix(suffix) {
            let num_str = num_str.trim();
            let val: f64 = num_str.parse().ok()?;
            return Some((val * multiplier as f64) as u64);
        }
    }
    s.parse::<u64>().ok()
}
