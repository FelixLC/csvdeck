mod engine;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, Context, CornerRadius, Event, FontId, Key, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2,
};
use engine::{Filter, Op, Query, Stats};
use polars::prelude::{AnyValue, DataFrame};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ROW_H: f32 = 22.0;
const HEAD_H: f32 = 66.0;
const DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_COPY_CELLS: usize = 2_000_000;

enum Msg {
    Loaded { gen: u64, path: PathBuf, sep: u8, res: Result<DataFrame, String>, ms: u128 },
    View { gen: u64, res: Result<DataFrame, String>, ms: u128 },
    Stats { gen: u64, stats: Vec<Stats> },
    Exported(Result<PathBuf, String>),
    Folder { dir: PathBuf, files: Vec<(PathBuf, u64)> },
}

/// Grid hit target, in data column indices (0 = first CSV column).
#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Header(usize),
    Cell(usize, usize),
    Gutter(usize),
}

struct App {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    gen: Arc<AtomicU64>,

    file: Option<PathBuf>,
    folder: Option<PathBuf>,
    folder_files: Vec<(PathBuf, u64)>,
    file_search: String,
    reveal_file: bool,
    sep_choice: Option<u8>,
    sep: u8,
    header: bool,

    df: Option<DataFrame>,
    view: Option<DataFrame>,
    names: Vec<String>,
    numeric: Vec<bool>,
    widths: Vec<f32>,
    hidden: Vec<bool>,
    stats: Vec<Stats>,

    q: Query,
    dirty: Option<Instant>,

    sel_col: Option<usize>,
    cursor: Option<(usize, usize)>,
    anchor: Option<(usize, usize)>,
    scroll_to_cursor: bool,
    menu_target: Option<Hit>,
    cell_popup: Option<(String, String)>,
    scroll_offset: Vec2,
    viewport: Vec2,
    focus_search: bool,
    col_search: String,

    busy: Option<&'static str>,
    status: String,
    show_filters: bool,
    show_summary: bool,
    insights: bool,
    load_ms: u128,
    query_ms: u128,
}

fn main() -> eframe::Result {
    // Finder may append a `-psn_…` process serial number argument.
    let path = std::env::args().skip(1).find(|a| !a.starts_with("-psn")).map(PathBuf::from);
    #[cfg(target_os = "macos")]
    macos::install_open_handler();
    eframe::run_native(
        "csvdeck",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1360.0, 860.0])
                .with_min_inner_size([640.0, 400.0])
                .with_title("csvdeck")
                .with_drag_and_drop(true),
            ..Default::default()
        },
        Box::new(move |cc| {
            cc.egui_ctx.all_styles_mut(|s| {
                s.spacing.item_spacing = vec2(8.0, 6.0);
                s.spacing.button_padding = vec2(8.0, 3.0);
            });
            #[cfg(target_os = "macos")]
            let _ = macos::CTX.set(cc.egui_ctx.clone());
            let mut app = App::new();
            if let Some(p) = path {
                app.open_path(&cc.egui_ctx, p);
            }
            Ok(Box::new(app))
        }),
    )
}

