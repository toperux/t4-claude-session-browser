use anyhow::Result;
use chrono::Local;
use eframe::egui::{self, Align2, Color32, RichText, Sense};
use std::cmp::Reverse;
use std::collections::HashSet;
use std::sync::mpsc::{channel, Receiver};

use crate::del::{self, human_bytes, DeletePlan};
use crate::index::{truncate, Index, SessionMeta};
use crate::paths::ClaudeDir;
use crate::transcript::{self, Entry, Event, LoadOpts, Transcript};
use crate::update::Available;

const ROLE_USER: Color32 = Color32::from_rgb(0x5c, 0xb8, 0x5c);
const ROLE_ASSISTANT: Color32 = Color32::from_rgb(0x6f, 0xa8, 0xdc);
const ROLE_TOOL: Color32 = Color32::from_rgb(0xd8, 0xa6, 0x57);
const ROLE_THINKING: Color32 = Color32::from_rgb(0xa9, 0x8c, 0xd0);
const ROLE_ERROR: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x6c);

/// How many transcript entries are drawn before the "show more" button.
const PAGE: usize = 300;

/// Vertical padding inside a session row; part of the fixed row height.
const ROW_MARGIN_Y: f32 = 5.0;

#[derive(PartialEq, Clone, Copy)]
enum Sort {
    Date,
    Size,
    Msgs,
}

pub fn run(dir: ClaudeDir) -> Result<()> {
    let app = App::new(dir)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 880.0])
            .with_min_inner_size([900.0, 500.0])
            .with_title("Claude Session Browser"),
        ..Default::default()
    };
    eframe::run_native(
        "Claude Session Browser",
        options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Ask GitHub for a newer release off the UI thread. The startup check is
/// throttled and its errors collapse to `None` - a failed check must be
/// indistinguishable from "up to date". A `manual` check (the settings window)
/// ignores the throttle and reports failure, because the user asked.
fn spawn_update_check(ctx: egui::Context, manual: bool) -> UpdateCheck {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = if manual {
            crate::update::check_now().map_err(|e| e.to_string())
        } else {
            Ok(crate::update::check_throttled().unwrap_or(None))
        };
        let _ = tx.send(result);
        ctx.request_repaint();
    });
    UpdateCheck { rx, manual }
}

/// An update check in flight.
struct UpdateCheck {
    rx: Receiver<Result<Option<Available>, String>>,
    manual: bool,
}

/// Download and swap the executable. Sends the installed version, or the error.
fn spawn_install(ctx: egui::Context) -> Receiver<Result<String, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        // No progress bar: there is no console attached to a GUI launch.
        let result = crate::update::install(false, false)
            .map(|s| s.version().to_string())
            .map_err(|e| e.to_string());
        let _ = tx.send(result);
        ctx.request_repaint();
    });
    rx
}

/// A transcript being parsed off the UI thread.
struct PendingPreview {
    id: String,
    rx: Receiver<Result<Transcript, String>>,
}

/// Precomputed sidebar row. Rebuilding these per frame meant cloning every
/// `SessionMeta` on every repaint.
struct ProjectRow {
    slug: String,
    label: String,
    count: usize,
    bytes: u64,
}

struct App {
    dir: ClaudeDir,
    index: Index,
    project_rows: Vec<ProjectRow>,

    project_filter: Option<String>,
    search: String,
    sort: Sort,
    visible: Vec<SessionMeta>,

    /// Sessions ticked for deletion.
    marked: HashSet<String>,
    /// The session shown in the preview pane.
    focused: Option<String>,
    /// Row under the cursor, so the next frame can highlight it.
    hovered: Option<String>,
    /// Anchor for shift-click ranges.
    anchor: Option<usize>,

    pending: Option<PendingPreview>,
    preview: Option<(String, Transcript)>,
    /// Lower-cased searchable text per preview entry, built once on load so
    /// the find box does not re-lowercase the whole transcript every frame.
    preview_lower: Vec<String>,
    /// Why the current selection has no transcript, if it failed.
    preview_error: Option<String>,
    preview_search: String,
    preview_shown: usize,

    confirm: Option<Vec<DeletePlan>>,
    status: String,
    settings_open: bool,
    /// Outcome of the last manual update check, shown in the settings window.
    update_note: String,

    /// The startup check, or a manual one, running off the UI thread.
    update_check: Option<UpdateCheck>,
    update_started: bool,
    /// A newer release, once one is known. `None` keeps the banner hidden.
    update_banner: Option<Available>,
    /// An install in flight, and its result once it lands.
    install_rx: Option<Receiver<Result<String, String>>>,
    installed: Option<String>,
}

impl App {
    fn new(dir: ClaudeDir) -> Result<Self> {
        let index = Index::build(&dir)?;
        let mut app = Self {
            dir,
            index,
            project_rows: Vec::new(),
            project_filter: None,
            search: String::new(),
            sort: Sort::Date,
            visible: Vec::new(),
            marked: HashSet::new(),
            focused: None,
            hovered: None,
            anchor: None,
            pending: None,
            preview: None,
            preview_lower: Vec::new(),
            preview_error: None,
            preview_search: String::new(),
            preview_shown: PAGE,
            confirm: None,
            status: String::new(),
            settings_open: false,
            update_note: String::new(),
            // Spawned on the first frame instead of here: the worker needs an
            // egui Context to request a repaint when its result lands.
            update_check: None,
            update_started: false,
            update_banner: None,
            install_rx: None,
            installed: None,
        };
        app.status = app.index.warning_summary().unwrap_or_default();
        app.rebuild_projects();
        app.refilter();
        Ok(app)
    }

    fn rebuild_projects(&mut self) {
        self.project_rows = self
            .index
            .projects()
            .into_iter()
            .map(|p| ProjectRow {
                count: p.sessions.len(),
                bytes: p.size_bytes(),
                slug: p.slug,
                label: p.label,
            })
            .collect();
    }

    fn refilter(&mut self) {
        let needle = self.search.to_lowercase();
        let mut list: Vec<SessionMeta> = self
            .index
            .sessions
            .iter()
            .filter(|s| {
                self.project_filter
                    .as_ref()
                    .is_none_or(|slug| &s.project_slug == slug)
            })
            .filter(|s| {
                needle.is_empty()
                    || s.title.to_lowercase().contains(&needle)
                    || s.location().to_lowercase().contains(&needle)
                    || s.id.starts_with(&needle)
            })
            .cloned()
            .collect();

        match self.sort {
            Sort::Date => list.sort_by_key(|s| Reverse(s.activity())),
            Sort::Size => list.sort_by_key(|s| Reverse(s.size_bytes)),
            Sort::Msgs => list.sort_by_key(|s| Reverse(s.user_msgs + s.assistant_msgs)),
        }
        self.visible = list;
        self.anchor = None;
    }

    /// Parse a transcript on a worker thread so a 20 MB session never blocks a frame.
    fn focus(&mut self, id: &str, ctx: &egui::Context) {
        // Re-clicking a session that failed to load retries it, rather than
        // being a no-op that leaves the pane stuck.
        let settled = self.pending.is_some() || self.preview.is_some();
        if self.focused.as_deref() == Some(id) && settled {
            return;
        }
        self.focused = Some(id.to_string());
        self.preview = None;
        self.preview_error = None;
        self.preview_shown = PAGE;

        // Exact match, not `Index::find`: that is a prefix lookup, and an id
        // that happens to prefix another would refuse to load at all.
        let Some(meta) = self.index.sessions.iter().find(|s| s.id == id) else {
            self.preview_error = Some(format!("session {id} is no longer in the index"));
            return;
        };
        let path = meta.path.clone();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = transcript::load(&path, &LoadOpts::default()).map_err(|e| e.to_string());
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.pending = Some(PendingPreview {
            id: id.to_string(),
            rx,
        });
    }