impl App {
    fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            gen: Arc::new(AtomicU64::new(0)),
            file: None,
            folder: None,
            folder_files: vec![],
            file_search: String::new(),
            reveal_file: false,
            sep_choice: None,
            sep: b',',
            header: true,
            df: None,
            view: None,
            names: vec![],
            numeric: vec![],
            widths: vec![],
            hidden: vec![],
            stats: vec![],
            q: Query::default(),
            dirty: None,
            sel_col: None,
            cursor: None,
            anchor: None,
            scroll_to_cursor: false,
            menu_target: None,
            cell_popup: None,
            scroll_offset: Vec2::ZERO,
            viewport: Vec2::ZERO,
            focus_search: false,
            col_search: String::new(),
            busy: None,
            status: String::new(),
            show_filters: false,
            show_summary: true,
            insights: true,
            load_ms: 0,
            query_ms: 0,
        }
    }

    fn next_gen(&self) -> u64 {
        self.gen.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn spawn(&self, ctx: &Context, job: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    // ---------- jobs ----------

    fn open(&mut self, ctx: &Context, path: PathBuf) {
        let gen = self.next_gen();
        let sep = self.sep_choice.unwrap_or_else(|| engine::sniff_separator(&path));
        let header = self.header;
        self.busy = Some("Loading");
        self.status = format!("Opening {}", path.display());
        self.spawn(ctx, move || {
            let t = Instant::now();
            let res = engine::load(&path, sep, header).map_err(|e| e.to_string());
            Msg::Loaded { gen, path, sep, res, ms: t.elapsed().as_millis() }
        });
    }

    fn open_path(&mut self, ctx: &Context, path: PathBuf) {
        if path.is_dir() {
            self.open_folder(ctx, path);
        } else {
            self.open(ctx, path);
        }
    }

    fn open_folder(&mut self, ctx: &Context, dir: PathBuf) {
        self.folder = Some(dir.clone());
        self.folder_files.clear();
        self.file_search.clear();
        self.status = format!("Scanning {}", dir.display());
        self.spawn(ctx, move || Msg::Folder { files: engine::list_data_files(&dir), dir });
    }

    /// Files in the sidebar after the name filter.
    fn listed_files(&self) -> Vec<&PathBuf> {
        let needle = self.file_search.to_lowercase();
        let root = self.folder.as_deref().unwrap_or(std::path::Path::new(""));
        self.folder_files
            .iter()
            .map(|(p, _)| p)
            .filter(|p| needle.is_empty() || p.strip_prefix(root).unwrap_or(p).to_string_lossy().to_lowercase().contains(&needle))
            .collect()
    }

    fn step_file(&mut self, ctx: &Context, delta: isize) {
        let files = self.listed_files();
        if files.is_empty() {
            return;
        }
        let at = self.file.as_ref().and_then(|f| files.iter().position(|p| *p == f));
        let next = match at {
            Some(i) => (i as isize + delta).clamp(0, files.len() as isize - 1) as usize,
            None => 0,
        };
        if at != Some(next) {
            let p = files[next].clone();
            self.reveal_file = true;
            self.open(ctx, p);
        }
    }

    fn run_query(&mut self, ctx: &Context) {
        let Some(df) = self.df.clone() else { return };
        let gen = self.next_gen();
        if self.q.is_identity() {
            self.query_ms = 0;
            self.set_view(ctx, gen, df);
            return;
        }
        let q = self.q.clone();
        self.busy = Some("Filtering");
        self.spawn(ctx, move || {
            let t = Instant::now();
            let res = engine::query(&df, &q).map_err(|e| e.to_string());
            Msg::View { gen, res, ms: t.elapsed().as_millis() }
        });
    }

    fn set_view(&mut self, ctx: &Context, gen: u64, view: DataFrame) {
        let n = view.height();
        self.cursor = self.cursor.filter(|c| c.0 < n);
        self.anchor = self.anchor.filter(|c| c.0 < n);
        self.view = Some(view.clone());
        self.busy = Some("Profiling");
        let current = self.gen.clone();
        self.spawn(ctx, move || match engine::stats(&view, gen, &current) {
            Some(stats) => Msg::Stats { gen, stats },
            None => Msg::Stats { gen: u64::MAX, stats: vec![] },
        });
    }

    fn export(&mut self, ctx: &Context) {
        let Some(view) = self.view.clone() else { return };
        let stem = self.file.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string());
        let ext = if self.file.as_deref().is_some_and(engine::is_parquet) { "parquet" } else { "csv" };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}_filtered.{ext}", stem.unwrap_or_else(|| "export".into())))
            .add_filter("CSV", &["csv"])
            .add_filter("Parquet", &["parquet"])
            .save_file()
        else {
            return;
        };
        let sep = self.sep;
        self.busy = Some("Exporting");
        self.spawn(ctx, move || {
            Msg::Exported(engine::export(&view, &path, sep).map(|_| path).map_err(|e| e.to_string()))
        });
    }

    fn handle_messages(&mut self, ctx: &Context) {
        while let Ok(msg) = self.rx.try_recv() {
            let current = self.gen.load(Ordering::Relaxed);
            match msg {
                Msg::Loaded { gen, path, sep, res, ms } if gen == current => match res {
                    Ok(df) => {
                        self.names = df.columns()[1..].iter().map(|c| c.name().to_string()).collect();
                        self.numeric = df.columns()[1..].iter().map(|c| c.dtype().is_primitive_numeric()).collect();
                        self.widths = initial_widths(&df);
                        self.hidden = vec![false; self.names.len()];
                        self.stats.clear();
                        self.q = Query::default();
                        (self.cursor, self.anchor, self.sel_col) = (None, None, None);
                        self.scroll_offset = Vec2::ZERO;
                        self.load_ms = ms;
                        self.query_ms = 0;
                        self.sep = sep;
                        self.status.clear();
                        self.file = Some(path);
                        self.df = Some(df.clone());
                        self.set_view(ctx, gen, df);
                    }
                    Err(e) => {
                        self.busy = None;
                        self.status = format!("Could not open file: {e}");
                    }
                },
                Msg::View { gen, res, ms } if gen == current => match res {
                    Ok(view) => {
                        self.query_ms = ms;
                        self.status.clear();
                        self.set_view(ctx, gen, view);
                    }
                    Err(e) => {
                        self.busy = None;
                        self.status = format!("Filter error: {e}");
                    }
                },
                Msg::Stats { gen, stats } if gen == current => {
                    self.stats = stats;
                    self.busy = None;
                }
                Msg::Folder { dir, files } if self.folder.as_ref() == Some(&dir) => {
                    self.status = if files.is_empty() { "No CSV, TSV or Parquet files in this folder".into() } else { String::new() };
                    let first = files.first().map(|f| f.0.clone());
                    self.folder_files = files;
                    let current_listed = self.file.as_ref().is_some_and(|f| self.folder_files.iter().any(|(p, _)| p == f));
                    if let (false, Some(p)) = (current_listed, first) {
                        self.open(ctx, p);
                    }
                }
                Msg::Exported(res) => {
                    self.busy = None;
                    self.status = match res {
                        Ok(p) => format!("Exported to {}", p.display()),
                        Err(e) => format!("Export failed: {e}"),
                    };
                }
                _ => {}
            }
        }
    }

    // ---------- helpers ----------

    fn visible_cols(&self) -> Vec<usize> {
        (0..self.names.len()).filter(|&j| !self.hidden[j]).collect()
    }

    fn cell_text(&self, view: &DataFrame, r: usize, j: usize) -> String {
        engine::fmt(&view.columns()[j + 1].get(r).unwrap_or(AnyValue::Null))
    }

    fn add_filter(&mut self, j: usize, op: Op, value: String) {
        self.q.filters.push(Filter { col: self.names[j].clone(), op, value, enabled: true });
        self.show_filters = true;
    }

    fn set_sort(&mut self, j: usize, desc: Option<bool>) {
        self.q.sort = desc.map(|d| (self.names[j].clone(), d));
    }

    fn sort_of(&self, j: usize) -> Option<bool> {
        self.q.sort.as_ref().filter(|(n, _)| *n == self.names[j]).map(|s| s.1)
    }

    /// Selection as (row range, visible-position range).
    fn selection(&self, vis: &[usize]) -> Option<(std::ops::RangeInclusive<usize>, std::ops::RangeInclusive<usize>)> {
        let (c, a) = (self.cursor?, self.anchor.or(self.cursor)?);
        let pc = vis.iter().position(|&j| j == c.1)?;
        let pa = vis.iter().position(|&j| j == a.1).unwrap_or(pc);
        Some((c.0.min(a.0)..=c.0.max(a.0), pc.min(pa)..=pc.max(pa)))
    }

    fn copy_selection(&mut self, ctx: &Context) {
        let Some(view) = self.view.clone() else { return };
        let vis = self.visible_cols();
        let Some((rows, cols)) = self.selection(&vis) else { return };
        let cells = rows.clone().count() * cols.clone().count();
        if cells > MAX_COPY_CELLS {
            self.status = format!("Selection too large to copy ({cells} cells). Use Export.");
            return;
        }
        let mut out = String::new();
        for r in rows {
            let line: Vec<String> = cols.clone().map(|p| self.cell_text(&view, r, vis[p])).collect();
            out.push_str(&line.join("\t"));
            out.push('\n');
        }
        out.pop();
        ctx.copy_text(out);
        self.status = format!("Copied {cells} cell{}", if cells == 1 { "" } else { "s" });
    }

    // ---------- keyboard ----------

    fn keyboard(&mut self, ctx: &Context) {
        // ⌘⇧O first: ⌘O also matches when shift is held.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, Key::O)) {
            self.pick_folder(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::O)) {
            self.pick_file(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::F)) {
            self.focus_search = true;
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        if self.folder.is_some() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, Key::ArrowDown)) {
                self.step_file(ctx, 1);
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, Key::ArrowUp)) {
                self.step_file(ctx, -1);
            }
        }
        let Some(view) = &self.view else { return };
        let n = view.height();
        let vis = self.visible_cols();
        if n == 0 || vis.is_empty() {
            return;
        }
        let page = ((self.viewport.y - HEAD_H) / ROW_H).max(1.0) as isize;
        let (copy, select_all, esc, moves) = ctx.input(|i| {
            let m = i.modifiers;
            let mut mv: Option<(isize, isize, bool)> = None;
            let far: isize = 1_000_000_000;
            for (k, dr, dc) in [
                (Key::ArrowUp, -1isize, 0isize),
                (Key::ArrowDown, 1, 0),
                (Key::ArrowLeft, 0, -1),
                (Key::ArrowRight, 0, 1),
                (Key::PageUp, -page, 0),
                (Key::PageDown, page, 0),
                (Key::Home, -far, 0),
                (Key::End, far, 0),
                (Key::Tab, 0, 1),
                (Key::Enter, 1, 0),
            ] {
                if i.key_pressed(k) {
                    let (dr, dc) = if m.command { (dr.signum() * far, dc.signum() * far) } else { (dr, dc) };
                    mv = Some((dr, dc, m.shift && !matches!(k, Key::Tab | Key::Enter)));
                }
            }
            (
                i.events.iter().any(|e| matches!(e, Event::Copy)),
                m.command && i.key_pressed(Key::A),
                i.key_pressed(Key::Escape),
                mv,
            )
        });
        if copy {
            self.copy_selection(ctx);
        }
        if select_all {
            self.anchor = Some((0, vis[0]));
            self.cursor = Some((n - 1, vis[vis.len() - 1]));
        }
        if esc {
            (self.cursor, self.anchor) = (None, None);
        }
        if let Some((dr, dc, extend)) = moves {
            let (r, j) = self.cursor.unwrap_or((0, vis[0]));
            let p = vis.iter().position(|&x| x == j).unwrap_or(0) as isize;
            let r = (r as isize + dr).clamp(0, n as isize - 1) as usize;
            let p = (p + dc).clamp(0, vis.len() as isize - 1) as usize;
            self.cursor = Some((r, vis[p]));
            if !extend {
                self.anchor = self.cursor;
            }
            self.sel_col = Some(vis[p]);
            self.scroll_to_cursor = true;
        }
    }

    fn pick_file(&mut self, ctx: &Context) {
        if let Some(p) = rfd::FileDialog::new()
            .add_filter("Data files", &engine::DATA_EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_file()
        {
            self.open(ctx, p);
        }
    }

    fn pick_folder(&mut self, ctx: &Context) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.open_folder(ctx, dir);
        }
    }

    // ---------- panels ----------

    fn toolbar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            if ui.button("📂 Open").on_hover_text("Open a file  ⌘O").clicked() {
                self.pick_file(&ctx);
            }
            if ui.button("🗁 Folder").on_hover_text("Open a folder  ⌘ Shift O").clicked() {
                self.pick_folder(&ctx);
            }
            if let Some(f) = &self.file {
                ui.label(RichText::new(f.file_name().unwrap_or_default().to_string_lossy()).strong())
                    .on_hover_text(f.display().to_string());
            }
            ui.separator();

            let sep_label = |s: Option<u8>| match s {
                None => "Auto".to_string(),
                Some(b'\t') => "Tab".to_string(),
                Some(c) => format!("{}", c as char),
            };
            let before = (self.sep_choice, self.header);
            let text_file = !self.file.as_deref().is_some_and(engine::is_parquet);
            ui.add_enabled_ui(text_file, |ui| {
                egui::ComboBox::from_id_salt("sep")
                    .selected_text(format!("Delimiter: {}", sep_label(self.sep_choice)))
                    .show_ui(ui, |ui| {
                        for s in [None, Some(b','), Some(b';'), Some(b'\t'), Some(b'|')] {
                            ui.selectable_value(&mut self.sep_choice, s, sep_label(s));
                        }
                    });
                ui.checkbox(&mut self.header, "Header row");
            });
            if before != (self.sep_choice, self.header) {
                if let Some(p) = self.file.clone() {
                    self.open(&ctx, p);
                }
            }
            ui.separator();

            let search = ui.add(
                TextEdit::singleline(&mut self.q.search)
                    .hint_text("🔍 Search all columns  ⌘F")
                    .desired_width(260.0),
            );
            if std::mem::take(&mut self.focus_search) {
                search.request_focus();
            }
            ui.toggle_value(&mut self.q.case_sensitive, "Aa").on_hover_text("Match case");
            ui.separator();

            let active = self.q.filters.iter().filter(|f| f.enabled).count();
            let label = if active > 0 { format!("⏷ Filters ({active})") } else { "⏷ Filters".into() };
            ui.toggle_value(&mut self.show_filters, label);
            ui.toggle_value(&mut self.show_summary, "📊 Summary");
            ui.toggle_value(&mut self.insights, "📈 Insights").on_hover_text("Column profiles in the header");
            self.columns_menu(ui);
            if ui.add_enabled(self.view.is_some(), egui::Button::new("💾 Export view…")).clicked() {
                self.export(&ctx);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::widgets::global_theme_preference_switch(ui);
            });
        });
    }

    fn columns_menu(&mut self, ui: &mut Ui) {
        let hidden = self.hidden.iter().filter(|h| **h).count();
        let label = if hidden > 0 { format!("☰ Columns ({hidden} hidden)") } else { "☰ Columns".into() };
        // Stay open while clicking checkboxes and the search box inside.
        let config = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        egui::containers::menu::MenuButton::new(label).config(config).ui(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Show all").clicked() {
                    self.hidden.iter_mut().for_each(|h| *h = false);
                }
                if ui.button("Hide all").clicked() {
                    self.hidden.iter_mut().for_each(|h| *h = true);
                }
            });
            ui.add(TextEdit::singleline(&mut self.col_search).hint_text("Find column").desired_width(220.0));
            let needle = self.col_search.to_lowercase();
            ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for (j, name) in self.names.iter().enumerate() {
                    if needle.is_empty() || name.to_lowercase().contains(&needle) {
                        let mut shown = !self.hidden[j];
                        if ui.checkbox(&mut shown, name).changed() {
                            self.hidden[j] = !shown;
                        }
                    }
                }
            });
        });
    }

    fn filter_bar(&mut self, ui: &mut Ui) {
        let mut remove = None;
        for (i, f) in self.q.filters.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.enabled, "");
                egui::ComboBox::from_id_salt(("fcol", i))
                    .width(180.0)
                    .selected_text(&f.col)
                    .show_ui(ui, |ui| {
                        for n in &self.names {
                            ui.selectable_value(&mut f.col, n.clone(), n);
                        }
                    });
                egui::ComboBox::from_id_salt(("fop", i))
                    .width(130.0)
                    .selected_text(f.op.label())
                    .show_ui(ui, |ui| {
                        for op in Op::ALL {
                            ui.selectable_value(&mut f.op, op, op.label());
                        }
                    });
                if f.op.needs_value() {
                    ui.add(TextEdit::singleline(&mut f.value).hint_text("value").desired_width(240.0));
                }
                if ui.small_button("🗙").on_hover_text("Remove filter").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.q.filters.remove(i);
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.names.is_empty(), egui::Button::new("➕ Add filter")).clicked() {
                let j = self.sel_col.unwrap_or(0);
                self.add_filter(j, Op::Contains, String::new());
            }
            if self.q.filters.len() > 1 {
                egui::ComboBox::from_id_salt("anyall")
                    .selected_text(if self.q.any { "Match any" } else { "Match all" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.q.any, false, "Match all");
                        ui.selectable_value(&mut self.q.any, true, "Match any");
                    });
            }
            if !self.q.filters.is_empty() && ui.button("Clear filters").clicked() {
                self.q.filters.clear();
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if let Some(b) = self.busy {
                ui.add(egui::Spinner::new());
                ui.label(format!("{b}…"));
                ui.separator();
            }
            if let (Some(df), Some(view)) = (&self.df, &self.view) {
                let (n, total) = (view.height(), df.height());
                if n == total {
                    ui.label(format!("{} rows", thousands(total)));
                } else {
                    ui.label(RichText::new(format!("{} of {} rows", thousands(n), thousands(total))).strong());
                }
                ui.label(format!("· {} columns", self.names.len()));
                ui.weak(format!("· loaded in {} ms", self.load_ms));
                if self.query_ms > 0 {
                    ui.weak(format!("· filtered in {} ms", self.query_ms));
                }
            }
            if !self.status.is_empty() {
                ui.separator();
                ui.label(&self.status);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let (Some((r, j)), Some(view)) = (self.cursor, &self.view) {
                    let vis = self.visible_cols();
                    if let Some((rows, cols)) = self.selection(&vis) {
                        let cells = rows.count() * cols.count();
                        if cells > 1 {
                            ui.weak(format!("{} cells selected", thousands(cells)));
                        }
                    }
                    let v = self.cell_text(view, r, j);
                    let preview: String = v.chars().take(120).collect::<String>().replace('\n', "¶");
                    ui.label(RichText::new(preview).monospace());
                    ui.weak(format!("{} · row {}:", self.names[j], r + 1));
                }
            });
        });
    }

    fn files_panel(&mut self, ui: &mut Ui) {
        let Some(root) = self.folder.clone() else { return };
        let ctx = ui.ctx().clone();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let name = root.file_name().unwrap_or(root.as_os_str()).to_string_lossy();
            ui.strong(format!("🗁 {name}")).on_hover_text(root.display().to_string());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("🗙").on_hover_text("Close folder").clicked() {
                    self.folder = None;
                }
            });
        });
        ui.add(TextEdit::singleline(&mut self.file_search).hint_text("Filter files").desired_width(f32::INFINITY));
        let files: Vec<PathBuf> = self.listed_files().into_iter().cloned().collect();
        ui.weak(format!("{} of {} files · Option ⏶ ⏷ to switch", files.len(), self.folder_files.len()));
        ui.add_space(2.0);
        let mut picked = None;
        let reveal = std::mem::take(&mut self.reveal_file);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for p in &files {
                let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().to_string();
                let size = self.folder_files.iter().find(|(f, _)| f == p).map_or(0, |f| f.1);
                let selected = self.file.as_ref() == Some(p);
                let resp = ui
                    .add(
                        egui::Button::selectable(selected, rel)
                            .truncate()
                            .right_text(RichText::new(human_size(size)).weak().small())
                            .min_size(vec2(ui.available_width(), 0.0)),
                    )
                    .on_hover_text(p.display().to_string());
                if selected && reveal {
                    resp.scroll_to_me(None);
                }
                if resp.clicked() && !selected {
                    picked = Some(p.clone());
                }
            }
        });
        if let Some(p) = picked {
            self.open(&ctx, p);
        }
    }

    fn summary(&mut self, ui: &mut Ui) {
        let Some(j) = self.sel_col.filter(|&j| j < self.names.len()) else {
            ui.add_space(12.0);
            ui.weak("Click a column header to see its profile.");
            ui.add_space(8.0);
            ui.weak("Double-click a header to sort, right-click for more.");
            return;
        };
        let rows = self.view.as_ref().map_or(0, |v| v.height());
        ui.add_space(6.0);
        ui.heading(&self.names[j]);
        let Some(s) = self.stats.get(j).cloned() else {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new());
                ui.weak("Profiling…");
            });
            return;
        };
        ui.weak(format!("{} · {} rows", s.dtype, thousands(rows)));
        ui.add_space(6.0);

        egui::Grid::new("stats").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            let pct = |n: usize| if rows == 0 { 0.0 } else { n as f64 * 100.0 / rows as f64 };
            ui.label("Missing");
            ui.label(format!("{} ({:.1}%)", thousands(s.nulls), pct(s.nulls)));
            ui.end_row();
            ui.label("Distinct");
            ui.label(format!("{} ({:.1}%)", thousands(s.distinct), pct(s.distinct)));
            ui.end_row();
            if !s.min.is_empty() {
                ui.label("Min");
                ui.label(&s.min);
                ui.end_row();
                ui.label("Max");
                ui.label(&s.max);
                ui.end_row();
            }
            if let Some(m) = s.mean {
                ui.label("Mean");
                ui.label(format!("{m:.4}"));
                ui.end_row();
            }
        });

        if !s.hist.is_empty() {
            ui.add_space(10.0);
            ui.strong("Distribution");
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 110.0), Sense::hover());
            draw_hist(ui, rect, &s.hist, accent(ui));
            ui.horizontal(|ui| {
                ui.weak(&s.min);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.weak(&s.max));
            });
        }

        if !s.top.is_empty() {
            ui.add_space(10.0);
            ui.strong("Most frequent");
            ui.weak("Click a value to filter on it");
            let total = rows.max(1) as f32;
            for (v, n) in &s.top {
                let w = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
                let frac = *n as f32 / total;
                let p = ui.painter();
                if resp.hovered() {
                    p.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let bar = Rect::from_min_size(rect.min, vec2(w * frac, rect.height()));
                p.rect_filled(bar, 3.0, accent(ui).gamma_multiply(0.25));
                let label = if v.is_empty() { "(empty)".to_string() } else { v.replace('\n', "¶") };
                let count = p.text(
                    rect.right_center() - vec2(6.0, 0.0),
                    Align2::RIGHT_CENTER,
                    format!("{} · {:.1}%", thousands(*n as usize), frac * 100.0),
                    FontId::proportional(11.5),
                    ui.visuals().weak_text_color(),
                );
                let label_clip = Rect::from_min_max(rect.min, pos2(count.min.x - 8.0, rect.max.y));
                p.with_clip_rect(label_clip).text(
                    rect.left_center() + vec2(6.0, 0.0),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(13.0),
                    ui.visuals().text_color(),
                );
                if resp.on_hover_text(v).clicked() {
                    self.add_filter(j, Op::Equals, v.clone());
                }
            }
        }

        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Sort ⏶").clicked() {
                self.set_sort(j, Some(false));
            }
            if ui.button("Sort ⏷").clicked() {
                self.set_sort(j, Some(true));
            }
            if ui.button("Filter…").clicked() {
                self.add_filter(j, Op::Contains, String::new());
            }
            if ui.button("Hide column").clicked() {
                self.hidden[j] = true;
                self.sel_col = None;
            }
        });
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.35);
            ui.label(RichText::new("csvdeck").size(32.0).strong());
            ui.add_space(4.0);
            ui.weak("Drop a CSV, TSV or Parquet file or a folder here, or");
            ui.add_space(8.0);
            if ui.button(RichText::new("Open file…  ⌘O").size(16.0)).clicked() {
                self.pick_file(&ctx);
            }
            if ui.button(RichText::new("Open folder…  ⌘ Shift O").size(16.0)).clicked() {
                self.pick_folder(&ctx);
            }
        });
    }

    // ---------- grid ----------

    fn grid(&mut self, ui: &mut Ui) {
        let Some(view) = self.view.clone() else { return };
        let vis = self.visible_cols();
        let n = view.height();
        let head_h = if self.insights { HEAD_H } else { 30.0 };
        let total_rows = self.df.as_ref().map_or(n, |d| d.height());
        let gutter = (thousands(total_rows).len() as f32) * 7.5 + 22.0;

        let mut xs = Vec::with_capacity(vis.len() + 1);
        let mut x = gutter;
        for &j in &vis {
            xs.push(x);
            x += self.widths[j];
        }
        xs.push(x);
        let content = vec2(x, head_h + n as f32 * ROW_H);

        let mut area = ScrollArea::both().auto_shrink(false).animated(false);
        if std::mem::take(&mut self.scroll_to_cursor) {
            if let Some((r, j)) = self.cursor {
                if let Some(p) = vis.iter().position(|&v| v == j) {
                    let mut off = self.scroll_offset;
                    let (cx0, cx1) = (xs[p], xs[p + 1]);
                    let (cy0, cy1) = (head_h + r as f32 * ROW_H, head_h + (r + 1) as f32 * ROW_H);
                    if cx0 - gutter < off.x {
                        off.x = cx0 - gutter;
                    } else if cx1 > off.x + self.viewport.x {
                        off.x = cx1 - self.viewport.x;
                    }
                    if cy0 - head_h < off.y {
                        off.y = cy0 - head_h;
                    } else if cy1 > off.y + self.viewport.y {
                        off.y = cy1 - self.viewport.y;
                    }
                    area = area.scroll_offset(off.max(Vec2::ZERO));
                }
            }
        }

        let visuals = ui.visuals().clone();
        let grid_line = Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6));
        let text_color = visuals.text_color();
        let weak = visuals.weak_text_color();
        let accent = accent(ui);
        let font = FontId::proportional(13.0);
        let small = FontId::proportional(11.0);
        let sel = self.selection(&vis);
        let mut clicked: Option<(Hit, bool, bool)> = None; // (hit, double, shift)
        let mut dragged_to: Option<Hit> = None;
        let mut resize: Option<(usize, f32)> = None;

        let out = area.show_viewport(ui, |ui, vp| {
            ui.set_min_size(content);
            let origin = ui.min_rect().min;
            let screen = |x: f32, y: f32| pos2(origin.x + x, origin.y + y);
            let painter = ui.painter().clone();
            let clip = Rect::from_min_max(screen(vp.min.x, vp.min.y), screen(vp.max.x, vp.max.y));
            let body_clip = Rect::from_min_max(screen(vp.min.x + gutter, vp.min.y + head_h), clip.max);

            let first = ((vp.min.y - head_h) / ROW_H).floor().max(0.0) as usize;
            let last = (((vp.max.y - head_h) / ROW_H).ceil().max(0.0) as usize).min(n);
            let pfirst = xs.partition_point(|&x0| x0 <= vp.min.x + gutter).saturating_sub(1).min(vis.len());
            let plast = xs.partition_point(|&x0| x0 < vp.max.x).min(vis.len());

            // Body
            let body = painter.with_clip_rect(body_clip);
            for r in first..last {
                let y = head_h + r as f32 * ROW_H;
                let row_rect = Rect::from_min_max(screen(vp.min.x, y), screen(vp.max.x, y + ROW_H));
                if r % 2 == 1 {
                    body.rect_filled(row_rect, 0.0, visuals.faint_bg_color);
                }
                for p in pfirst..plast {
                    let j = vis[p];
                    let cell = Rect::from_min_max(screen(xs[p], y), screen(xs[p + 1], y + ROW_H));
                    let selected = sel.as_ref().is_some_and(|(rs, ps)| rs.contains(&r) && ps.contains(&p));
                    if selected {
                        body.rect_filled(cell, 0.0, visuals.selection.bg_fill.gamma_multiply(0.35));
                    }
                    if self.cursor == Some((r, j)) {
                        body.rect_stroke(cell.shrink(1.0), 0.0, Stroke::new(2.0, accent), StrokeKind::Inside);
                    }
                    let mut v = self.cell_text(&view, r, j);
                    if v.len() > 300 {
                        v = v.chars().take(300).collect();
                    }
                    if v.contains('\n') {
                        v = v.replace('\n', "¶");
                    }
                    if !v.is_empty() {
                        let cp = body.with_clip_rect(cell.shrink2(vec2(5.0, 0.0)).intersect(body_clip));
                        if self.numeric[j] {
                            cp.text(cell.right_center() - vec2(6.0, 0.0), Align2::RIGHT_CENTER, v, font.clone(), text_color);
                        } else {
                            cp.text(cell.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, v, font.clone(), text_color);
                        }
                    }
                    body.vline(cell.right() - 0.5, cell.y_range(), grid_line);
                }
            }

            // Sticky gutter (original row numbers)
            let gutter_rect = Rect::from_min_max(screen(vp.min.x, vp.min.y + head_h), pos2(origin.x + vp.min.x + gutter, clip.max.y));
            let gp = painter.with_clip_rect(gutter_rect);
            gp.rect_filled(gutter_rect, 0.0, visuals.extreme_bg_color);
            let rows_col = &view.columns()[0];
            for r in first..last {
                let y = head_h + r as f32 * ROW_H;
                let label = engine::fmt(&rows_col.get(r).unwrap_or(AnyValue::Null));
                let in_sel = sel.as_ref().is_some_and(|(rs, _)| rs.contains(&r));
                gp.text(
                    screen(vp.min.x + gutter - 8.0, y + ROW_H / 2.0),
                    Align2::RIGHT_CENTER,
                    label,
                    small.clone(),
                    if in_sel { accent } else { weak },
                );
            }
            gp.vline(gutter_rect.right() - 0.5, gutter_rect.y_range(), grid_line);

            // Sticky header
            let head_rect = Rect::from_min_max(screen(vp.min.x, vp.min.y), pos2(clip.max.x, origin.y + vp.min.y + head_h));
            painter.rect_filled(head_rect, 0.0, visuals.panel_fill);
            let hp = painter.with_clip_rect(Rect::from_min_max(pos2(head_rect.min.x + gutter, head_rect.min.y), head_rect.max));
            for p in pfirst..plast {
                let j = vis[p];
                let r = Rect::from_min_max(screen(xs[p], vp.min.y), screen(xs[p + 1], vp.min.y + head_h));
                if self.sel_col == Some(j) {
                    hp.rect_filled(r, 0.0, accent.gamma_multiply(0.15));
                }
                let inner = r.shrink2(vec2(6.0, 4.0));
                let cp = hp.with_clip_rect(inner.intersect(hp.clip_rect()));
                let arrow = match self.sort_of(j) {
                    Some(false) => " ⏶",
                    Some(true) => " ⏷",
                    None => "",
                };
                cp.text(
                    inner.left_top(),
                    Align2::LEFT_TOP,
                    format!("{}{arrow}", self.names[j]),
                    FontId::proportional(13.5),
                    visuals.strong_text_color(),
                );
                if self.insights {
                    if let Some(s) = self.stats.get(j) {
                        let rows = n.max(1) as f32;
                        let meta = if s.nulls > 0 {
                            format!("{} · {} distinct · {:.0}% missing", s.dtype, compact(s.distinct), s.nulls as f32 * 100.0 / rows)
                        } else {
                            format!("{} · {} distinct", s.dtype, compact(s.distinct))
                        };
                        cp.text(inner.left_top() + vec2(0.0, 18.0), Align2::LEFT_TOP, meta, small.clone(), weak);
                        let viz = Rect::from_min_max(pos2(inner.min.x, inner.min.y + 34.0), inner.max);
                        if !s.hist.is_empty() {
                            draw_hist_painter(&cp, viz, &s.hist, accent);
                        } else {
                            draw_top_bar(&cp, viz, s, rows, accent, visuals.faint_bg_color);
                        }
                        if s.nulls > 0 {
                            let w = viz.width() * s.nulls as f32 / rows;
                            cp.rect_filled(
                                Rect::from_min_size(pos2(viz.min.x, r.max.y - 3.0), vec2(w.max(2.0), 2.0)),
                                0.0,
                                Color32::from_rgb(225, 90, 90),
                            );
                        }
                    } else {
                        cp.text(inner.left_top() + vec2(0.0, 18.0), Align2::LEFT_TOP, "…", small.clone(), weak);
                    }
                }
                hp.vline(r.right() - 0.5, r.y_range(), grid_line);
            }
            painter.hline(head_rect.x_range(), head_rect.bottom() - 0.5, grid_line);
            let corner = Rect::from_min_size(head_rect.min, vec2(gutter, head_h));
            painter.rect_filled(corner, 0.0, visuals.extreme_bg_color);
            painter.text(corner.center(), Align2::CENTER_CENTER, "#", small.clone(), weak);

            // Interaction: one sense for the whole grid, resize handles on top.
            let resp = ui.interact(clip, ui.id().with("grid"), Sense::click_and_drag());
            let hit_at = |pos: egui::Pos2| -> Option<Hit> {
                let (lx, ly) = (pos.x - origin.x, pos.y - origin.y);
                let p = xs.partition_point(|&x0| x0 <= lx).checked_sub(1).filter(|&p| p < vis.len());
                if ly - vp.min.y < head_h {
                    return (lx - vp.min.x >= gutter).then_some(p).flatten().map(|p| Hit::Header(vis[p]));
                }
                let r = ((ly - head_h) / ROW_H).floor();
                if r < 0.0 || r as usize >= n {
                    return None;
                }
                if lx - vp.min.x < gutter {
                    return Some(Hit::Gutter(r as usize));
                }
                p.map(|p| Hit::Cell(r as usize, vis[p]))
            };
            let shift = ui.input(|i| i.modifiers.shift);
            if let Some(pos) = resp.interact_pointer_pos() {
                if resp.double_clicked() {
                    clicked = hit_at(pos).map(|h| (h, true, false));
                } else if resp.drag_started() || resp.clicked() {
                    clicked = hit_at(pos).map(|h| (h, false, shift));
                } else if resp.dragged() {
                    dragged_to = hit_at(pos);
                }
            }
            if resp.secondary_clicked() {
                self.menu_target = resp.interact_pointer_pos().and_then(hit_at);
            }
            resp.context_menu(|ui| self.context_menu(ui));

            for p in pfirst..plast {
                let j = vis[p];
                let hx = origin.x + xs[p + 1];
                let handle = Rect::from_min_max(pos2(hx - 4.0, origin.y + vp.min.y), pos2(hx + 4.0, origin.y + vp.min.y + head_h));
                let hr = ui.interact(handle, ui.id().with(("resize", j)), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                if hr.dragged() {
                    resize = Some((j, hr.drag_delta().x));
                }
                if hr.double_clicked() {
                    resize = Some((j, f32::NAN));
                }
            }
        });

        self.scroll_offset = out.state.offset;
        self.viewport = out.inner_rect.size();

        if let Some((j, dx)) = resize {
            self.widths[j] = if dx.is_nan() { autosize(&view, j) } else { (self.widths[j] + dx).clamp(40.0, 2000.0) };
        }
        if let Some((hit, double, shift)) = clicked {
            match hit {
                Hit::Header(j) if double => {
                    let next = match self.sort_of(j) {
                        None => Some(false),
                        Some(false) => Some(true),
                        Some(true) => None,
                    };
                    self.set_sort(j, next);
                }
                Hit::Header(j) => {
                    self.sel_col = Some(j);
                    if n > 0 {
                        self.anchor = Some((0, j));
                        self.cursor = Some((n - 1, j));
                    }
                }
                Hit::Cell(r, j) if double => {
                    self.cell_popup = Some((format!("{} · row {}", self.names[j], r + 1), self.cell_text(&view, r, j)));
                }
                Hit::Cell(r, j) => {
                    self.cursor = Some((r, j));
                    if !shift || self.anchor.is_none() {
                        self.anchor = self.cursor;
                    }
                    self.sel_col = Some(j);
                }
                Hit::Gutter(r) if !vis.is_empty() => {
                    self.anchor = Some((r, vis[0]));
                    self.cursor = Some((r, vis[vis.len() - 1]));
                }
                Hit::Gutter(_) => {}
            }
        } else if let Some(Hit::Cell(r, j)) = dragged_to {
            self.cursor = Some((r, j));
        }
    }

    fn context_menu(&mut self, ui: &mut Ui) {
        let Some(target) = self.menu_target else { return };
        let view = self.view.clone();
        let j = match target {
            Hit::Header(j) | Hit::Cell(_, j) => j,
            Hit::Gutter(_) => {
                if ui.button("Copy selection").clicked() {
                    self.copy_selection(ui.ctx());
                    ui.close();
                }
                return;
            }
        };
        ui.weak(&self.names[j]);
        if let (Hit::Cell(r, _), Some(view)) = (target, &view) {
            let v = self.cell_text(view, r, j);
            let short: String = v.chars().take(30).collect();
            if ui.button("Copy selection").clicked() {
                self.copy_selection(ui.ctx());
                ui.close();
            }
            if ui.button(format!("Filter: = \"{short}\"")).clicked() {
                self.add_filter(j, if v.is_empty() { Op::Empty } else { Op::Equals }, v.clone());
                ui.close();
            }
            if ui.button(format!("Filter: ≠ \"{short}\"")).clicked() {
                self.add_filter(j, if v.is_empty() { Op::NotEmpty } else { Op::NotEquals }, v.clone());
                ui.close();
            }
            ui.separator();
        }
        if ui.button("Sort ascending").clicked() {
            self.set_sort(j, Some(false));
            ui.close();
        }
        if ui.button("Sort descending").clicked() {
            self.set_sort(j, Some(true));
            ui.close();
        }
        if self.q.sort.is_some() && ui.button("Clear sort").clicked() {
            self.q.sort = None;
            ui.close();
        }
        ui.separator();
        if ui.button("Add filter on column…").clicked() {
            self.add_filter(j, Op::Contains, String::new());
            ui.close();
        }
        if ui.button("Hide column").clicked() {
            self.hidden[j] = true;
            ui.close();
        }
        if ui.button("Autosize column").clicked() {
            if let Some(view) = &view {
                self.widths[j] = autosize(view, j);
            }
            ui.close();
        }
        if ui.button("Copy column name").clicked() {
            ui.ctx().copy_text(self.names[j].clone());
            ui.close();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_messages(&ctx);

        if let Some(p) = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty())) {
            self.open_path(&ctx, p);
        }
        #[cfg(target_os = "macos")]
        if let Some(p) = macos::take_opened() {
            self.open_path(&ctx, p);
        }
        self.keyboard(&ctx);

        let before = self.q.clone();

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        if self.show_filters && self.df.is_some() {
            egui::Panel::top("filters").show(ui, |ui| {
                ui.add_space(4.0);
                self.filter_bar(ui);
                ui.add_space(2.0);
            });
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
        });
        if self.folder.is_some() {
            egui::Panel::left("files").resizable(true).default_size(240.0).min_size(180.0).show(ui, |ui| {
                self.files_panel(ui);
            });
        }
        if self.show_summary && self.df.is_some() {
            egui::Panel::right("summary").resizable(true).default_size(300.0).min_size(260.0).show(ui, |ui| {
                ScrollArea::vertical().show(ui, |ui| self.summary(ui));
            });
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            if self.view.is_some() {
                self.grid(ui);
            } else {
                self.empty_state(ui);
            }
        });

        if let Some((title, value)) = &mut self.cell_popup {
            let mut open = true;
            egui::Window::new(title.as_str())
                .open(&mut open)
                .default_size([520.0, 280.0])
                .show(&ctx, |ui| {
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(value.clone());
                    }
                    ScrollArea::vertical().show(ui, |ui| {
                        ui.add(TextEdit::multiline(value).desired_width(f32::INFINITY).code_editor());
                    });
                });
            if !open {
                self.cell_popup = None;
            }
        }

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let r = ctx.content_rect();
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, "drop".into()));
            p.rect_filled(r, 0.0, Color32::from_black_alpha(160));
            p.text(r.center(), Align2::CENTER_CENTER, "Drop to open", FontId::proportional(28.0), Color32::WHITE);
        }

        if self.q != before {
            self.dirty = Some(Instant::now());
        }
        if let Some(t) = self.dirty {
            if t.elapsed() >= DEBOUNCE {
                self.dirty = None;
                self.run_query(&ctx);
            } else {
                ctx.request_repaint_after(DEBOUNCE - t.elapsed());
            }
        }
    }
}