    fn poll_preview(&mut self) {
        use std::sync::mpsc::TryRecvError;

        let Some(pending) = &self.pending else { return };
        let received = match pending.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            // The worker died without sending - treat it as a failure rather
            // than waiting on a channel that will never produce anything.
            Err(TryRecvError::Disconnected) => Err("transcript reader stopped".to_string()),
        };
        let id = pending.id.clone();
        self.pending = None;
        match received {
            Ok(t) => {
                self.preview_lower = t
                    .entries
                    .iter()
                    .map(|e| e.event.searchable().to_lowercase())
                    .collect();
                self.preview = Some((id, t));
            }
            Err(e) => {
                self.status = format!("preview failed: {e}");
                self.preview_error = Some(e);
            }
        }
    }

    /// Drain the startup check and any in-flight install.
    ///
    /// A failed *check* is silent by design: a machine that is offline, behind a
    /// proxy, or rate-limited should look exactly like one that is up to date.
    /// A failed *install* is not silent - the user asked for that one.
    fn poll_update(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;

        if !self.update_started {
            self.update_started = true;
            self.update_check = Some(spawn_update_check(ctx.clone(), false));
        }

        let landed = self
            .update_check
            .as_ref()
            .and_then(|c| match c.rx.try_recv() {
                Ok(result) => Some((c.manual, result)),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some((c.manual, Err("update checker stopped".to_string())))
                }
            });
        if let Some((manual, result)) = landed {
            self.update_check = None;
            match result {
                Ok(found) => {
                    if manual {
                        self.update_note = match &found {
                            Some(a) => format!("csb {} is available", a.version),
                            None => format!("csb {} is up to date", crate::update::CURRENT),
                        };
                    }
                    // `None` also clears a stale banner, e.g. after a CLI update.
                    self.update_banner = found;
                }
                Err(e) if manual => self.update_note = format!("update check failed: {e}"),
                Err(_) => {}
            }
        }

        if let Some(rx) = &self.install_rx {
            match rx.try_recv() {
                Ok(Ok(version)) => {
                    self.installed = Some(version);
                    self.update_banner = None;
                    self.install_rx = None;
                }
                Ok(Err(e)) => {
                    self.status = format!("update failed: {e}");
                    self.install_rx = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.status = "update failed: updater stopped".to_string();
                    self.install_rx = None;
                }
            }
        }
    }

    /// Thin strip above the toolbar. Renders nothing unless there is news.
    fn update_bar(&mut self, ctx: &egui::Context) {
        if self.update_banner.is_none() && self.installed.is_none() && self.install_rx.is_none() {
            return;
        }
        // Tinted and padded so it reads as a notice, not another toolbar row:
        // amber while an update is on offer, green once it has landed.
        // The green is muted further than the amber: a success notice should
        // sit back, an offer should stand out.
        let fill = if self.installed.is_some() {
            ROLE_USER.gamma_multiply(0.10)
        } else {
            ROLE_TOOL.gamma_multiply(0.18)
        };
        let frame = egui::Frame::default()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(14.0, 10.0));
        egui::TopBottomPanel::top("update")
            .frame(frame)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    if let Some(version) = &self.installed {
                        ui.label(
                            RichText::new(format!(
                                "✔ csb {version} installed — restart csb to use it"
                            ))
                            .color(ROLE_USER)
                            .strong(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                self.installed = None;
                            }
                        });
                        return;
                    }
                    if self.install_rx.is_some() {
                        ui.add(egui::Spinner::new());
                        ui.label("downloading update…");
                        return;
                    }
                    let Some(found) = &self.update_banner else {
                        return;
                    };
                    ui.label(
                        RichText::new(format!("⬆ csb {} is available", found.version))
                            .color(ROLE_TOOL)
                            .strong(),
                    );
                    ui.label(RichText::new(format!("you have {}", crate::update::CURRENT)).weak());
                    if crate::update::package_managed() {
                        // An Update button here would only fail; say what works.
                        ui.label(RichText::new("upgrade it with your package manager").weak())
                            .on_hover_text(crate::update::PACKAGE_MANAGED_HINT);
                    } else {
                        let update = egui::Button::new(
                            RichText::new("Update").strong().color(Color32::BLACK),
                        )
                        .fill(ROLE_TOOL);
                        if ui.add(update).clicked() {
                            self.install_rx = Some(spawn_install(ctx.clone()));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Dismiss").clicked() {
                            self.update_banner = None;
                        }
                    });
                });
            });
    }

    fn focused_meta(&self) -> Option<&SessionMeta> {
        let id = self.focused.as_ref()?;
        self.index.sessions.iter().find(|s| &s.id == id)
    }

    /// Marked sessions, or the focused one when nothing is marked. Clones, so
    /// call it when acting - not to render a label every frame.
    fn delete_targets(&self) -> Vec<SessionMeta> {
        if self.marked.is_empty() {
            return self.focused_meta().cloned().into_iter().collect();
        }
        self.index
            .sessions
            .iter()
            .filter(|s| self.marked.contains(&s.id))
            .cloned()
            .collect()
    }

    /// Count and size of what `delete_targets` would return, without cloning.
    /// Must mirror its selection rule exactly, or the button and the action
    /// would disagree.
    fn selection_summary(&self) -> (usize, u64) {
        if self.marked.is_empty() {
            return match self.focused_meta() {
                Some(s) => (1, s.size_bytes),
                None => (0, 0),
            };
        }
        let bytes = self
            .index
            .sessions
            .iter()
            .filter(|s| self.marked.contains(&s.id))
            .map(|s| s.size_bytes)
            .sum();
        (self.marked.len(), bytes)
    }

    fn reindex(&mut self) {
        match Index::build(&self.dir) {
            Ok(index) => {
                self.index = index;
                if let Some(summary) = self.index.warning_summary() {
                    self.status = summary;
                }
                let alive: HashSet<&String> = self.index.sessions.iter().map(|s| &s.id).collect();
                self.marked.retain(|id| alive.contains(id));
                if self.focused.as_ref().is_some_and(|f| !alive.contains(f)) {
                    self.focused = None;
                    self.preview = None;
                    self.preview_error = None;
                }
                drop(alive);
                self.rebuild_projects();
                self.refilter();
            }
            Err(e) => self.status = format!("reindex failed: {e}"),
        }
    }

    fn run_delete(&mut self, plans: &[DeletePlan]) {
        let mut ok = 0;
        for p in plans {
            match del::execute(&self.dir, p) {
                Ok(()) => ok += 1,
                Err(e) => {
                    self.status = format!("deleted {ok}, then failed: {e}");
                    self.marked.clear();
                    self.reindex();
                    return;
                }
            }
        }
        let bytes: u64 = plans.iter().map(|p| p.bytes).sum();
        self.status = format!(
            "moved {ok} session(s) ({}) to the recycle bin",
            human_bytes(bytes)
        );
        self.marked.clear();
        self.reindex();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_preview();
        self.poll_update(ctx);

        self.update_bar(ctx);
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ui));
        egui::TopBottomPanel::bottom("actions").show(ctx, |ui| self.action_bar(ui));
        egui::SidePanel::left("projects")
            .default_width(250.0)
            .width_range(160.0..=420.0)
            .show(ctx, |ui| self.projects_pane(ui));
        egui::SidePanel::left("sessions")
            .default_width(400.0)
            .width_range(260.0..=700.0)
            .show(ctx, |ui| self.sessions_pane(ui, ctx));
        egui::CentralPanel::default().show(ctx, |ui| self.preview_pane(ui));

        if self.settings_open {
            self.settings_window(ctx);
        }
        if let Some(plans) = self.confirm.take() {
            self.confirm_modal(ctx, plans);
        }
    }
}

impl App {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Claude Session Browser");
            ui.separator();
            ui.label("Search");
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.search)
                        .desired_width(260.0)
                        .hint_text("title, path or id"),
                )
                .changed()
            {
                self.refilter();
            }
            if !self.search.is_empty() && ui.button("✕").clicked() {
                self.search.clear();
                self.refilter();
            }

            ui.separator();
            ui.label("Sort");
            let mut sort = self.sort;
            egui::ComboBox::from_id_salt("sort")
                .selected_text(match sort {
                    Sort::Date => "recent",
                    Sort::Size => "size",
                    Sort::Msgs => "messages",
                })
                .width(110.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut sort, Sort::Date, "recent");
                    ui.selectable_value(&mut sort, Sort::Size, "size");
                    ui.selectable_value(&mut sort, Sort::Msgs, "messages");
                });
            if sort != self.sort {
                self.sort = sort;
                self.refilter();
            }

            ui.separator();
            if ui.button("⟲ Reload").clicked() {
                self.reindex();
                // reindex reports unreadable files; don't overwrite that.
                if self.index.warnings.is_empty() {
                    self.status = "reindexed".into();
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⚙").on_hover_text("Settings").clicked() {
                    self.settings_open = !self.settings_open;
                }
                ui.label(RichText::new(&self.status).color(ROLE_USER));
            });
        });
        ui.add_space(4.0);
    }

    /// Version and the manual update check. Non-modal: closing it does not
    /// cancel a check in flight, the result just waits here.
    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.set_min_width(440.0);
                ui.add_space(4.0);

                ui.label(RichText::new("ABOUT").small().weak());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Claude Session Browser").strong());
                    ui.label(RichText::new(format!("v{}", crate::update::CURRENT)).weak());
                });
                let repo = format!(
                    "github.com/{}/{}",
                    crate::update::REPO_OWNER,
                    crate::update::REPO_NAME
                );
                ui.hyperlink_to(&repo, format!("https://{repo}"));

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                ui.label(RichText::new("UPDATES").small().weak());
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "csb looks for a new release once a day at startup. \
                         Checking here asks GitHub right now.",
                    )
                    .weak(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    // Disabled while any check is in flight, the startup one included.
                    if ui
                        .add_enabled(
                            self.update_check.is_none(),
                            egui::Button::new("Check for updates"),
                        )
                        .clicked()
                    {
                        if crate::update::checks_disabled() {
                            self.update_note =
                                "update checks are disabled (CSB_NO_UPDATE_CHECK is set)".into();
                        } else {
                            self.update_note = "checking…".into();
                            self.update_check = Some(spawn_update_check(ctx.clone(), true));
                        }
                    }
                    // Any check, so the disabled button right after launch is
                    // explained too.
                    if self.update_check.is_some() {
                        ui.spinner();
                    }
                });
                if !self.update_note.is_empty() {
                    ui.label(RichText::new(&self.update_note).weak());
                }
                ui.add_space(8.0);
            });
        self.settings_open = open;
    }

    fn projects_pane(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(RichText::new("PROJECTS").small().weak());
        ui.add_space(4.0);

        let total: u64 = self.index.sessions.iter().map(|s| s.size_bytes).sum();
        let mut pick: Option<Option<String>> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if project_row(
                    ui,
                    "*all*",
                    self.project_filter.is_none(),
                    "All projects",
                    self.index.sessions.len(),
                    total,
                ) {
                    pick = Some(None);
                }
                for p in &self.project_rows {
                    let selected = self.project_filter.as_deref() == Some(p.slug.as_str());
                    if project_row(ui, &p.slug, selected, &p.label, p.count, p.bytes) {
                        pick = Some(Some(p.slug.clone()));
                    }
                }
            });

        if let Some(next) = pick {
            self.project_filter = next;
            self.refilter();
        }
    }

    fn sessions_pane(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("SESSIONS ({})", self.visible.len()))
                    .small()
                    .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("none").clicked() {
                    self.marked.clear();
                }
                if ui.small_button("all").clicked() {
                    for s in &self.visible {
                        self.marked.insert(s.id.clone());
                    }
                }
            });
        });
        ui.add_space(4.0);

        // Row interactions are collected and applied after the loop.
        let mut clicked: Option<(usize, bool, bool)> = None;
        let mut toggled: Option<String> = None;
        let mut next_hovered: Option<String> = None;

        // Rows are laid out only for the visible range. That needs a fixed row
        // height, so the two text lines below must not wrap: the title is
        // truncated by the label, the meta line is a plain `horizontal`.
        let row_height = ui
            .text_style_height(&egui::TextStyle::Body)
            .max(ui.spacing().interact_size.y)
            + ui.spacing().item_spacing.y
            + ui.text_style_height(&egui::TextStyle::Small)
            + 2.0 * ROW_MARGIN_Y;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_height, self.visible.len(), |ui, range| {
                for i in range {
                    let s = &self.visible[i];
                    let focused = self.focused.as_deref() == Some(s.id.as_str());
                    let mut mark = self.marked.contains(&s.id);

                    let fill = if focused {
                        ui.visuals().selection.bg_fill.gamma_multiply(0.55)
                    } else if self.hovered.as_deref() == Some(s.id.as_str()) {
                        ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.35)
                    } else {
                        Color32::TRANSPARENT
                    };

                    // Track where the checkbox ends: the row's click target has
                    // to start after it, or it would swallow every tick.
                    let mut checkbox_right = 0.0_f32;
                    let frame = egui::Frame::none()
                        .fill(fill)
                        .inner_margin(egui::Margin::symmetric(6.0, ROW_MARGIN_Y))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal_top(|ui| {
                                let cb = ui.checkbox(&mut mark, "");
                                if cb.changed() {
                                    toggled = Some(s.id.clone());
                                }
                                checkbox_right = cb.rect.right();
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::Label::new(RichText::new(&s.title).strong())
                                            .truncate(),
                                    );
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!(
                                                "{}  ·  {} msgs  ·  {}",
                                                s.activity()
                                                    .with_timezone(&Local)
                                                    .format("%Y-%m-%d %H:%M"),
                                                s.user_msgs + s.assistant_msgs,
                                                human_bytes(s.size_bytes),
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                        if s.is_recent() {
                                            ui.label(
                                                RichText::new("ACTIVE?").small().color(ROLE_ERROR),
                                            );
                                        }
                                    });
                                });
                            });
                        });

                    // An explicit rect and a stable id, rather than interacting
                    // with a layout response whose auto-id can shift per frame.
                    let outer = frame.response.rect;
                    let body_rect = egui::Rect::from_min_max(
                        egui::pos2(checkbox_right + 4.0, outer.min.y),
                        outer.max,
                    );
                    let resp = ui
                        .interact(
                            body_rect,
                            egui::Id::new(("session-row", s.id.as_str())),
                            Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.hovered() {
                        next_hovered = Some(s.id.clone());
                    }

                    if resp.clicked() {
                        let mods = ui.input(|i| i.modifiers);
                        clicked = Some((i, mods.ctrl || mods.command, mods.shift));
                    }
                }
            });
        if self.visible.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("no sessions match").weak());
        }

        self.hovered = next_hovered;

        if let Some(id) = toggled {
            if !self.marked.remove(&id) {
                self.marked.insert(id);
            }
        }

        if let Some((i, ctrl, shift)) = clicked {
            let id = self.visible[i].id.clone();
            if shift {
                if let Some(from) = self.anchor {
                    let (lo, hi) = if from <= i { (from, i) } else { (i, from) };
                    for s in &self.visible[lo..=hi] {
                        self.marked.insert(s.id.clone());
                    }
                }
            } else if ctrl && !self.marked.remove(&id) {
                self.marked.insert(id.clone());
            }
            self.anchor = Some(i);
            self.focus(&id, ctx);
        }
    }

    fn preview_pane(&mut self, ui: &mut egui::Ui) {
        let Some(meta) = self.focused_meta().cloned() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("select a session to preview it").weak());
            });
            return;
        };

        ui.add_space(6.0);
        ui.label(RichText::new(&meta.title).heading());
        ui.label(
            RichText::new(format!(
                "{}  ·  {}{}",
                meta.id,
                meta.location(),
                meta.git_branch
                    .as_ref()
                    .map(|b| format!("  ·  {b}"))
                    .unwrap_or_default(),
            ))
            .small()
            .weak(),
        );
        ui.label(
            RichText::new(format!(
                "{} user · {} assistant · {} tool calls · {}",
                meta.user_msgs,
                meta.assistant_msgs,
                meta.tool_calls,
                human_bytes(meta.size_bytes)
            ))
            .small()
            .weak(),
        );

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Find");
            ui.add(
                egui::TextEdit::singleline(&mut self.preview_search)
                    .desired_width(240.0)
                    .hint_text("filter this transcript"),
            );
            if !self.preview_search.is_empty() && ui.button("✕").clicked() {
                self.preview_search.clear();
            }
        });
        ui.separator();

        let Some((id, transcript)) = &self.preview else {
            ui.add_space(20.0);
            match &self.preview_error {
                Some(err) => {
                    ui.label(
                        RichText::new(format!("could not read this transcript: {err}"))
                            .color(ROLE_ERROR),
                    );
                    ui.label(
                        RichText::new("click the session again to retry")
                            .small()
                            .weak(),
                    );
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("parsing transcript…");
                    });
                }
            }
            return;
        };
        if id != &meta.id {
            return; // a newer selection is still loading
        }

        let needle = self.preview_search.to_lowercase();
        let matches: Vec<&Entry> = transcript
            .entries
            .iter()
            .zip(&self.preview_lower)
            .filter(|(_, lower)| needle.is_empty() || lower.contains(&needle))
            .map(|(e, _)| e)
            .collect();

        if !needle.is_empty() {
            ui.label(
                RichText::new(format!("{} matching entries", matches.len()))
                    .small()
                    .weak(),
            );
        }

        let shown = self.preview_shown.min(matches.len());
        let mut grow = false;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for entry in &matches[..shown] {
                    draw_entry(ui, entry);
                }
                ui.add_space(8.0);
                if shown < matches.len() {
                    if ui
                        .button(format!("show more ({} remaining)", matches.len() - shown))
                        .clicked()
                    {
                        grow = true;
                    }
                } else if transcript.truncated {
                    ui.label(
                        RichText::new(
                            "preview capped — run `csb show <id>` for the whole transcript",
                        )
                        .small()
                        .weak(),
                    );
                }
                ui.add_space(8.0);
            });

        if grow {
            self.preview_shown += PAGE;
        }
    }

    fn action_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let (count, bytes) = self.selection_summary();

            let label = match count {
                0 => "nothing selected".to_string(),
                _ if self.marked.is_empty() => {
                    format!("focused session · {}", human_bytes(bytes))
                }
                n => format!("{n} marked · {}", human_bytes(bytes)),
            };
            ui.label(label);

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let text = if self.marked.is_empty() {
                    "🗑 Delete session".to_string()
                } else {
                    format!("🗑 Delete {} session(s)", self.marked.len())
                };
                if ui
                    .add_enabled(count > 0, egui::Button::new(RichText::new(text).strong()))
                    .clicked()
                {
                    let targets = self.delete_targets();
                    self.confirm = Some(targets.iter().map(|m| del::plan(&self.dir, m)).collect());
                }
                ui.label(RichText::new("goes to the recycle bin").small().weak());
            });
        });
        ui.add_space(6.0);
    }

    fn confirm_modal(&mut self, ctx: &egui::Context, plans: Vec<DeletePlan>) {
        let bytes: u64 = plans.iter().map(|p| p.bytes).sum();
        let files: usize = plans.iter().map(|p| p.paths.len()).sum();
        let live = plans.iter().filter(|p| p.recent).count();
        let mut decision: Option<bool> = None;

        // Dim everything behind the dialog *and* swallow input aimed at it.
        // Painting alone is not enough: the panels underneath stay live, so the
        // selection could be changed out from under a plan already on screen.
        egui::Area::new("modal-veil".into())
            .order(egui::Order::Middle)
            .fixed_pos(egui::Pos2::ZERO)
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                ui.painter()
                    .rect_filled(screen, 0.0, Color32::from_black_alpha(140));
                // A full-screen interactive widget in a layer above the panels
                // wins hit-testing, so clicks never reach them.
                ui.allocate_rect(screen, Sense::click_and_drag());
            });

        egui::Window::new("Confirm delete")
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(720.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(format!(
                        "Move {} session(s) — {files} paths, {} — to the recycle bin?",
                        plans.len(),
                        human_bytes(bytes)
                    ))
                    .strong(),
                );
                if live > 0 {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!(
                            "⚠ {live} of these were active in the last 5 minutes and may be open right now."
                        ))
                        .color(ROLE_ERROR)
                        .strong(),
                    );
                }
                ui.add_space(8.0);

                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for p in &plans {
                            ui.label(RichText::new(truncate(&p.title, 90)).strong());
                            for path in &p.paths {
                                ui.label(
                                    RichText::new(format!("    {}", path.display()))
                                        .small()
                                        .weak()
                                        .monospace(),
                                );
                            }
                            ui.add_space(4.0);
                        }
                    });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                    if ui
                        .button(RichText::new("Delete").color(ROLE_ERROR).strong())
                        .clicked()
                    {
                        decision = Some(true);
                    }
                });
            });

        match decision {
            Some(true) => self.run_delete(&plans),
            Some(false) => self.status = "delete cancelled".into(),
            // Still open: put it back for the next frame.
            None => self.confirm = Some(plans),
        }
    }
}