fn accent(ui: &Ui) -> Color32 {
    ui.visuals().hyperlink_color
}

fn draw_hist(ui: &Ui, rect: Rect, hist: &[u32], color: Color32) {
    ui.painter().rect_filled(rect, CornerRadius::same(4), ui.visuals().faint_bg_color);
    draw_hist_painter(ui.painter(), rect.shrink(4.0), hist, color);
}

fn draw_hist_painter(p: &egui::Painter, rect: Rect, hist: &[u32], color: Color32) {
    let max = *hist.iter().max().unwrap_or(&1).max(&1) as f32;
    let w = rect.width() / hist.len() as f32;
    for (i, &h) in hist.iter().enumerate() {
        if h == 0 {
            continue;
        }
        // sqrt keeps small bins visible next to a dominant one
        let bh = ((h as f32 / max).sqrt() * rect.height()).max(1.5);
        let x = rect.min.x + i as f32 * w;
        p.rect_filled(
            Rect::from_min_max(pos2(x + 0.5, rect.max.y - bh), pos2(x + w - 0.5, rect.max.y)),
            1.0,
            color.gamma_multiply(0.75),
        );
    }
}

/// Stacked bar of the top values' shares, rest in grey.
fn draw_top_bar(p: &egui::Painter, rect: Rect, s: &Stats, rows: f32, color: Color32, rest: Color32) {
    let bar = Rect::from_min_size(pos2(rect.min.x, rect.max.y - 10.0), vec2(rect.width(), 8.0));
    p.rect_filled(bar, 2.0, rest);
    let mut x = bar.min.x;
    for (i, (v, n)) in s.top.iter().take(4).enumerate() {
        let w = bar.width() * *n as f32 / rows;
        p.rect_filled(
            Rect::from_min_size(pos2(x, bar.min.y), vec2((w - 1.0).max(1.0), bar.height())),
            1.0,
            color.gamma_multiply(0.9 - i as f32 * 0.18),
        );
        x += w;
        if i == 0 {
            let label = if v.is_empty() { "(empty)" } else { v.as_str() };
            p.text(
                pos2(rect.min.x, bar.min.y - 2.0),
                Align2::LEFT_BOTTOM,
                format!("{} {:.0}%", label.replace('\n', "¶"), *n as f32 * 100.0 / rows),
                FontId::proportional(10.5),
                color,
            );
        }
    }
}