fn project_row(
    ui: &mut egui::Ui,
    key: &str,
    selected: bool,
    label: &str,
    count: usize,
    bytes: u64,
) -> bool {
    let fill = if selected {
        ui.visuals().selection.bg_fill.gamma_multiply(0.55)
    } else {
        Color32::TRANSPARENT
    };
    let frame = egui::Frame::none()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(6.0, 5.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.vertical(|ui| {
                ui.label(RichText::new(truncate(label, 60)));
                ui.label(
                    RichText::new(format!("{count} sessions · {}", human_bytes(bytes)))
                        .small()
                        .weak(),
                );
            });
        });
    ui.interact(
        frame.response.rect,
        egui::Id::new(("project-row", key)),
        Sense::click(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .clicked()
}

fn draw_entry(ui: &mut egui::Ui, entry: &Entry) {
    let stamp = entry
        .ts
        .map(|t| t.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_default();
    let side = entry.sidechain;

    match &entry.event {
        Event::User(text) => block(ui, "user", ROLE_USER, &stamp, side, text),
        Event::Assistant(text) => block(ui, "claude", ROLE_ASSISTANT, &stamp, side, text),
        Event::Thinking(text) => {
            egui::CollapsingHeader::new(
                RichText::new(format!("thinking · {} chars", text.chars().count()))
                    .small()
                    .color(ROLE_THINKING),
            )
            .id_salt(ui.next_auto_id())
            .show(ui, |ui| {
                ui.label(RichText::new(text).weak());
            });
        }
        Event::ToolUse {
            name,
            headline,
            raw,
        } => {
            // Headers do not wrap, so keep them inside the pane.
            egui::CollapsingHeader::new(
                RichText::new(truncate(&format!("▸ {name}: {headline}"), 110)).color(ROLE_TOOL),
            )
            .id_salt(ui.next_auto_id())
            .show(ui, |ui| {
                ui.label(RichText::new(raw).monospace().small());
            });
        }
        Event::ToolResult {
            is_error,
            preview,
            raw,
        } => {
            let color = if *is_error {
                ROLE_ERROR
            } else {
                ui.visuals().weak_text_color()
            };
            egui::CollapsingHeader::new(
                RichText::new(truncate(&format!("  ↳ {preview}"), 130))
                    .small()
                    .color(color),
            )
            .id_salt(ui.next_auto_id())
            .show(ui, |ui| {
                ui.label(RichText::new(truncate(raw, 20_000)).monospace().small());
            });
        }
        Event::Meta(label) => {
            ui.label(RichText::new(format!("· {label}")).small().weak());
        }
    }
}

fn block(ui: &mut egui::Ui, who: &str, color: Color32, stamp: &str, sidechain: bool, text: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(who).color(color).strong());
        ui.label(RichText::new(stamp).small().weak());
        if sidechain {
            ui.label(RichText::new("subagent").small().weak());
        }
    });
    // Very long turns are clipped; the CLI has the untruncated text.
    ui.label(truncate(text, 12_000));
    ui.add_space(2.0);
}