fn initial_widths(df: &DataFrame) -> Vec<f32> {
    (0..df.width() - 1).map(|j| autosize(df, j)).collect()
}

/// Width from header and the first 200 values.
fn autosize(df: &DataFrame, j: usize) -> f32 {
    let c = &df.columns()[j + 1];
    let longest = (0..df.height().min(200))
        .map(|r| engine::fmt(&c.get(r).unwrap_or(AnyValue::Null)).chars().count())
        .chain([c.name().chars().count() + 2])
        .max()
        .unwrap_or(8);
    (longest as f32 * 7.4 + 20.0).clamp(70.0, 360.0)
}

/// 950, 9.5k, 95k, 2.1M: fits a narrow column header.
fn compact(n: usize) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1e3),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn human_size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.0} KB", bytes as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{ffi, sel, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification};
    use objc2_foundation::{NSArray, NSNotification, NSNotificationCenter, NSURL};
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::sync::{Mutex, OnceLock};

    static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    pub static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

    /// First file Finder asked us to open since the last call.
    pub fn take_opened() -> Option<PathBuf> {
        std::mem::take(&mut *OPENED.lock().ok()?).into_iter().next()
    }

    unsafe extern "C-unwind" fn open_urls(_: &AnyObject, _: Sel, _: &AnyObject, urls: &NSArray<NSURL>) {
        if let Ok(mut opened) = OPENED.lock() {
            opened.extend(urls.iter().filter_map(|u| u.to_file_path()));
        }
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    }

    /// Finder hands double-clicked / "Open With" files to the app delegate's `application:openURLs:`,
    /// before the first frame. winit owns the delegate and doesn't implement it, so graft the method
    /// onto winit's delegate class at willFinishLaunching, which fires before the open event.
    pub fn install_open_handler() {
        let block = block2::RcBlock::new(|_: NonNull<NSNotification>| {
            let Some(mtm) = MainThreadMarker::new() else { return };
            let Some(delegate) = NSApplication::sharedApplication(mtm).delegate() else { return };
            let obj: &AnyObject = delegate.as_ref();
            let cls: *const AnyClass = obj.class();
            unsafe {
                let imp: Imp = std::mem::transmute(
                    open_urls as unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, &NSArray<NSURL>),
                );
                ffi::class_addMethod(cls as *mut AnyClass, sel!(application:openURLs:), imp, c"v@:@@".as_ptr());
            }
        });
        let token = unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
                None,
                &block,
            )
        };
        std::mem::forget(token); // observe for the app's lifetime
    }
}
