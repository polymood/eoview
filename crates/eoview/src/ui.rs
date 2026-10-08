//! User interface: menu bar, toolbar, side panel (layers, display, stretch, compare, inspector), status
//! bar, dock with the views, command palette and keyboard commands.
//!
//! Rules: each frequent action is one click or one key, on the view under the mouse. The menu bar has all
//! commands, with their keys. Each toolbar button shows its key in its tooltip. The command palette
//! (Ctrl+K) finds all commands by name.
use crate::app::{App, Cmp, Dialog, Pane, WORKSPACE_EXT, What};
use crate::icons::{self, Icon};
use crate::lang::{t, tf};
use crate::tools::Tool;
use crate::layer::{self, BINS, CMAPS, Kind, MapLayer, PRESETS, Stretch};
use eo_render::{Compare, CompositeUniforms, View2d};
use egui::{Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, vec2};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, DockState, TabViewer};

/// Display CRS choices: EPSG code (None: pixel space) and name. The layer CRS is also in the list.
pub const SPACES: &[(Option<u32>, &str)] = &[
    (Some(4326), "Geographic (EPSG:4326)"),
    (Some(3857), "Web Mercator (EPSG:3857)"),
    (Some(3413), "North polar stereographic (EPSG:3413)"),
    (Some(3031), "South polar stereographic (EPSG:3031)"),
    (None, "Pixels"),
];

/// Errors on a view (on the data, with a dark background: the same color for all themes).
const RED: Color32 = Color32::from_rgb(255, 110, 110);

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    /// Open products in the view (true: add them as layers).
    Open(bool, What),
    Save,
    Load,
    /// Export the values of the selected layer to a GeoTIFF file.
    Export,
    /// The Figure tab: open or close it. Export the figure to a file of this type (pdf, svg, png, csv).
    Figure,
    FigureExport(&'static str),
    NewView,
    Duplicate,
    CloseView,
    Layout(usize),
    Fit,
    OneToOne,
    Auto,
    /// The display of the selected layer (bands, stretch, color map) for all layers of the view.
    SameDisplay,
    NextCmap,
    Invert,
    Panel,
    Band(i32),
    SetBand(usize),
    Link,
    LinkMode,
    Compare(Cmp),
    SwipeOrient,
    Preset(usize),
    Cmap(usize),
    Space(Option<u32>),
    Palette,
    /// Time steps: forward or back, go to a step, play or pause.
    Step(i32),
    SetStep(usize),
    Play,
    /// Multiply the scale of the view.
    Zoom(f64),
    /// Copy the limits of the view (west, south, east, north) to the clipboard.
    CopyExtent,
    Help,
    Quit,
    /// Globe view or 2D view.
    Globe,
    /// Move the view to its own window, or back to the dock.
    Detach,
    /// Full screen mode of the window of the view: on or off.
    Fullscreen,
    /// Open or close the preferences window.
    Prefs,
    /// Open or close the render window.
    Render,
    /// Start a render of the view with the settings of the render window.
    RenderStart,
    /// Open or close the animate workspace.
    Animate,
    /// Smooth pixels in the view (linear when magnified), or squares.
    Smooth,
    /// Map overlay of the view, on or off: 0 coasts, 1 country borders, 2 country names.
    Overlay(u8),
    /// Preference: full resolution at all zoom levels, on or off.
    FullRes,
    /// The tool of the mouse: on, or off if it is on.
    Tool(Tool),
    /// Pixel grid, coordinate grid of the view: on or off.
    PixelGrid,
    CoordGrid,
    /// Escape: the tool off, the shape that the user draws goes, no selected shape. Else the compare mode off.
    Escape,
    /// Remove the pinned points.
    ClearPins,
    /// Remove the selected shape of the view.
    DeleteShape,
    /// A new layer: the aggregate over time of the selected layer, on the steps of the side panel.
    Aggregate(eo_cache::Agg),
    /// A new layer: the layer math of the side panel.
    Math,
    /// Open or close the Python panel, run its script.
    Python,
    PythonRun,
}

#[derive(Default)]
pub struct Palette {
    pub q: String,
    pub sel: usize,
}

/// Score of a fuzzy match of `q` in `s` (all characters of `q` in order, case ignored). Lower is better:
/// the position of the first match plus the gaps between the matches. None: no match.
pub fn fuzzy(q: &str, s: &str) -> Option<usize> {
    let s: Vec<char> = s.to_lowercase().chars().collect();
    let mut pos = 0;
    let mut score = 0;
    for (k, c) in q.to_lowercase().chars().filter(|c| !c.is_whitespace()).enumerate() {
        let i = s[pos..].iter().position(|&x| x == c)? + pos;
        score += if k == 0 { i } else { i - pos };
        pos = i + 1;
    }
    Some(score)
}

fn mb(b: usize) -> String {
    format!("{:.0} MB", b as f64 / (1 << 20) as f64)
}

/// The command with this name in the command palette (the English name, or the name in the language of
/// the interface).
pub fn command(app: &App, name: &str) -> Option<Cmd> {
    let cur = crate::lang::code();
    crate::lang::set("en");
    let en = commands(app, app.active);
    crate::lang::set(cur);
    en.into_iter().chain(commands(app, app.active)).find(|c| c.0 == name).map(|c| c.2)
}

/// Commands of the palette: name, key, command. Some depend on the selected layer of view `id`.
fn commands(app: &App, id: u32) -> Vec<(String, &'static str, Cmd)> {
    let mut v: Vec<(String, &'static str, Cmd)> = [
        (t("Open files..."), "Ctrl+O", Cmd::Open(false, What::Files)),
        (t("Open folder (SAFE, SEN3, Zarr)..."), "Ctrl+Alt+O", Cmd::Open(false, What::Dirs)),
        (t("Open URL..."), "Ctrl+L", Cmd::Open(false, What::Url)),
        (t("Add layer: files..."), "Ctrl+Shift+O", Cmd::Open(true, What::Files)),
        (t("Add layer: folder (SAFE, SEN3, Zarr)..."), "", Cmd::Open(true, What::Dirs)),
        (t("Add layer: URL..."), "", Cmd::Open(true, What::Url)),
        (t("Save workspace..."), "Ctrl+S", Cmd::Save),
        (t("Export data (GeoTIFF)..."), "", Cmd::Export),
        (t("Figure"), "F8", Cmd::Figure),
        (t("Open workspace..."), "", Cmd::Load),
        (t("New view"), "Ctrl+N", Cmd::NewView),
        (t("Duplicate view"), "Ctrl+D", Cmd::Duplicate),
        (t("Close view"), "Ctrl+W", Cmd::CloseView),
        (t("Detach view to a window, or attach it"), "Ctrl+Shift+D", Cmd::Detach),
        (t("Full screen: on or off"), "F11", Cmd::Fullscreen),
        (t("Layout: 1 view"), "Alt+1", Cmd::Layout(1)),
        (t("Layout: 2 views"), "Alt+2", Cmd::Layout(2)),
        (t("Layout: 2 x 2 views"), "Alt+3", Cmd::Layout(4)),
        (t("Layout: 3 x 3 views"), "Alt+4", Cmd::Layout(9)),
        (t("Fit"), "F", Cmd::Fit),
        (t("Zoom 1:1"), "1", Cmd::OneToOne),
        (t("Automatic stretch"), "A", Cmd::Auto),
        (t("Same display for all layers"), "", Cmd::SameDisplay),
        (t("Next color map"), "C", Cmd::NextCmap),
        (t("Invert color map"), "I", Cmd::Invert),
        (t("Show or hide side panel"), "H", Cmd::Panel),
        (t("Next band"), "]", Cmd::Band(1)),
        (t("Previous band"), "[", Cmd::Band(-1)),
        (t("Link or unlink view"), "L", Cmd::Link),
        (t("Link mode: geographic or pixel"), "Shift+L", Cmd::LinkMode),
        (t("Swipe line: vertical or horizontal"), "V", Cmd::SwipeOrient),
        (t("Open files as a time series..."), "", Cmd::Open(false, What::Series)),
        (t("Time: next step"), ".", Cmd::Step(1)),
        (t("Time: previous step"), ",", Cmd::Step(-1)),
        (t("Time: play or pause"), "Space", Cmd::Play),
        (t("Zoom in"), "+", Cmd::Zoom(1.5)),
        (t("Zoom out"), "-", Cmd::Zoom(1.0 / 1.5)),
        (t("Copy view extent (west, south, east, north)"), "Ctrl+Shift+C", Cmd::CopyExtent),
        (t("3D globe: on or off"), "G", Cmd::Globe),
        (t("Smooth pixels: on or off"), "", Cmd::Smooth),
        (t("Pixel grid: on or off"), "X", Cmd::PixelGrid),
        (t("Coordinate grid: on or off"), "N", Cmd::CoordGrid),
        (t("Remove the pinned points"), "", Cmd::ClearPins),
        (t("Python panel: open or close"), "F7", Cmd::Python),
        (t("Python: run the script"), "", Cmd::PythonRun),
        (t("Coasts: on or off"), "", Cmd::Overlay(0)),
        (t("Country borders: on or off"), "", Cmd::Overlay(1)),
        (t("Country names: on or off"), "", Cmd::Overlay(2)),
        (t("Preferences..."), "Ctrl+,", Cmd::Prefs),
        (t("Animate workspace: on or off"), "F6", Cmd::Animate),
        (t("Render video or frames..."), "Ctrl+R", Cmd::Render),
        (t("Render: start with the settings of the render window"), "", Cmd::RenderStart),
        (t("Full resolution at all zoom levels: on or off"), "", Cmd::FullRes),
        (t("Keys..."), "F1", Cmd::Help),
        (t("Quit"), "Ctrl+Q", Cmd::Quit),
    ]
    .into_iter()
    .map(|(n, k, c)| (n.to_string(), k, c))
    .collect();
    for (i, k) in OPEN_KINDS.iter().enumerate() {
        v.push((tf("Open {}...", &[t(k.0)]), "", Cmd::Open(false, What::Kind(i))));
    }
    for r in &app.recent {
        v.push((tf("Open recent: {}", &[r]), "", Cmd::Open(false, What::Path(r.clone()))));
    }
    for (c, n, k) in Cmp::ALL {
        v.push((tf("Compare: {}", &[t(n)]), k, Cmd::Compare(c)));
    }
    for (tl, n, k) in Tool::ALL {
        v.push((tf("Tool: {}", &[t(n)]), k, Cmd::Tool(tl)));
    }
    v.push((t("Remove the selected shape").into(), "Delete", Cmd::DeleteShape));
    for (a, n) in eo_cache::Agg::ALL {
        v.push((tf("Aggregate over time: {}", &[t(n)]), "", Cmd::Aggregate(a)));
    }
    for (i, (n, _)) in CMAPS.iter().enumerate() {
        v.push((tf("Color map: {}", &[n]), "", Cmd::Cmap(i)));
    }
    for s in SPACES {
        v.push((tf("Display CRS: {}", &[t(s.1)]), "", Cmd::Space(s.0)));
    }
    if let Some(l) = app.pane(id).and_then(|p| p.layers.get(p.sel)) {
        for (i, p) in PRESETS.iter().enumerate() {
            if l.preset_ok(p) {
                v.push((tf("Preset: {}", &[t(p.0)]), "", Cmd::Preset(i)));
            }
        }
        for c in 0..l.chans.len() {
            v.push((tf("Band: {} ({})", &[&l.chans[c].id, &l.chan_label(c)]), "", Cmd::SetBand(c)));
        }
    }
    v
}

/// The keys of the commands, and of the mouse.
pub fn keys_grid(app: &App, ui: &mut egui::Ui) {
    let list = commands(app, app.active);
    egui::Grid::new("keys").striped(true).show(ui, |ui| {
        for (name, key, _) in list.iter().filter(|c| !c.1.is_empty()) {
            ui.monospace(*key);
            ui.label(name);
            ui.end_row();
        }
        for (key, name) in [(t("Drag"), t("Pan")), (t("Mouse wheel, pinch"), t("Zoom at the cursor")), (t("Double-click"), t("Fit")), (t("Drop files"), t("Open in the view under the mouse")), ("Shift + drop", t("Add as layers"))] {
            ui.monospace(t(key));
            ui.label(t(name));
            ui.end_row();
        }
    });
}

/// The dock pass: one tab for each view.
struct Tabs<'a> {
    app: &'a mut App,
    closed: Vec<u32>,
    cmds: Vec<(Cmd, u32)>,
    screen: [u32; 2],
}

impl TabViewer for Tabs<'_> {
    type Tab = u32;

    fn title(&mut self, tab: &mut u32) -> egui::WidgetText {
        match *tab {
            crate::app::PY_TAB => return t("Python").into(),
            crate::app::CHARTS_TAB => return t("Charts").into(),
            crate::app::FIGURE_TAB => return t("Figure").into(),
            _ => {}
        }
        let p = self.app.pane(*tab);
        let t = p.map_or(String::new(), |p| p.title());
        match p.map_or(0, |p| p.link) {
            0 => t.into(),
            g => format!("{t}  [{}]", tf("link {}", &[&g.to_string()])).into(),
        }
    }

    fn id(&mut self, tab: &mut u32) -> egui::Id {
        egui::Id::new(("view", *tab))
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut u32) {
        match *tab {
            crate::app::PY_TAB => self.app.py_tab(ui),
            crate::app::CHARTS_TAB => self.app.charts_tab(ui),
            crate::app::FIGURE_TAB => self.app.figure_tab(ui, &mut self.cmds),
            id => pane_ui(self.app, ui, id, self.screen, &mut self.cmds),
        }
    }

    fn context_menu(&mut self, ui: &mut egui::Ui, tab: &mut u32, _: egui_dock::NodePath) {
        if crate::app::is_view(*tab) {
            view_menu(ui, *tab, false, &self.app.recent, &mut self.cmds);
        }
    }

    fn on_close(&mut self, tab: &mut u32) -> OnCloseResponse {
        match *tab {
            crate::app::PY_TAB => self.app.py.iter_mut().for_each(|p| p.open = false),
            crate::app::CHARTS_TAB => self.app.py.iter_mut().for_each(|p| p.charts_open = false),
            crate::app::FIGURE_TAB => self.app.figure_open = false,
            id => self.closed.push(id),
        }
        OnCloseResponse::Close
    }

    fn scroll_bars(&self, _: &u32) -> [bool; 2] {
        [false, false]
    }
}

/// Product types of the open menus: name, true if the product is a directory, file extensions.
/// The type sets the dialog (files or directories, file filter): the user does not need to know what to select.
pub const OPEN_KINDS: &[(&str, bool, &[&str])] = &[
    ("Sentinel-1 SAFE", true, &[]),
    ("Sentinel-2 SAFE", true, &[]),
    ("Sentinel-3 SEN3", true, &[]),
    ("Zarr store (EOPF, GeoZarr)", true, &[]),
    ("GeoTIFF, COG", false, &["tif", "tiff", "gtiff", "cog"]),
    ("JPEG 2000", false, &["jp2", "j2k", "jpx"]),
    ("NITF (SICD, SIDD)", false, &["ntf", "nitf", "nsf"]),
    ("NetCDF, HDF5", false, &["nc", "nc4", "h5", "hdf5", "he5"]),
];

/// File extensions of the dialog for files of all formats.
const ALL_EXT: &[&str] = &["tif", "tiff", "gtiff", "cog", "jp2", "j2k", "ntf", "nitf", "nsf", "nc", "nc4", "h5", "hdf5", "he5", "xml", "safe", "zarr", "txt", WORKSPACE_EXT];

/// Open menu: files of all formats, directories, a URL, the product types, the recent products.
/// `add`: the products are new layers of the view.
fn open_menu(ui: &mut egui::Ui, add: bool, id: u32, recent: &[String], cmds: &mut Vec<(Cmd, u32)>) {
    let mut item = |ui: &mut egui::Ui, name: &str, key: &str, w: What| {
        if ui.add(egui::Button::new(name).shortcut_text(key)).clicked() {
            cmds.push((Cmd::Open(add, w), id));
            ui.close();
        }
    };
    item(ui, t("Files..."), if add { "Ctrl+Shift+O" } else { "Ctrl+O" }, What::Files);
    item(ui, t("Folder (SAFE, SEN3, Zarr)..."), if add { "" } else { "Ctrl+Alt+O" }, What::Dirs);
    item(ui, "URL...", if add { "" } else { "Ctrl+L" }, What::Url);
    item(ui, t("Files as a time series..."), "", What::Series);
    ui.separator();
    for (i, k) in OPEN_KINDS.iter().enumerate() {
        item(ui, &format!("{}...", t(k.0)), "", What::Kind(i));
    }
    if !recent.is_empty() {
        ui.separator();
        ui.menu_button(t("Recent"), |ui| {
            for r in recent {
                let name = r.trim_end_matches('/').rsplit('/').next().unwrap_or(r);
                if ui.button(name).on_hover_text(r).clicked() {
                    cmds.push((Cmd::Open(add, What::Path(r.clone())), id));
                    ui.close();
                }
            }
        });
    }
}

/// One entry of a menu. The command runs in view `id`.
fn entry(ui: &mut egui::Ui, cmds: &mut Vec<(Cmd, u32)>, id: u32, name: &str, key: &str, c: Cmd) {
    if ui.add(egui::Button::new(name).shortcut_text(key)).clicked() {
        cmds.push((c, id));
        ui.close();
    }
}

/// A menu entry that shows a state (on or off).
fn check(ui: &mut egui::Ui, cmds: &mut Vec<(Cmd, u32)>, id: u32, on: bool, name: &str, key: &str, c: Cmd) {
    if ui.add(egui::Button::selectable(on, name).shortcut_text(key)).clicked() {
        cmds.push((c, id));
        ui.close();
    }
}

/// `out`: the view is detached (it has its own window).
fn view_menu(ui: &mut egui::Ui, id: u32, out: bool, recent: &[String], cmds: &mut Vec<(Cmd, u32)>) {
    ui.menu_button(t("Open"), |ui| open_menu(ui, false, id, recent, cmds));
    ui.menu_button(t("Add layer"), |ui| open_menu(ui, true, id, recent, cmds));
    ui.separator();
    let mut item = |ui: &mut egui::Ui, name: &str, key: &str, c: Cmd| {
        if ui.add(egui::Button::new(name).shortcut_text(key)).clicked() {
            cmds.push((c, id));
            ui.close();
        }
    };
    item(ui, t("Fit"), "F", Cmd::Fit);
    item(ui, t("Zoom 1:1"), "1", Cmd::OneToOne);
    item(ui, t("Automatic stretch"), "A", Cmd::Auto);
    ui.menu_button(t("Compare"), |ui| {
        for (c, n, k) in Cmp::ALL {
            item(ui, t(n), k, Cmd::Compare(c));
        }
    });
    ui.menu_button(t("Display CRS"), |ui| {
        for s in SPACES {
            item(ui, t(s.1), "", Cmd::Space(s.0));
        }
    });
    item(ui, t("Link or unlink"), "L", Cmd::Link);
    ui.separator();
    item(ui, t("New view"), "Ctrl+N", Cmd::NewView);
    item(ui, t("Duplicate view"), "Ctrl+D", Cmd::Duplicate);
    item(ui, t("Close view"), "Ctrl+W", Cmd::CloseView);
    ui.separator();
    item(ui, if out { t("Attach to the main window") } else { t("Detach to a window") }, "Ctrl+Shift+D", Cmd::Detach);
    item(ui, t("Full screen"), "F11", Cmd::Fullscreen);
}

/// Input of one view: pan, zoom, swipe line, cursor. The drawing comes after the link sync (`paint`).
fn pane_ui(app: &mut App, ui: &mut egui::Ui, id: u32, screen: [u32; 2], cmds: &mut Vec<(Cmd, u32)>) {
    let mut rect = ui.available_rect_before_wrap();
    // A view with time steps has its timeline at the bottom.
    if app.pane(id).is_some_and(|p| p.timed().is_some()) && rect.height() > 80.0 {
        let (view, bar) = rect.split_top_bottom_at_y(rect.bottom() - 28.0);
        rect = view;
        timeline(app, ui, id, bar, cmds);
    }
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let ppp = ui.ctx().pixels_per_point();
    let active = app.active == id;
    let tool = app.tool;
    // The events of the tool: a click (display position, double-click), a rectangle (start, end, done).
    let mut click: Option<([f64; 2], bool)> = None;
    let mut drag: Option<([f64; 2], [f64; 2], bool)> = None;
    let (drag_from, shape_drag) = (app.drag_from, app.shape_drag.filter(|d| d.0 == id));
    let mut edit = None;
    let Some(p) = app.pane_mut(id) else { return };
    let vp = egui::epaint::ViewportInPixels::from_points(&rect, ppp, screen);
    p.v.px = Rect::from_min_size(egui::pos2(vp.left_px as f32, vp.top_px as f32), vec2(vp.width_px as f32, vp.height_px as f32));
    p.rect = rect;
    p.painter = Some(ui.painter().clone());
    if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
        app.active = id;
    }
    if resp.contains_pointer() {
        app.hovered = Some(id);
    }
    let out = app.floating.contains(&id);
    resp.context_menu(|ui| view_menu(ui, id, out, &app.recent, cmds));
    let p = app.pane_mut(id).unwrap();
    if p.layers.is_empty() {
        let c = rect.center();
        if app.opening(id) {
            ui.painter().text(c, Align2::CENTER_CENTER, t("Opening..."), FontId::proportional(18.0), ui.visuals().weak_text_color());
        } else {
            ui.painter().text(c - vec2(0.0, 24.0), Align2::CENTER_CENTER, t("Drop files here"), FontId::proportional(18.0), ui.visuals().weak_text_color());
            let b = ui.put(Rect::from_center_size(c + vec2(0.0, 12.0), vec2(120.0, 26.0)), egui::Button::new(t("Open...  Ctrl+O")));
            if b.clicked() {
                cmds.push((Cmd::Open(false, What::Files), id));
            }
        }
    }
    if active && app.panes.len() > 1 {
        ui.painter().rect_stroke(rect.shrink(0.5), 0.0, Stroke::new(1.0, ui.visuals().hyperlink_color.gamma_multiply(0.6)), egui::StrokeKind::Inside);
    }
    let p = app.pane_mut(id).unwrap();
    // Swipe line: drag it when the pointer is near it.
    let swipe = p.cmp == Cmp::Swipe && p.specs.len() >= 2;
    let line = |p: &Pane| if p.vertical { rect.left() + p.swipe * rect.width() } else { rect.top() + p.swipe * rect.height() };
    let near = |p: &Pane, pos: Pos2| (if p.vertical { pos.x } else { pos.y } - line(p)).abs() < 8.0;
    if swipe && let Some(h) = resp.hover_pos() && (near(p, h) || p.swipe_drag) {
        ui.ctx().set_cursor_icon(if p.vertical { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
    }
    if resp.drag_started() && swipe && resp.interact_pointer_pos().is_some_and(|q| near(p, q)) {
        p.swipe_drag = true;
    }
    if resp.drag_stopped() {
        p.swipe_drag = false;
    }
    let disp = |p: &Pane, q: Pos2| Some(p.v.to_display([(q.x * ppp - p.v.px.min.x) as f64, (q.y * ppp - p.v.px.min.y) as f64])).filter(|c| c[0].is_finite());
    if p.swipe_drag {
        if let Some(q) = resp.interact_pointer_pos() {
            p.swipe = if p.vertical { (q.x - rect.left()) / rect.width() } else { (q.y - rect.top()) / rect.height() }.clamp(0.0, 1.0);
        }
    } else if tool == Tool::Region && (resp.dragged() || resp.drag_stopped()) {
        // The region tool: a drag makes a rectangle.
        if let Some(c) = resp.interact_pointer_pos().and_then(|q| disp(p, q)) {
            let start = || ui.input(|i| i.pointer.press_origin()).and_then(|q| disp(p, q));
            let from = if resp.drag_started() { start().unwrap_or(c) } else { drag_from.unwrap_or(c) };
            drag = Some((from, c, resp.drag_stopped()));
        }
    } else if let Some((_, k, v)) = shape_drag.filter(|_| resp.dragged() || resp.drag_stopped()) {
        // A drag of a shape or of one of its points.
        let d = resp.drag_delta() * ppp;
        let kx = if p.v.globe { p.v.center[1].to_radians().cos().max(0.05) } else { 1.0 };
        if let Some(c) = resp.interact_pointer_pos().and_then(|q| disp(p, q)) {
            edit = Some((k, v, c, [d.x as f64 / (p.v.scale * kx), -d.y as f64 / p.v.scale]));
        }
    } else if resp.dragged() {
        let d = resp.drag_delta() * ppp;
        // Globe: a degree of longitude is shorter away from the equator.
        let kx = if p.v.globe { p.v.center[1].to_radians().cos().max(0.05) } else { 1.0 };
        p.v.center[0] -= d.x as f64 / (p.v.scale * kx);
        p.v.center[1] += d.y as f64 / p.v.scale;
        p.v.clamp_globe();
        p.moved = true;
    }
    if tool != Tool::None {
        if let Some(c) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.double_clicked()).and_then(|q| disp(p, q)) {
            click = Some((c, resp.double_clicked()));
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    } else {
        // No tool: a click selects a shape, a drag on a shape moves it or its point.
        let hit = |p: &Pane, q: Option<Pos2>| q.and_then(|q| crate::tools::shape_at(p, q));
        if resp.clicked() {
            p.sel_shape = hit(p, resp.interact_pointer_pos()).map(|h| h.0);
        }
        if let Some((k, v)) = hit(p, resp.hover_pos()) {
            ui.ctx().set_cursor_icon(if v.is_some() || p.sel_shape == Some(k) { egui::CursorIcon::Move } else { egui::CursorIcon::PointingHand });
        }
        if resp.drag_started()
            && let Some((k, v)) = hit(p, ui.input(|i| i.pointer.press_origin()))
            && (p.sel_shape == Some(k) || v.is_some())
        {
            p.sel_shape = Some(k);
            app.shape_drag = Some((id, k, v));
        } else if resp.double_clicked() && hit(p, resp.interact_pointer_pos()).is_none() {
            p.v.fit = true;
            p.fit_user = true;
        }
    }
    let p = app.pane_mut(id).unwrap();
    p.v.cursor = None;
    if let Some(h) = resp.hover_pos() {
        let q = [(h.x * ppp - p.v.px.min.x) as f64, (h.y * ppp - p.v.px.min.y) as f64];
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let f = pinch as f64 * 2f64.powf(scroll as f64 / 200.0);
        if f != 1.0 {
            let before = p.v.to_display(q);
            p.v.scale = (p.v.scale * f).clamp(1e-12, 1e12);
            p.v.clamp_globe();
            let after = p.v.to_display(q);
            // Globe: the cursor can be off the globe.
            if before[0].is_finite() && after[0].is_finite() {
                p.v.center[0] += before[0] - after[0];
                p.v.center[1] += before[1] - after[1];
                p.v.clamp_globe();
            }
            p.moved = true;
        }
        p.v.cursor = Some(p.v.to_display(q)).filter(|c| c[0].is_finite());
    }
    if let Some((c, double)) = click {
        app.tool_click(id, c, double);
    }
    if let Some((a, b, done)) = drag {
        let first = app.drag_from.is_none();
        app.drag_from = (!done).then_some(a);
        app.tool_rect(id, a, b, done, first);
    }
    if let Some((k, v, c, d)) = edit {
        app.shape_drag(id, k, v, c, d);
    }
    if resp.drag_stopped() {
        app.shape_drag = None;
    }
    let p = app.pane_mut(id).unwrap();
    // Link badge: one click links or unlinks the view.
    if !p.layers.is_empty() {
        let (txt, tip) = if p.link > 0 { (tf("link {}", &[&p.link.to_string()]), t("Linked: this view pans and zooms with the other linked views. Click to unlink (L)")) } else { (t("unlinked").to_string(), t("Click to link this view (L)")) };
        let r = Rect::from_min_size(rect.right_top() + vec2(-74.0, 6.0), vec2(68.0, 20.0));
        let b = egui::Button::new(egui::RichText::new(txt).small()).fill(if p.link > 0 { ui.visuals().hyperlink_color.gamma_multiply(0.35) } else { Color32::from_black_alpha(140) });
        if ui.put(r, b).on_hover_text(tip).clicked() {
            cmds.push((Cmd::Link, id));
        }
    }
}

/// Timeline of a view: step back, play or pause, step forward, rate, the steps with their buffer state
/// (click or drag to go to a step), the number and the time of the selected step.
fn timeline(app: &mut App, ui: &mut egui::Ui, id: u32, bar: Rect, cmds: &mut Vec<(Cmd, u32)>) {
    let Some(p) = app.pane_mut(id) else { return };
    let Some(l) = p.timed() else { return };
    let (n, step) = (l.steps.len(), l.step);
    // One cell for each step, or one cell for each screen pixel if the layer has more steps (a data cube
    // can have a million steps): the cell shows the first of its steps.
    let cells = n.min(bar.width().max(1.0) as usize).max(1);
    let states: Vec<u8> = (0..cells).map(|c| p.buffer(l, c * n / cells)).collect();
    let text = format!("{} / {n}   {}{}", step + 1, l.step_label(step), if l.shown != step { format!("   {}", t("loading")) } else { String::new() });
    let play = p.play;
    let vis = ui.visuals().clone();
    ui.painter().rect_filled(bar, 0.0, vis.extreme_bg_color);
    let mut x = bar.left() + 4.0;
    let mut slot = |w: f32| {
        let r = Rect::from_min_size(egui::pos2(x, bar.top() + 3.0), vec2(w, bar.height() - 6.0));
        x += w + 4.0;
        r
    };
    if icons::put(ui, slot(24.0), Icon::StepBack, false).on_hover_text(t("Step back (,)")).clicked() {
        cmds.push((Cmd::Step(-1), id));
    }
    if icons::put(ui, slot(28.0), if play { Icon::Pause } else { Icon::Play }, play).on_hover_text(t("Play or pause (Space). The playback waits for a step that is not ready: it does not skip steps")).clicked() {
        cmds.push((Cmd::Play, id));
    }
    if icons::put(ui, slot(24.0), Icon::StepForward, false).on_hover_text(t("Step forward (.)")).clicked() {
        cmds.push((Cmd::Step(1), id));
    }
    ui.put(slot(64.0), egui::DragValue::new(&mut p.fps).range(0.2..=30.0).speed(0.1).suffix(" /s")).on_hover_text(t("Playback rate: steps for each second"));
    let g = ui.painter().layout_no_wrap(text, FontId::proportional(12.0), vis.text_color());
    let track = Rect::from_min_max(egui::pos2(slot(0.0).left() + 4.0, bar.top() + 7.0), egui::pos2(bar.right() - g.size().x - 16.0, bar.bottom() - 7.0));
    ui.painter().galley(egui::pos2(track.right() + 8.0, bar.center().y - g.size().y / 2.0), g, vis.text_color());
    if track.width() < 20.0 {
        return;
    }
    let (w, sw) = (track.width() / cells as f32, track.width() / n as f32);
    for (s, st) in states.iter().enumerate() {
        let c = [vis.extreme_bg_color.lerp_to_gamma(vis.text_color(), 0.18), vis.extreme_bg_color.lerp_to_gamma(vis.text_color(), 0.45), vis.warn_fg_color, Color32::from_rgb(80, 190, 110)][*st as usize];
        let r = Rect::from_min_size(egui::pos2(track.left() + s as f32 * w, track.top()), vec2((w - 1.0).max(1.0), track.height()));
        ui.painter().rect_filled(r, 1.0, c);
    }
    let cur = Rect::from_min_size(egui::pos2(track.left() + step as f32 * sw, track.top()), vec2(sw.max(2.0), track.height()));
    ui.painter().rect_stroke(cur.expand(2.0), 1.0, Stroke::new(2.0, vis.strong_text_color()), egui::StrokeKind::Outside);
    let resp = ui.interact(track.expand2(vec2(2.0, 7.0)), ui.id().with(("timeline", id)), Sense::click_and_drag());
    let at = |q: Pos2| (((q.x - track.left()) / sw).max(0.0) as usize).min(n - 1);
    if let Some(q) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.dragged())
        && at(q) != step
    {
        cmds.push((Cmd::SetStep(at(q)), id));
    }
    if let Some(s) = resp.hover_pos().map(at) {
        let state = t(["not open", "open", "tiles load", "ready"][states[s * cells / n] as usize]);
        let label = app.pane(id).and_then(|p| p.timed()).map_or(String::new(), |l| l.step_label(s));
        resp.on_hover_text_at_pointer(format!("{} / {n}   {label}\n{state}\n{}", s + 1, t("Green: ready for this view. Amber: tiles load. Gray: open. Dark: not open.")));
    }
}

/// Histogram of a channel with its stretch limits. Drag near a limit to move it. Double-click: automatic.
fn histogram(ui: &mut egui::Ui, h: &(Vec<u32>, f32, f32), st: &mut Stretch, color: Color32) -> (bool, bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click_and_drag());
    let pt = ui.painter_at(rect);
    pt.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let (bins, lo, hi) = (&h.0, h.1, h.2);
    let max = bins.iter().copied().max().unwrap_or(1).max(1) as f32;
    let x = |v: f32| rect.left() + ((v - lo) / (hi - lo)).clamp(0.0, 1.0) * rect.width();
    let v = |x: f32| lo + (x - rect.left()) / rect.width() * (hi - lo);
    let bw = rect.width() / BINS as f32;
    for (i, &b) in bins.iter().enumerate() {
        let hh = (b as f32 / max).sqrt() * (rect.height() - 4.0);
        let x0 = rect.left() + i as f32 * bw;
        pt.rect_filled(Rect::from_min_max(egui::pos2(x0, rect.bottom() - hh), egui::pos2(x0 + bw, rect.bottom())), 0.0, color.gamma_multiply(0.7));
    }
    let (xl, xh) = (x(st.lo), x(st.hi));
    let shade = Color32::from_black_alpha(130);
    pt.rect_filled(Rect::from_x_y_ranges(rect.left()..=xl, rect.y_range()), 0.0, shade);
    pt.rect_filled(Rect::from_x_y_ranges(xh..=rect.right(), rect.y_range()), 0.0, shade);
    for xx in [xl, xh] {
        pt.line_segment([egui::pos2(xx, rect.top()), egui::pos2(xx, rect.bottom())], Stroke::new(2.0, Color32::WHITE));
    }
    let id = resp.id.with("handle");
    let mut changed = false;
    if resp.drag_started()
        && let Some(q) = resp.interact_pointer_pos()
    {
        // 0: min, 1: max, 2: both (inside, away from the limits).
        let k: u8 = if (q.x - xl).abs() < 10.0 { 0 } else if (q.x - xh).abs() < 10.0 { 1 } else if q.x > xl && q.x < xh { 2 } else if q.x < xl { 0 } else { 1 };
        ui.memory_mut(|m| m.data.insert_temp(id, k));
    }
    if resp.dragged()
        && let Some(q) = resp.interact_pointer_pos()
    {
        match ui.memory(|m| m.data.get_temp::<u8>(id)).unwrap_or(0) {
            0 => st.lo = v(q.x).min(st.hi),
            1 => st.hi = v(q.x).max(st.lo),
            _ => {
                let d = resp.drag_delta().x / rect.width() * (hi - lo);
                st.lo += d;
                st.hi += d;
            }
        }
        changed = true;
    }
    let resp = resp.on_hover_text(t("Drag the limits, or drag between them to move both. Double-click: automatic stretch (A)"));
    (changed, resp.double_clicked())
}

/// A click in the product tree.
enum Pick {
    /// Show a variable or a band as data (one band with the color map).
    Chan(usize),
    /// Add the name of a band to the band math expression.
    Insert(usize),
    /// Channel k (red, green or blue) of the RGB composite gets a band.
    Rgb(usize, usize),
    /// The red, green and blue bands of a color image.
    Color([usize; 3]),
}

/// Product tree of a layer: the groups and the variables of the product, with a filter.
fn contents_ui(ui: &mut egui::Ui, l: &mut MapLayer) -> Option<Pick> {
    let mut pick = None;
    ui.horizontal(|ui| {
        ui.strong(t("Product"));
        ui.add(egui::TextEdit::singleline(&mut l.filter).hint_text(tf("Filter {} variables", &[&l.chans.len().to_string()])).desired_width(f32::INFINITY));
    });
    let l = &*l;
    egui::ScrollArea::vertical().id_salt(("contents", l.uid)).max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
        let f = l.filter.trim().to_lowercase();
        if f.is_empty() {
            group_ui(ui, l, &l.contents, "", &mut pick);
        } else {
            // With a filter: a flat list of the names that contain the text.
            for c in 0..l.chans.len() {
                let name = l.chan_label(c);
                if name.to_lowercase().contains(&f) || l.chans[c].id.to_lowercase().contains(&f) {
                    leaf_ui(ui, l, c, &name, &mut pick);
                }
            }
        }
    });
    pick
}

fn group_ui(ui: &mut egui::Ui, l: &MapLayer, g: &layer::Group, path: &str, pick: &mut Option<Pick>) {
    for sub in &g.groups {
        let p = format!("{path}/{}", sub.name);
        // Open at the start: the groups of the bands in use, and all groups of a small product.
        let open = l.chans.len() <= 16 || sub.has_any(&l.used);
        egui::CollapsingHeader::new(format!("{}  ({})", sub.name, sub.count())).id_salt((l.uid, &p)).default_open(open).show(ui, |ui| {
            if let Some(c) = sub.color {
                let on = l.kind == Kind::Rgb && (0..3).all(|k| l.rgb[k].trim() == l.chans[c[k]].id);
                if ui.selectable_label(on, t("Color image")).on_hover_text(t("Show the red, green and blue bands as they are")).clicked() {
                    *pick = Some(Pick::Color(c));
                }
            }
            group_ui(ui, l, sub, &p, pick);
        });
    }
    for &c in &g.chans {
        leaf_ui(ui, l, c, &l.chan_leaf(c), pick);
    }
}

fn leaf_ui(ui: &mut egui::Ui, l: &MapLayer, c: usize, label: &str, pick: &mut Option<Pick>) {
    let id = &l.chans[c].id;
    ui.horizontal(|ui| {
        // The name shows the variable as data, with the color map: not all variables are colors.
        let on = l.kind == Kind::Band && l.band == c;
        let tip = format!("{}\n{}", l.chan_label(c), tf("Show as one band with the color map. Name in expressions: {}", &[id]));
        if ui.selectable_label(on, label).on_hover_text(tip).clicked() {
            *pick = Some(Pick::Chan(c));
        }
        // To make a composite: the channel buttons (RGB mode) or the insert button (band math mode).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match l.kind {
            Kind::Rgb => {
                for k in [2, 1, 0] {
                    let b = egui::Button::selectable(l.rgb[k].trim() == id, ["R", "G", "B"][k]).small();
                    if ui.add(b).on_hover_text(tf("Use as the {} channel of the RGB composite", &[t(["red", "green", "blue"][k])])).clicked() {
                        *pick = Some(Pick::Rgb(k, c));
                    }
                }
            }
            Kind::Expr => {
                let b = egui::Button::selectable(l.used.contains(&c), "+").small();
                if ui.add(b).on_hover_text(tf("Add {} to the expression", &[id])).clicked() {
                    *pick = Some(Pick::Insert(c));
                }
            }
            Kind::Wind => {
                for k in [1, 0] {
                    let b = egui::Button::selectable(l.rgb[k].trim() == id, ["U", "V"][k]).small();
                    if ui.add(b).on_hover_text(tf("Use as the component to the {} of the wind", &[t(["east", "north"][k])])).clicked() {
                        *pick = Some(Pick::Rgb(k, c));
                    }
                }
            }
            Kind::Band => {}
        });
    });
}

/// Color map swatches: one click selects.
fn swatches(ui: &mut egui::Ui, cur: usize, invert: bool) -> Option<usize> {
    let mut out = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for (i, (n, c)) in CMAPS.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(44.0, 14.0), Sense::click());
            gradient(ui, r, &c.iter().map(|&c| layer::hex(c)).collect::<Vec<_>>(), invert);
            if i == cur {
                ui.painter().rect_stroke(r.expand(1.5), 2.0, Stroke::new(2.0, ui.visuals().hyperlink_color), egui::StrokeKind::Outside);
            }
            if resp.on_hover_text(*n).clicked() {
                out = Some(i);
            }
        }
    });
    out
}

fn gradient(ui: &egui::Ui, r: Rect, stops: &[[u8; 3]], invert: bool) {
    let l = layer::lut(stops);
    let n = 32;
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let j = ((if invert { 1.0 - t } else { t }) * 255.0) as usize;
        let x0 = r.left() + r.width() * i as f32 / n as f32;
        ui.painter().rect_filled(Rect::from_x_y_ranges(x0..=x0 + r.width() / n as f32 + 0.5, r.y_range()), 0.0, Color32::from_rgb(l[j][0], l[j][1], l[j][2]));
    }
}

impl App {
    /// The whole interface of one frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // The detached views are not in this window: their windows set their state (`detached_ui`). The
        // scene of the animate workspace is in the preview.
        let (floating, scene) = (&self.floating, self.scene_pane());
        if self.hovered.is_some_and(|h| !floating.contains(&h)) {
            self.hovered = None;
        }
        self.panes.iter_mut().filter(|p| !floating.contains(&p.id) && Some(p.id) != scene).for_each(|p| p.painter = None);
        let mut cmds: Vec<(Cmd, u32)> = vec![];

        egui::Panel::top("menu").show(ui, |ui| self.menus(ui, &mut cmds));
        if let Some(scene) = scene {
            self.active = scene;
            self.animate_ui(ui, scene, &mut cmds);
        } else {
            egui::Panel::top("bar").show(ui, |ui| self.toolbar(ui, &mut cmds));
            egui::Panel::bottom("status").show(ui, |ui| self.status(ui));
            if self.panel {
                egui::Panel::left("side").resizable(true).default_size(330.0).show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.side(ui, &mut cmds));
                });
            }
            let screen = self.win.as_ref().map_or([1, 1], |w| w.size());
            // The tabs of the Python editor, of the charts and of the figure show when they are open.
            let (py, charts) = self.py.as_ref().map_or((false, false), |p| (p.open, p.charts_open));
            self.show_tab(crate::app::PY_TAB, py);
            self.show_tab(crate::app::CHARTS_TAB, charts);
            self.show_tab(crate::app::FIGURE_TAB, self.figure_open);
            let mut dock = std::mem::replace(&mut self.dock, DockState::new(vec![]));
            let mut tabs = Tabs { app: self, closed: vec![], cmds: vec![], screen };
            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                let mut style = egui_dock::Style::from_egui(ui.style().as_ref());
                style.tab_bar.height = 22.0;
                DockArea::new(&mut dock).style(style).show_leaf_collapse_buttons(false).show_leaf_close_all_buttons(false).show_inside(ui, &mut tabs);
            });
            let (closed, more) = (tabs.closed, tabs.cmds);
            self.dock = dock;
            cmds.extend(more);
            for id in closed {
                self.closed(id);
            }
            // After the animate workspace, the scene view has the same area as in the workspace.
            if let Some((id, center, width)) = self.cam_restore
                && let Some(p) = self.pane_mut(id).filter(|p| p.painter.is_some() && p.v.px.width() > 1.0)
            {
                (p.v.center, p.v.scale) = (center, p.v.px.width() as f64 / width);
                self.cam_restore = None;
            }
        }

        // Dropped files: one file replaces the layers of the view under the mouse, more files go in more
        // views. With Shift, the files are new layers of the view.
        let (dropped, shift, hovering) = ctx.input(|i| {
            let d: Vec<String> = i.raw.dropped_files.iter().map(|f| f.path().to_string_lossy().into_owned()).collect();
            (d, i.modifiers.shift, !i.raw.hovered_files.is_empty())
        });
        if !dropped.is_empty() {
            self.open_many(self.target(), dropped, shift);
        }
        if hovering {
            let r = ctx.content_rect();
            ui.painter().text(r.center(), Align2::CENTER_CENTER, t("Drop: open in the view under the mouse\nShift + drop: add as layers"), FontId::proportional(20.0), Color32::WHITE);
        }

        if !ctx.egui_wants_keyboard_input() {
            self.keys(&ctx, &mut cmds);
        }
        for (c, id) in cmds {
            self.run(c, id);
        }
        if self.palette.is_some() {
            self.palette_ui(&ctx);
        }
        self.url_ui(&ctx);
        self.help_ui(&ctx);
        self.prefs_ui(&ctx);
        self.py_frame();
        // `eoview --python FILE`: the script runs when the layers of the view show.
        let shown = self.pane(self.active).is_some_and(|p| !p.layers.is_empty() && p.layers.iter().all(|l| !l.inputs.is_empty()) && p.v.inputs.iter().all(|i| i.warp.is_some()));
        if shown && self.opens_pending() == 0 && let Some(f) = self.cli_python.take() {
            self.py_open(&std::path::absolute(&f).unwrap_or(f.into()));
            if self.error.is_none() {
                self.py_run();
            }
        }
        self.render_ui(&ctx);

        if let Some(b) = &mut self.bench {
            if let Some(p) = self.panes.first_mut()
                && b.drive(&mut p.v)
            {
                p.moved = true;
            }
        }
        self.crosshair();
        if let Some(dt) = self.play(ctx.input(|i| i.time)) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(dt.max(0.0)));
        }
        self.sync();
        self.paint(&ctx, None);
        // The detached views follow the links, the time cursor and the crosshair of this frame.
        self.wins.iter().for_each(|d| d.window.request_redraw());
    }

    /// One frame of a render: view `id` on the full frame (`screen` pixels), without the controls, and the
    /// time of its step at the bottom left if `stamp` is true.
    /// `legend`: the text of the legend (empty: the name of the layer and its unit).
    pub fn export_ui(&mut self, ui: &mut egui::Ui, id: u32, screen: [u32; 2], stamp: bool, legend_text: &str) {
        let ctx = ui.ctx().clone();
        let rect = ctx.content_rect();
        let ppp = ctx.pixels_per_point();
        let Some(p) = self.pane_mut(id) else { return };
        let vp = egui::epaint::ViewportInPixels::from_points(&rect, ppp, screen);
        p.v.px = Rect::from_min_size(egui::pos2(vp.left_px as f32, vp.top_px as f32), vec2(vp.width_px as f32, vp.height_px as f32));
        p.rect = rect;
        p.painter = Some(ui.painter().clone());
        p.v.cursor = None;
        // The time of the frame: between the times of two steps for a frame that is a blend of them.
        let label = p.timed().filter(|_| stamp).map(|l| {
            let (a, b) = (l.steps[l.shown].t, l.steps[(l.shown + l.stride.max(1)) % l.steps.len()].t);
            if l.blend && p.tmix > 0.0 && a.is_finite() && b.is_finite() { eo_core::time::text(a + (b - a) * p.tmix as f64) } else { l.step_label(l.shown) }
        });
        // The legend of the frame: the color map of the top data layer, with its limits and its unit.
        let legend = p.layers.iter().rev().find(|l| l.visible && l.kind != Kind::Rgb && !l.inputs.is_empty()).filter(|_| stamp).map(|l| {
            let unit = l.inputs[0].var().units.clone();
            let name = if l.kind == Kind::Wind { t("Wind speed").to_string() } else { l.comp_name() };
            (l.stops.clone(), l.invert, l.st[0].lo, l.st[0].hi, if unit.is_empty() { name } else { format!("{name} ({unit})") })
        });
        let legend = legend.map(|(s, i, lo, hi, name)| (s, i, lo, hi, if legend_text.is_empty() { name } else { legend_text.to_string() }));
        self.sync();
        self.paint(&ctx, Some(id));
        if let Some((stops, invert, lo, hi, name)) = legend {
            let pt = ui.painter();
            let bar = Rect::from_min_size(rect.right_bottom() + vec2(-16.0 - 12.0 - 260.0, -16.0 - 44.0), vec2(260.0, 12.0));
            pt.rect_filled(bar.expand2(vec2(12.0, 0.0)).with_min_y(bar.top() - 24.0).with_max_y(bar.bottom() + 24.0), 4.0, Color32::from_black_alpha(170));
            gradient(ui, bar, &stops, invert);
            let text = |pos: Pos2, align: Align2, s: String, size: f32| {
                pt.text(pos, align, s, FontId::proportional(size), Color32::WHITE);
            };
            text(bar.left_top() + vec2(0.0, -5.0), Align2::LEFT_BOTTOM, name, 13.0);
            text(bar.left_bottom() + vec2(0.0, 4.0), Align2::LEFT_TOP, format!("{lo:.4}").trim_end_matches('0').trim_end_matches('.').to_string(), 12.0);
            text(bar.right_bottom() + vec2(0.0, 4.0), Align2::RIGHT_TOP, format!("{hi:.4}").trim_end_matches('0').trim_end_matches('.').to_string(), 12.0);
        }
        if let Some(text) = label {
            let pt = ui.painter();
            let g = pt.layout_no_wrap(text, FontId::monospace(18.0), Color32::WHITE);
            // Top left: the scale bar is at the bottom left.
            let r = Rect::from_min_size(rect.left_top() + vec2(16.0, 16.0), g.size() + vec2(20.0, 12.0));
            pt.rect_filled(r, 4.0, Color32::from_black_alpha(170));
            pt.galley(r.min + vec2(10.0, 6.0), g, Color32::WHITE);
        }
    }

    /// Crosshair: the cursor of the view under the mouse, in link group terms.
    fn crosshair(&mut self) {
        self.cursor = None;
        if let Some(i) = self.hovered.and_then(|h| self.panes.iter().position(|p| p.id == h))
            && self.panes[i].link != 0
            && let Some(c) = self.panes[i].v.cursor
        {
            let (g, id) = (self.panes[i].link, self.panes[i].id);
            self.cursor = self.cam(i, Some(c)).map(|cam| (g, id, cam));
        }
    }

    /// The interface of the window of a detached view (`wins[k]`): the view on the full window. H shows or
    /// hides the side panel in this window. The keys of this window are commands for its view.
    pub fn detached_ui(&mut self, ui: &mut egui::Ui, k: usize, screen: [u32; 2]) {
        let ctx = ui.ctx().clone();
        let id = self.wins[k].pane;
        if self.hovered == Some(id) {
            self.hovered = None;
        }
        let Some(p) = self.pane_mut(id) else { return };
        p.painter = None;
        let mut cmds: Vec<(Cmd, u32)> = vec![];
        // Escape goes out of the full screen mode.
        if self.wins[k].window.fullscreen().is_some() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.wins[k].window.set_fullscreen(None);
        }
        if self.wins[k].panel {
            // The side panel shows the active view. In this window, it shows the view of the window.
            let active = std::mem::replace(&mut self.active, id);
            egui::Panel::left("side").resizable(true).default_size(330.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.side(ui, &mut cmds));
            });
            self.active = active;
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| pane_ui(self, ui, id, screen, &mut cmds));
        let (dropped, shift) = ctx.input(|i| {
            let d: Vec<String> = i.raw.dropped_files.iter().map(|f| f.path().to_string_lossy().into_owned()).collect();
            (d, i.modifiers.shift)
        });
        if !dropped.is_empty() {
            self.open_many(id, dropped, shift);
        }
        if !ctx.egui_wants_keyboard_input() {
            let n = cmds.len();
            self.keys(&ctx, &mut cmds);
            cmds[n..].iter_mut().for_each(|c| c.1 = id);
        }
        let mut touched = !cmds.is_empty();
        for (c, i) in cmds {
            match c {
                Cmd::Panel => self.wins[k].panel ^= true,
                // The dialogs of these commands are in the main window.
                Cmd::Palette | Cmd::Help | Cmd::Prefs | Cmd::Render | Cmd::Open(..) | Cmd::Save | Cmd::Load | Cmd::Export => {
                    self.run(c, i);
                    if let Some(w) = &self.win {
                        w.window.focus_window();
                    }
                }
                _ => self.run(c, i),
            }
        }
        self.crosshair();
        touched |= self.pane(id).is_some_and(|p| p.moved || p.v.fit);
        self.sync();
        self.paint(&ctx, Some(id));
        // The main window shows the result: linked views, side panel, dialogs.
        if touched && let Some(w) = &self.win {
            w.window.request_redraw();
        }
        let title = self.pane(id).map_or(String::new(), |p| match p.link {
            0 => p.title(),
            g => format!("{}  [link {g}]", p.title()),
        });
        let d = &mut self.wins[k];
        if d.title != title {
            d.window.set_title(&format!("{title} - {}", crate::APP));
            d.title = title;
        }
    }

    fn keys(&mut self, ctx: &egui::Context, cmds: &mut Vec<(Cmd, u32)>) {
        let t = self.target();
        let (cmd, sh, alt, none) = (Modifiers::COMMAND, Modifiers::COMMAND | Modifiers::SHIFT, Modifiers::ALT, Modifiers::NONE);
        let table: &[(Modifiers, Key, Cmd)] = &[
            (Modifiers::COMMAND | Modifiers::ALT, Key::O, Cmd::Open(false, What::Dirs)),
            (sh, Key::O, Cmd::Open(true, What::Files)),
            (cmd, Key::O, Cmd::Open(false, What::Files)),
            (cmd, Key::L, Cmd::Open(false, What::Url)),
            (cmd, Key::S, Cmd::Save),
            (cmd, Key::K, Cmd::Palette),
            (cmd, Key::Comma, Cmd::Prefs),
            (cmd, Key::R, Cmd::Render),
            (none, Key::F6, Cmd::Animate),
            (none, Key::F7, Cmd::Python),
            (none, Key::F8, Cmd::Figure),
            (cmd, Key::Q, Cmd::Quit),
            (sh, Key::C, Cmd::CopyExtent),
            (none, Key::F1, Cmd::Help),
            (none, Key::G, Cmd::Globe),
            (none, Key::Plus, Cmd::Zoom(1.5)),
            (none, Key::Equals, Cmd::Zoom(1.5)),
            (none, Key::Minus, Cmd::Zoom(1.0 / 1.5)),
            (cmd, Key::N, Cmd::NewView),
            (sh, Key::D, Cmd::Detach),
            (none, Key::F11, Cmd::Fullscreen),
            (cmd, Key::D, Cmd::Duplicate),
            (cmd, Key::W, Cmd::CloseView),
            (alt, Key::Num1, Cmd::Layout(1)),
            (alt, Key::Num2, Cmd::Layout(2)),
            (alt, Key::Num3, Cmd::Layout(4)),
            (alt, Key::Num4, Cmd::Layout(9)),
            (none, Key::F, Cmd::Fit),
            (none, Key::Num1, Cmd::OneToOne),
            (none, Key::A, Cmd::Auto),
            (none, Key::C, Cmd::NextCmap),
            (none, Key::I, Cmd::Invert),
            (none, Key::H, Cmd::Panel),
            (none, Key::CloseBracket, Cmd::Band(1)),
            (none, Key::OpenBracket, Cmd::Band(-1)),
            (Modifiers::SHIFT, Key::L, Cmd::LinkMode),
            (none, Key::L, Cmd::Link),
            (none, Key::W, Cmd::Compare(Cmp::Swipe)),
            (none, Key::B, Cmd::Compare(Cmp::Blend)),
            (none, Key::D, Cmd::Compare(Cmp::Difference)),
            (none, Key::K, Cmd::Compare(Cmp::Flicker)),
            (none, Key::Escape, Cmd::Escape),
            (none, Key::Delete, Cmd::DeleteShape),
            (none, Key::M, Cmd::Tool(Tool::Measure)),
            (none, Key::T, Cmd::Tool(Tool::Transect)),
            (none, Key::R, Cmd::Tool(Tool::Region)),
            (none, Key::P, Cmd::Tool(Tool::Pin)),
            (none, Key::X, Cmd::PixelGrid),
            (none, Key::N, Cmd::CoordGrid),
            (none, Key::V, Cmd::SwipeOrient),
            (none, Key::Period, Cmd::Step(1)),
            (none, Key::Comma, Cmd::Step(-1)),
            (none, Key::Space, Cmd::Play),
        ];
        ctx.input_mut(|i| {
            for (m, k, c) in table {
                // Without a modifier in the pattern, a key with Ctrl is not this command.
                if (*m == none && (i.modifiers.command || i.modifiers.alt)) || (*m == alt && i.modifiers.command) {
                    continue;
                }
                if i.consume_key(*m, *k) {
                    cmds.push((c.clone(), t));
                }
            }
        });
    }

    pub fn run(&mut self, c: Cmd, id: u32) {
        match c {
            Cmd::Open(add, What::Url) => self.url = Some((String::new(), add, id)),
            Cmd::Open(add, What::Path(p)) => self.open(id, p, add),
            Cmd::Open(add, what) => self.dialog = Some(Dialog::Open { pane: id, add, what }),
            Cmd::Save => self.dialog = Some(Dialog::Save),
            Cmd::Export => self.dialog = Some(Dialog::Export(id)),
            Cmd::Figure => self.figure_open ^= true,
            Cmd::FigureExport(ext) => self.dialog = Some(Dialog::Figure(id, ext)),
            Cmd::Load => self.dialog = Some(Dialog::Load),
            Cmd::NewView => drop(self.split(id)),
            Cmd::Duplicate => self.duplicate(id),
            Cmd::CloseView => self.close(id),
            Cmd::Detach => self.detach(id),
            Cmd::Prefs => self.prefs_open = if self.prefs_open.is_some() { None } else { Some(Default::default()) },
            Cmd::Render => self.render_open ^= true,
            Cmd::Animate => self.set_animate(!self.animate),
            Cmd::RenderStart => self.render_start(id),
            Cmd::FullRes => {
                self.prefs.full_res ^= true;
                self.save_prefs();
            }
            Cmd::Fullscreen => {
                // The window of the view: its own window if the view is detached, else the main window.
                let w = self.wins.iter().find(|d| d.pane == id).map(|d| &d.window).or(self.win.as_ref().map(|w| &w.window));
                if let Some(w) = w {
                    let on = w.fullscreen().is_none();
                    w.set_fullscreen(on.then_some(winit::window::Fullscreen::Borderless(None)));
                }
            }
            Cmd::Layout(n) => {
                self.active = id;
                self.layout(n);
            }
            Cmd::Panel => self.panel ^= true,
            Cmd::Tool(tl) => {
                self.tool = if self.tool == tl { Tool::None } else { tl };
                self.drag_from = None;
            }
            Cmd::ClearPins => self.pins.clear(),
            Cmd::Python => {
                if let Some(p) = &mut self.py {
                    p.open ^= true;
                }
            }
            Cmd::PythonRun => self.py_run(),
            Cmd::Math => self.math(id),
            Cmd::Aggregate(a) => {
                self.agg.0 = a;
                let range = self.agg.1.unwrap_or((0, usize::MAX));
                self.aggregate(id, a, range);
            }
            Cmd::DeleteShape => self.delete_shape(id),
            Cmd::Escape => {
                if self.tool != Tool::None || self.panes.iter().any(|p| p.sel_shape.is_some() || p.shapes.iter().any(|s| !s.done)) {
                    self.tool = Tool::None;
                    for p in &mut self.panes {
                        p.shapes.retain(|s| s.done);
                        p.sel_shape = None;
                    }
                } else {
                    self.run(Cmd::Compare(Cmp::Off), id);
                }
            }
            Cmd::LinkMode => self.link_px ^= true,
            Cmd::Palette => self.palette = Some(Palette::default()),
            Cmd::Space(s) => self.set_space(id, s),
            Cmd::Globe => {
                let on = self.pane(id).is_some_and(|p| !p.v.globe);
                self.set_globe(id, on);
            }
            Cmd::Quit => self.quit = true,
            Cmd::Help => self.help ^= true,
            Cmd::CopyExtent => {
                let Some(r) = self.pane(id).map(|p| p.v.rect()) else { return };
                let text = match (self.lonlat(id, [r[0], r[1]]), self.lonlat(id, [r[2], r[3]])) {
                    (Some(a), Some(b)) => format!("{:.6}, {:.6}, {:.6}, {:.6}", a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)),
                    _ => format!("{}, {}, {}, {}", r[0], r[1], r[2], r[3]),
                };
                self.ctx.copy_text(text);
            }
            Cmd::Zoom(f) => {
                if let Some(p) = self.pane_mut(id) {
                    p.v.scale = (p.v.scale * f).clamp(1e-12, 1e12);
                    p.moved = true;
                }
            }
            Cmd::SetStep(s) => self.set_time(id, s),
            Cmd::Step(d) => {
                if let Some(l) = self.pane(id).and_then(|p| p.timed()) {
                    self.set_time(id, (l.step as i32 + d).rem_euclid(l.steps.len() as i32) as usize);
                }
            }
            Cmd::Play => {
                let now = self.ctx.input(|i| i.time);
                if let Some(p) = self.pane_mut(id).filter(|p| p.timed().is_some()) {
                    (p.play, p.next_step) = (!p.play, now);
                }
            }
            _ => {
                let Some(p) = self.pane_mut(id) else { return };
                let sel = p.sel;
                let recompile = match c {
                    Cmd::Fit => {
                        p.v.fit = true;
                        p.fit_user = true;
                        false
                    }
                    Cmd::OneToOne => {
                        if let Some(w) = p.v.inputs.first().and_then(|i| i.warp.as_ref()) {
                            p.v.scale = 1.0 / w.0.px_size();
                            p.moved = true;
                        }
                        false
                    }
                    Cmd::Link => {
                        p.link = if p.link == 0 { 1 } else { 0 };
                        false
                    }
                    Cmd::Compare(m) => {
                        p.cmp = m;
                        if m == Cmp::Difference {
                            p.diff_range();
                        }
                        false
                    }
                    Cmd::SwipeOrient => {
                        p.vertical ^= true;
                        false
                    }
                    Cmd::Smooth => {
                        p.smooth ^= true;
                        false
                    }
                    Cmd::PixelGrid => {
                        p.pixel_grid ^= true;
                        false
                    }
                    Cmd::CoordGrid => {
                        p.coord_grid ^= true;
                        false
                    }
                    Cmd::SameDisplay => {
                        // The bands (by their names), the stretch and the colors of the selected layer, for
                        // example for the orbits of a day: one stretch, no seams between them.
                        let Some(src) = p.layers.get(sel).map(|l| l.save()) else { return };
                        for l in p.layers.iter_mut().enumerate().filter(|(k, _)| *k != sel).map(|x| x.1) {
                            let mut s = src.clone();
                            (s.series, s.step, s.opacity, s.visible) = (vec![], l.step, l.opacity, l.visible);
                            l.apply(&s);
                        }
                        for k in (0..p.layers.len()).filter(|&k| k != sel) {
                            self.compile(id, k);
                        }
                        false
                    }
                    Cmd::Overlay(k) => {
                        match k {
                            0 => p.overlays.coasts ^= true,
                            1 => p.overlays.borders ^= true,
                            _ => p.overlays.names ^= true,
                        }
                        false
                    }
                    _ => {
                        let Some(l) = p.layers.get_mut(sel) else { return };
                        match c {
                            Cmd::Auto => {
                                l.auto();
                                if p.cmp == Cmp::Difference {
                                    p.diff_range();
                                }
                                false
                            }
                            Cmd::NextCmap => {
                                l.set_cmap(l.cmap + 1);
                                false
                            }
                            Cmd::Cmap(i) => {
                                l.set_cmap(i);
                                false
                            }
                            Cmd::Invert => {
                                l.invert ^= true;
                                false
                            }
                            Cmd::Band(d) => {
                                l.cycle_band(d);
                                true
                            }
                            Cmd::SetBand(b) => {
                                (l.kind, l.band, l.auto_pending) = (Kind::Band, b, true);
                                true
                            }
                            Cmd::Preset(i) => {
                                l.set_preset(&PRESETS[i]);
                                true
                            }
                            _ => false,
                        }
                    }
                };
                if recompile {
                    self.compile(id, sel);
                }
            }
        }
    }

    /// Dialog for a URL: Enter opens it, Esc closes the dialog.
    fn url_ui(&mut self, ctx: &egui::Context) {
        let Some((text, add, id)) = &mut self.url else { return };
        let mut go = false;
        let r = egui::Modal::new(egui::Id::new("url")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.label(if *add { t("Add a layer from a URL") } else { t("Open a URL") });
            let te = ui.add(egui::TextEdit::singleline(text).hint_text("https://... or s3://bucket/key (COG, JPEG 2000, NetCDF, .zarr store)").desired_width(f32::INFINITY));
            te.request_focus();
            go = ui.input(|i| i.key_pressed(Key::Enter));
        });
        if go {
            let (text, add, id) = (text.trim().trim_matches('"').to_string(), *add, *id);
            self.url = None;
            if !text.is_empty() {
                self.open(id, text, add);
            }
        } else if r.should_close() {
            self.url = None;
        }
    }

    fn palette_ui(&mut self, ctx: &egui::Context) {
        let id = self.target();
        let list = commands(self, id);
        let Some(pal) = &mut self.palette else { return };
        let mut hits: Vec<(usize, usize)> = list.iter().enumerate().filter_map(|(i, c)| fuzzy(&pal.q, &c.0).map(|s| (s, i))).collect();
        hits.sort();
        hits.truncate(14);
        let mut run = None;
        let r = egui::Modal::new(egui::Id::new("palette")).show(ctx, |ui| {
            ui.set_width(460.0);
            let te = ui.add(egui::TextEdit::singleline(&mut pal.q).hint_text(t("Type a command, a band, a preset or a color map")).desired_width(f32::INFINITY));
            te.request_focus();
            if te.changed() {
                pal.sel = 0;
            }
            let (down, up, enter) = ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter)));
            if down {
                pal.sel = (pal.sel + 1).min(hits.len().saturating_sub(1));
            }
            if up {
                pal.sel = pal.sel.saturating_sub(1);
            }
            ui.separator();
            for (k, &(_, i)) in hits.iter().enumerate() {
                let (name, key, _) = &list[i];
                let b = ui.add(egui::Button::selectable(k == pal.sel, name.as_str()).shortcut_text(*key).min_size(vec2(ui.available_width(), 0.0)));
                if b.clicked() || (enter && k == pal.sel) {
                    run = Some(i);
                }
            }
        });
        if let Some(i) = run {
            self.palette = None;
            self.run(list[i].2.clone(), id);
        } else if r.should_close() {
            self.palette = None;
        }
    }

    /// Menu bar: all commands with their keys, in the order of other desktop software. The commands run in
    /// the active view.
    fn menus(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<(Cmd, u32)>) {
        let id = self.active;
        let (link, cmp, space, play, timed) = self.pane(id).map_or((0, Cmp::Off, None, false, false), |p| (p.link, p.cmp, p.v.space, p.play, p.timed().is_some()));
        let globe = self.pane(id).is_some_and(|p| p.v.globe);
        let sel = self.pane(id).and_then(|p| p.layers.get(p.sel));
        let own = self.pane(id).and_then(|p| p.layers.first()).and_then(|l| l.default_space());
        let presets: Vec<usize> = (0..PRESETS.len()).filter(|&i| sel.is_some_and(|l| l.preset_ok(&PRESETS[i]))).collect();
        let (cmap, has_layer) = (sel.map_or(0, |l| l.cmap), sel.is_some());
        let (panel, link_px) = (self.panel, self.link_px);
        let grids = self.pane(id).map_or((false, false), |p| (p.pixel_grid, p.coord_grid));
        let (tool, pins) = (self.tool, !self.pins.is_empty());
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(t("File"), |ui| {
                entry(ui, cmds, id, t("Open files..."), "Ctrl+O", Cmd::Open(false, What::Files));
                entry(ui, cmds, id, t("Open folder (SAFE, SEN3, Zarr)..."), "Ctrl+Alt+O", Cmd::Open(false, What::Dirs));
                entry(ui, cmds, id, t("Open URL..."), "Ctrl+L", Cmd::Open(false, What::Url));
                entry(ui, cmds, id, t("Open files as a time series..."), "", Cmd::Open(false, What::Series));
                ui.menu_button(t("Open product"), |ui| {
                    for (i, k) in OPEN_KINDS.iter().enumerate() {
                        entry(ui, cmds, id, &format!("{}...", t(k.0)), "", Cmd::Open(false, What::Kind(i)));
                    }
                });
                ui.add_enabled_ui(!self.recent.is_empty(), |ui| {
                    ui.menu_button(t("Open recent"), |ui| {
                        for r in &self.recent {
                            let name = r.trim_end_matches('/').rsplit('/').next().unwrap_or(r);
                            if ui.button(name).on_hover_text(r).clicked() {
                                cmds.push((Cmd::Open(false, What::Path(r.clone())), id));
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                ui.menu_button(t("Add layer"), |ui| open_menu(ui, true, id, &self.recent, cmds));
                ui.separator();
                entry(ui, cmds, id, t("Save workspace..."), "Ctrl+S", Cmd::Save);
                entry(ui, cmds, id, t("Render video or frames..."), "Ctrl+R", Cmd::Render);
                entry(ui, cmds, id, t("Open workspace..."), "", Cmd::Load);
                ui.separator();
                entry(ui, cmds, id, t("Quit"), "Ctrl+Q", Cmd::Quit);
            });
            ui.menu_button(t("Edit"), |ui| {
                entry(ui, cmds, id, t("Copy view extent"), "Ctrl+Shift+C", Cmd::CopyExtent);
                ui.separator();
                entry(ui, cmds, id, t("Command palette..."), "Ctrl+K", Cmd::Palette);
                ui.separator();
                entry(ui, cmds, id, t("Preferences..."), "Ctrl+,", Cmd::Prefs);
            });
            ui.menu_button(t("View"), |ui| {
                entry(ui, cmds, id, t("Fit"), "F", Cmd::Fit);
                entry(ui, cmds, id, t("Zoom 1:1"), "1", Cmd::OneToOne);
                entry(ui, cmds, id, t("Zoom in"), "+", Cmd::Zoom(1.5));
                entry(ui, cmds, id, t("Zoom out"), "-", Cmd::Zoom(1.0 / 1.5));
                check(ui, cmds, id, globe, t("3D globe"), "G", Cmd::Globe);
                ui.menu_button(t("Display CRS"), |ui| {
                    if let Some(e) = own.filter(|_| SPACES.iter().all(|x| x.0 != own)) {
                        check(ui, cmds, id, space == own, &tf("Layer CRS (EPSG:{})", &[&e.to_string()]), "", Cmd::Space(own));
                    }
                    for s in SPACES {
                        check(ui, cmds, id, space == s.0, t(s.1), "", Cmd::Space(s.0));
                    }
                });
                ui.separator();
                check(ui, cmds, id, panel, t("Side panel"), "H", Cmd::Panel);
                check(ui, cmds, id, grids.0, t("Pixel grid"), "X", Cmd::PixelGrid);
                check(ui, cmds, id, grids.1, t("Coordinate grid"), "N", Cmd::CoordGrid);
                ui.separator();
                entry(ui, cmds, id, t("New view"), "Ctrl+N", Cmd::NewView);
                entry(ui, cmds, id, t("Duplicate view"), "Ctrl+D", Cmd::Duplicate);
                entry(ui, cmds, id, t("Close view"), "Ctrl+W", Cmd::CloseView);
                entry(ui, cmds, id, t("Detach to a window"), "Ctrl+Shift+D", Cmd::Detach);
                entry(ui, cmds, id, t("Full screen"), "F11", Cmd::Fullscreen);
                ui.menu_button(t("Layout"), |ui| {
                    for (n, name, k) in [(1, "1 view", "Alt+1"), (2, "2 views", "Alt+2"), (4, "2 x 2 views", "Alt+3"), (9, "3 x 3 views", "Alt+4")] {
                        entry(ui, cmds, id, t(name), k, Cmd::Layout(n));
                    }
                });
                ui.separator();
                check(ui, cmds, id, link > 0, t("Link this view"), "L", Cmd::Link);
                check(ui, cmds, id, !link_px, t("Link mode: geographic"), if link_px { "Shift+L" } else { "" }, Cmd::LinkMode);
                check(ui, cmds, id, link_px, t("Link mode: pixel"), if link_px { "" } else { "Shift+L" }, Cmd::LinkMode);
            });
            ui.menu_button(t("Layer"), |ui| {
                ui.add_enabled_ui(has_layer, |ui| {
                    entry(ui, cmds, id, t("Next band"), "]", Cmd::Band(1));
                    entry(ui, cmds, id, t("Previous band"), "[", Cmd::Band(-1));
                    ui.add_enabled_ui(!presets.is_empty(), |ui| {
                        ui.menu_button(t("Preset"), |ui| {
                            for &i in &presets {
                                entry(ui, cmds, id, t(PRESETS[i].0), "", Cmd::Preset(i));
                            }
                        });
                    });
                    entry(ui, cmds, id, t("Export data (GeoTIFF)..."), "", Cmd::Export);
                    ui.separator();
                    entry(ui, cmds, id, t("Automatic stretch"), "A", Cmd::Auto);
                    entry(ui, cmds, id, t("Same display for all layers"), "", Cmd::SameDisplay);
                    ui.menu_button(t("Color map"), |ui| {
                        for (i, (n, _)) in CMAPS.iter().enumerate() {
                            check(ui, cmds, id, i == cmap, n, "", Cmd::Cmap(i));
                        }
                    });
                    entry(ui, cmds, id, t("Next color map"), "C", Cmd::NextCmap);
                    entry(ui, cmds, id, t("Invert color map"), "I", Cmd::Invert);
                    ui.separator();
                    ui.add_enabled_ui(timed, |ui| {
                        ui.menu_button(t("Aggregate over time"), |ui| {
                            for (a, n) in eo_cache::Agg::ALL {
                                entry(ui, cmds, id, t(n), "", Cmd::Aggregate(a));
                            }
                        });
                    });
                });
            });
            ui.menu_button(t("Compare"), |ui| {
                for (c, n, k) in Cmp::ALL {
                    check(ui, cmds, id, cmp == c, t(n), k, Cmd::Compare(c));
                }
                ui.separator();
                entry(ui, cmds, id, t("Swipe line: vertical or horizontal"), "V", Cmd::SwipeOrient);
            });
            ui.menu_button(t("Tools"), |ui| {
                for (tl, n, k) in Tool::ALL {
                    check(ui, cmds, id, tool == tl, t(n), k, Cmd::Tool(tl));
                }
                ui.add_enabled_ui(pins, |ui| entry(ui, cmds, id, t("Remove the pinned points"), "", Cmd::ClearPins));
                ui.separator();
                entry(ui, cmds, id, t("Python..."), "F7", Cmd::Python);
                ui.separator();
                check(ui, cmds, id, grids.0, t("Pixel grid"), "X", Cmd::PixelGrid);
                check(ui, cmds, id, grids.1, t("Coordinate grid"), "N", Cmd::CoordGrid);
            });
            ui.menu_button(t("Time"), |ui| {
                ui.add_enabled_ui(timed, |ui| {
                    check(ui, cmds, id, play, t("Play"), "Space", Cmd::Play);
                    entry(ui, cmds, id, t("Next step"), ".", Cmd::Step(1));
                    entry(ui, cmds, id, t("Previous step"), ",", Cmd::Step(-1));
                    entry(ui, cmds, id, t("First step"), "", Cmd::SetStep(0));
                    entry(ui, cmds, id, t("Last step"), "", Cmd::SetStep(usize::MAX));
                });
                ui.separator();
                entry(ui, cmds, id, t("Open files as a time series..."), "", Cmd::Open(false, What::Series));
            });
            ui.menu_button(t("Help"), |ui| {
                entry(ui, cmds, id, t("Keys..."), "F1", Cmd::Help);
                ui.separator();
                ui.label(format!("{} {}", crate::APP, env!("CARGO_PKG_VERSION")));
                if let Some(w) = &self.win {
                    ui.small(&w.name);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let on = self.animate;
                if ui.selectable_label(on, t("Animate")).on_hover_text(t("The animate workspace: make the scene of an animation, with a preview, and render it (F6)")).clicked() != ui.selectable_label(!on, t("View")).on_hover_text(t("The view workspace")).clicked() {
                    cmds.push((Cmd::Animate, id));
                }
            });
        });
    }

    /// Window with the keys of the commands.
    fn help_ui(&mut self, ctx: &egui::Context) {
        if !self.help {
            return;
        }
        let mut open = true;
        egui::Window::new(t("Keys")).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| keys_grid(self, ui));
        });
        self.help = open;
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<(Cmd, u32)>) {
        let id = self.active;
        ui.horizontal(|ui| {
            // Split buttons: the button opens files of all formats, the arrow has the product types.
            for (add, icon, name, tip) in [
                (false, Icon::Open, t("Open"), t("Open files in the active view (Ctrl+O). Drop files or directories on a view to open them there")),
                (true, Icon::AddLayer, t("Layer"), t("Add files as layers of the active view (Ctrl+Shift+O, or Shift + drop)")),
            ] {
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 1.0;
                    if icons::button(ui, icon, name, false).on_hover_text(tip).clicked() {
                        cmds.push((Cmd::Open(add, What::Files), id));
                    }
                    let arrow = icons::button(ui, Icon::Down, "", false).on_hover_text(t("Product types (SAFE, SEN3, Zarr, GeoTIFF, ...), folders, URL, time series, recent products"));
                    egui::Popup::menu(&arrow).show(|ui| open_menu(ui, add, id, &self.recent, cmds));
                });
            }
            let mut b = |ui: &mut egui::Ui, icon: Icon, text: &str, tip: &str, c: Cmd| {
                if icons::button(ui, icon, text, false).on_hover_text(tip).clicked() {
                    cmds.push((c, id));
                }
            };
            b(ui, Icon::Save, t("Save"), t("Save the workspace: layout, views, layers, settings (Ctrl+S)"), Cmd::Save);
            ui.separator();
            b(ui, Icon::Fit, t("Fit"), t("Show all data of the view (F, or double-click)"), Cmd::Fit);
            b(ui, Icon::ZoomIn, "", t("Zoom in (+, or the mouse wheel)"), Cmd::Zoom(1.5));
            b(ui, Icon::ZoomOut, "", t("Zoom out (-)"), Cmd::Zoom(1.0 / 1.5));
            ui.separator();
            for (n, c, r, name, k) in [(1, 1, 1, "1 view", "Alt+1"), (2, 2, 1, "2 views", "Alt+2"), (4, 2, 2, "2 x 2 views", "Alt+3"), (9, 3, 3, "3 x 3 views", "Alt+4")] {
                b(ui, Icon::Grid(c, r), "", &format!("{} ({k})", tf("Layout: {}", &[t(name)])), Cmd::Layout(n));
            }
            if ui.button("1:1").on_hover_text(t("One data pixel for each screen pixel (1)")).clicked() {
                cmds.push((Cmd::OneToOne, id));
            }
            ui.separator();
            let Some(p) = self.pane(id) else { return };
            let (link, cmp, space, globe) = (p.link, p.cmp, p.v.space, p.v.globe);
            if icons::button(ui, Icon::Link, t("Link"), link > 0).on_hover_text(t("Link the active view: it pans and zooms with the other linked views (L)")).clicked() {
                cmds.push((Cmd::Link, id));
            }
            if icons::button(ui, Icon::Globe, t("Globe"), globe).on_hover_text(t("Show the active view on a 3D globe, or as a 2D map (G)")).clicked() {
                cmds.push((Cmd::Globe, id));
            }
            ui.separator();
            let (pg, cg) = self.pane(id).map_or((false, false), |p| (p.pixel_grid, p.coord_grid));
            for (icon, on, tip, c) in [(Icon::PixelGrid, pg, "Pixel grid: the edges of the pixels, and their values in a large zoom (X)", Cmd::PixelGrid), (Icon::Graticule, cg, "Coordinate grid: lines of longitude and latitude (N)", Cmd::CoordGrid)] {
                if icons::button(ui, icon, "", on).on_hover_text(t(tip)).clicked() {
                    cmds.push((c, id));
                }
            }
            for ((tl, n, k), icon) in Tool::ALL.into_iter().zip([Icon::Ruler, Icon::Transect, Icon::Region, Icon::Pin]) {
                if icons::button(ui, icon, "", self.tool == tl).on_hover_text(format!("{} ({k})", t(n))).clicked() {
                    cmds.push((Cmd::Tool(tl), id));
                }
            }
            ui.separator();
            let mut px = self.link_px;
            ui.selectable_value(&mut px, false, t("Geo")).on_hover_text(t("Link by center latitude, longitude and ground resolution: views in different CRSs stay aligned (Shift+L)"));
            ui.selectable_value(&mut px, true, t("Pixel")).on_hover_text(t("Link by pixel region: for products on the same grid (Shift+L)"));
            self.link_px = px;
            ui.separator();
            let r = ui.allocate_exact_size(vec2(13.0, 13.0), Sense::hover()).0;
            icons::draw(ui.painter(), Icon::Compare, r, ui.visuals().text_color());
            ui.label(t("Compare"));
            for (c, n, k) in Cmp::ALL {
                if ui.selectable_label(cmp == c, t(n)).on_hover_text(format!("{} ({k})", tf("{} of the two lowest layers of the active view", &[t(n)]))).clicked() {
                    cmds.push((Cmd::Compare(c), id));
                }
            }
            ui.separator();
            let own = self.pane(id).and_then(|p| p.layers.first()).and_then(|l| l.default_space());
            let label = |s: Option<u32>| match SPACES.iter().find(|x| x.0 == s) {
                Some(x) => t(x.1).to_string(),
                None => tf("Layer CRS (EPSG:{})", &[&s.unwrap_or(0).to_string()]),
            };
            let mut sp = space;
            egui::ComboBox::from_id_salt("crs").selected_text(label(sp)).width(210.0).show_ui(ui, |ui| {
                if own.is_some() && SPACES.iter().all(|x| x.0 != own) {
                    ui.selectable_value(&mut sp, own, label(own));
                }
                for s in SPACES {
                    ui.selectable_value(&mut sp, s.0, t(s.1));
                }
            });
            if sp != space {
                cmds.push((Cmd::Space(sp), id));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(ui, Icon::Search, t("Commands  Ctrl+K"), false).on_hover_text(t("Find any command, band, preset or color map by name")).clicked() {
                    cmds.push((Cmd::Palette, id));
                }
            });
        });
    }

    pub fn status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(e) = self.error.clone() {
                if ui.small_button("x").on_hover_text(t("Close the message")).clicked() {
                    self.error = None;
                }
                ui.colored_label(ui.visuals().error_fg_color, e);
                ui.separator();
            }
            if let Some(id) = self.hovered
                && let Some(c) = self.pane(id).and_then(|p| p.v.cursor)
            {
                let space = self.pane(id).and_then(|p| p.v.space);
                if let Some((lon, lat)) = self.lonlat(id, c) {
                    ui.monospace(format!("lat {lat:.6}  lon {lon:.6}"));
                    ui.separator();
                }
                ui.monospace(match space {
                    Some(e) => format!("EPSG:{e}  {:.3}  {:.3}", c[0], c[1]),
                    None => format!("pixel {:.1}  {:.1}", c[0], -c[1]),
                });
                if let Some(v) = self.inspect(id).1 {
                    ui.separator();
                    ui.monospace(v);
                }
            }
            if let Some(x) = &self.export {
                match &x.end {
                    None => {
                        ui.spinner();
                        ui.monospace(tf("Export: {} / {} tiles", &[&x.done.to_string(), &x.total.to_string()]));
                        if ui.small_button(t("Stop")).clicked() {
                            x.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    Some(note) => {
                        let note = note.clone();
                        if ui.small_button("x").on_hover_text(t("Close the message")).clicked() {
                            self.export = None;
                        }
                        ui.label(tf("Export: {}", &[&note]));
                    }
                }
                ui.separator();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let st = self.engine.stats();
                let (alloc, _) = self.win.as_ref().map_or((0, 0), |w| w.gpu.usage());
                ui.monospace(format!("RAM {} / {}   GPU {} / {}", mb(st.raw + st.dec + st.probe + st.work), mb(st.limit), mb(alloc), mb(self.gpu_budget)));
                if st.running + st.wanted > 0 {
                    ui.separator();
                    ui.monospace(tf("loading {} tiles", &[&(st.running + st.wanted).to_string()]));
                    ui.spinner();
                }
            });
        });
    }

    pub fn side(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<(Cmd, u32)>) {
        let id = self.active;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.strong(t("Layers"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+ Add").on_hover_text(t("Add layers to this view (Ctrl+Shift+O, or Shift + drop)")).clicked() {
                    cmds.push((Cmd::Open(true, What::Files), id));
                }
            });
        });
        let Some(p) = self.pane_mut(id) else { return };
        if p.layers.is_empty() {
            ui.label(t("No layer. Open a product (Ctrl+O) or drop files on the view."));
            return;
        }
        // The list shows the top layer first.
        let mut rebuild = false;
        let mut action: Option<(usize, i32)> = None;
        let n = p.layers.len();
        for k in (0..n).rev() {
            let tag = match p.spec_layer.iter().position(|&i| i == k) {
                Some(0) if p.cmp != Cmp::Off => "A ",
                Some(1) if p.cmp != Cmp::Off => "B ",
                _ => "",
            };
            ui.horizontal(|ui| {
                let l = &mut p.layers[k];
                rebuild |= ui.checkbox(&mut l.visible, "").on_hover_text(t("Show or hide")).changed();
                let name = format!("{tag}{}", l.label(200));
                // The buttons from the right, then the name in the rest of the row (a fixed sum of widths
                // can be more than the row: the panel then grows at each frame).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::small(ui, Icon::Close).on_hover_text(t("Remove")).clicked() {
                        action = Some((k, 0));
                    }
                    if ui.add_enabled_ui(k > 0, |ui| icons::small(ui, Icon::Down)).inner.on_hover_text(t("Move down")).clicked() {
                        action = Some((k, -1));
                    }
                    if ui.add_enabled_ui(k + 1 < n, |ui| icons::small(ui, Icon::Up)).inner.on_hover_text(t("Move up")).clicked() {
                        action = Some((k, 1));
                    }
                    let w = ui.available_width();
                    if ui.add_sized([w, 18.0], egui::Button::selectable(p.sel == k, name).truncate()).on_hover_text(&l.path).clicked() {
                        p.sel = k;
                    }
                });
            });
        }
        match action {
            Some((k, 0)) => {
                p.layers.remove(k);
                p.sel = p.sel.min(p.layers.len().saturating_sub(1));
                rebuild = true;
            }
            Some((k, d)) => {
                let j = (k as i32 + d) as usize;
                p.layers.swap(k, j);
                p.sel = j;
                rebuild = true;
            }
            None => {}
        }
        if let Some(e) = &p.err {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if rebuild {
            self.rebuild(id);
        }
        let p = self.pane_mut(id).unwrap();
        if p.cmp != Cmp::Off {
            ui.separator();
            compare_ui(ui, p);
        }
        let Some(l) = p.layers.get_mut(p.sel) else { return };
        let sel = p.sel;

        ui.separator();
        ui.horizontal(|ui| {
            ui.strong(l.comp_name());
            ui.add(egui::Slider::new(&mut l.opacity, 0.0..=1.0).show_value(false)).on_hover_text(t("Opacity"));
        });
        if let Some(d) = l.any().map(|a| a.ds.product.desc.clone()) {
            ui.small(d);
        }
        let mut changed = false;
        ui.horizontal(|ui| {
            let k = l.kind;
            ui.selectable_value(&mut l.kind, Kind::Band, t("Band"));
            ui.selectable_value(&mut l.kind, Kind::Rgb, "RGB");
            ui.selectable_value(&mut l.kind, Kind::Expr, t("Band math"));
            ui.selectable_value(&mut l.kind, Kind::Wind, t("Wind")).on_hover_text(t("A vector field: the speed with the color map, and arrows. It uses the components to the east (U) and to the north (V)"));
            if k != l.kind {
                // The wind mode starts with the components that the product has.
                if l.kind == Kind::Wind && let Some([u, v]) = layer::wind_pair(&l.names()) {
                    (l.rgb[0], l.rgb[1]) = (u, v);
                }
                if l.kind == Kind::Wind && l.cmap == 0 {
                    l.set_cmap(layer::WIND_CMAP);
                }
                l.auto_pending = true;
                changed = true;
            }
        });
        match l.kind {
            Kind::Band => {}
            Kind::Rgb => {
                for (k, lbl) in ["R", "G", "B"].iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(*lbl);
                        let r = ui.add(egui::TextEdit::singleline(&mut l.rgb[k]).desired_width(f32::INFINITY));
                        changed |= r.lost_focus();
                    });
                }
            }
            Kind::Expr => {
                let r = ui.add(egui::TextEdit::singleline(&mut l.expr).hint_text("(B08 - B04) / (B08 + B04)").desired_width(f32::INFINITY));
                changed |= r.lost_focus();
            }
            Kind::Wind => {
                for (k, lbl) in ["U", "V"].iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(*lbl).on_hover_text([t("The component to the east"), t("The component to the north")][k]);
                        let r = ui.add(egui::TextEdit::singleline(&mut l.rgb[k]).desired_width(f32::INFINITY));
                        changed |= r.lost_focus();
                    });
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut l.particles, t("Particles")).on_hover_text(t("Points that follow the wind, with a trail"));
                    ui.checkbox(&mut l.arrows, t("Arrows"));
                    ui.checkbox(&mut l.fill, t("Speed colors")).on_hover_text(t("The speed with the color map. Without it, the layers below show, and the particles have the color of the speed"));
                });
            }
        }
        ui.horizontal_wrapped(|ui| {
            for p in PRESETS {
                if l.preset_ok(p) && ui.small_button(t(p.0)).clicked() {
                    l.set_preset(p);
                    changed = true;
                }
            }
        });
        if let Some(e) = &l.err {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        // Product tree: a click on a name shows the variable as data. The buttons of a row set a channel
        // (RGB mode) or add the name to the expression (band math mode).
        match contents_ui(ui, l) {
            Some(Pick::Insert(c)) => {
                if !l.expr.is_empty() && !l.expr.ends_with([' ', '(']) {
                    l.expr.push(' ');
                }
                l.expr += &l.chans[c].id;
                changed = true;
            }
            Some(Pick::Chan(c)) => {
                (l.kind, l.band, l.auto_pending) = (Kind::Band, c, true);
                changed = true;
            }
            Some(Pick::Rgb(k, c)) => {
                (l.rgb[k], l.auto_pending) = (l.chans[c].id.clone(), true);
                changed = true;
            }
            Some(Pick::Color(c)) => {
                (l.kind, l.rgb, l.auto_pending) = (Kind::Rgb, c.map(|c| l.chans[c].id.clone()), true);
                changed = true;
            }
            None => {}
        }

        ui.separator();
        let n = if l.kind == Kind::Rgb { 3 } else { 1 };
        let colors = if n == 3 { [Color32::from_rgb(230, 80, 80), Color32::from_rgb(90, 200, 90), Color32::from_rgb(90, 140, 255)] } else { [ui.visuals().text_color(); 3] };
        let mut again = false;
        for k in 0..n {
            if let Some(h) = l.hist.get(k).cloned() {
                let (_, dbl) = histogram(ui, &h, &mut l.st[k], colors[k]);
                again |= dbl;
            }
            ui.horizontal(|ui| {
                let st = &mut l.st[k];
                let speed = ((st.hi - st.lo).abs() / 300.0).max(1e-6) as f64;
                ui.add(egui::DragValue::new(&mut st.lo).speed(speed).max_decimals(4)).on_hover_text(t("Minimum"));
                ui.add(egui::DragValue::new(&mut st.hi).speed(speed).max_decimals(4)).on_hover_text(t("Maximum"));
                ui.add(egui::DragValue::new(&mut st.gamma).speed(0.01).range(0.1..=5.0).prefix("gamma ")).on_hover_text(t("Gamma"));
                again |= ui.checkbox(&mut st.db, "dB").changed();
            });
        }
        ui.horizontal(|ui| {
            ui.label(t("Clip %"));
            again |= ui.add(egui::Slider::new(&mut l.clip, 0.0..=10.0)).changed();
            again |= ui.button(t("Auto")).on_hover_text(t("Automatic stretch (A)")).clicked();
            if l.is_color() && ui.button(t("As is")).on_hover_text(t("No stretch: the colors of the file")).clicked() {
                l.as_is();
            }
        });
        if again {
            l.auto();
        }
        if l.kind != Kind::Rgb {
            ui.separator();
            if let Some(i) = swatches(ui, l.cmap, l.invert) {
                l.set_cmap(i);
            }
            ui.horizontal(|ui| {
                ui.checkbox(&mut l.invert, t("Invert (I)"));
                ui.menu_button(t("Edit colors"), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for s in l.stops.iter_mut() {
                            ui.color_edit_button_srgb(s);
                        }
                        if ui.small_button("+").on_hover_text(t("Add a color")).clicked() {
                            l.stops.push(*l.stops.last().unwrap());
                        }
                        if l.stops.len() > 2 && ui.small_button("-").on_hover_text(t("Remove the last color")).clicked() {
                            l.stops.pop();
                        }
                    });
                });
            });
        }
        if changed {
            self.compile(id, sel);
        }

        if let Some(l) = self.pane(id).and_then(|p| p.layers.get(p.sel)).filter(|l| l.steps.len() > 1) {
            let n = l.steps.len();
            let labels: Vec<String> = (0..n).map(|s| l.step_label(s)).collect();
            ui.separator();
            egui::CollapsingHeader::new(t("Aggregate over time")).default_open(false).show(ui, |ui| {
                let (how, range) = &mut self.agg;
                let (mut a, mut b) = range.unwrap_or((0, n - 1));
                (a, b) = (a.min(n - 1), b.min(n - 1));
                egui::ComboBox::from_id_salt("agg").selected_text(t(how.name())).show_ui(ui, |ui| {
                    for (x, name) in eo_cache::Agg::ALL {
                        ui.selectable_value(how, x, t(name));
                    }
                });
                egui::Grid::new("agg range").num_columns(3).show(ui, |ui| {
                    for (k, v) in [(t("First step"), &mut a), (t("Last step"), &mut b)] {
                        ui.label(k);
                        let mut s = *v + 1;
                        ui.add(egui::DragValue::new(&mut s).range(1..=n));
                        *v = s - 1;
                        ui.small(&labels[*v]);
                        ui.end_row();
                    }
                });
                *range = Some((a, b)).filter(|r| *r != (0, n - 1));
                ui.small(tf("{} steps. The steps must be on the same grid.", &[&(a.abs_diff(b) + 1).to_string()]));
                if ui.button(t("Make the layer")).on_hover_text(t("A new layer of this view. The tiles of the view read the steps: the result shows step by step.")).clicked() {
                    cmds.push((Cmd::Aggregate(*how), id));
                }
            });
        }
        if let Some(p) = self.pane(id).filter(|p| !p.layers.is_empty()) {
            let names: Vec<String> = p.layers.iter().enumerate().map(|(i, l)| format!("{} = {}", crate::app::math_name(i), l.name)).collect();
            ui.separator();
            egui::CollapsingHeader::new(t("Layer math")).default_open(false).show(ui, |ui| {
                for n in names {
                    ui.small(n);
                }
                let r = ui.add(egui::TextEdit::singleline(&mut self.math).hint_text("a - b").desired_width(f32::INFINITY));
                ui.small(t("Operators: + - * / ^, comparisons < > <= >= == != (1 or 0), functions abs sqrt ln log10 exp min max pow clamp, and mask(x, c): no data where c is not 0. Each layer shows one band. The layers are on the same grid."));
                if ui.button(t("Make the layer")).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    cmds.push((Cmd::Math, id));
                }
            });
        }
        ui.separator();
        egui::CollapsingHeader::new(t("Tools")).default_open(true).show(ui, |ui| crate::tools::results_ui(self, ui, id));
        egui::CollapsingHeader::new(t("Inspector")).default_open(true).show(ui, |ui| match self.hovered {
            Some(h) => {
                let t = self.inspect(h).0;
                ui.monospace(t);
            }
            None => drop(ui.label(t("Move the cursor over a view."))),
        });
        egui::CollapsingHeader::new(t("Memory")).default_open(false).show(ui, |ui| {
            let st = self.engine.stats();
            let (alloc, used) = self.win.as_ref().map_or((0, 0), |w| w.gpu.usage());
            let tiles = self.win.as_ref().map_or(0, |w| w.gpu.resident());
            ui.monospace(format!(
                "RAM budget {}\n  raw bytes {}\n  decoded   {}\n  inspector {}\n  work      {}\nGPU budget {}\n  allocated {}\n  tiles     {} ({})\nDisk budget {}\n  remote data {}\ntiles running {} wanted {}",
                mb(st.limit),
                mb(st.raw),
                mb(st.dec),
                mb(st.probe),
                mb(st.work),
                mb(self.gpu_budget),
                mb(alloc),
                mb(used),
                tiles,
                mb(st.disk_limit as usize),
                mb(st.disk as usize),
                st.running,
                st.wanted,
            ));
            if let Some(w) = &self.win {
                ui.small(&w.name);
            }
        });
    }

    /// Values under the cursor of view `id`: the text of the inspector, and the value of the top layer
    /// for the status bar.
    pub fn inspect(&mut self, id: u32) -> (String, Option<String>) {
        let Some(p) = self.pane(id) else { return (String::new(), None) };
        let Some(c) = p.v.cursor else { return (t("Move the cursor over the view.").into(), None) };
        let mut s = String::new();
        let mut top = None;
        let mut req = vec![];
        for (li, l) in p.layers.iter().enumerate().rev() {
            if l.inputs.is_empty() {
                continue;
            }
            s += &format!("{}\n", l.label(60));
            let mut vals = vec![];
            for (j, inp) in l.inputs.iter().enumerate() {
                let name = l.used.get(j).map_or("?", |&c| l.chans[c].id.as_str());
                let Some((w, _)) = self.warps.get(&(inp.id, p.v.space)) else { continue };
                let Some((x, y)) = w.inverse(c[0], c[1]) else {
                    s += &format!("  {name}: {}\n", t("outside"));
                    vals.push(f64::NAN);
                    continue;
                };
                let (x, y) = (x as u64, y as u64);
                if self.bench.is_none() {
                    req.push((inp.id, x, y));
                }
                let v = inp.var();
                let unit = if v.units.is_empty() { String::new() } else { format!(" {}", v.units) };
                let probe = self.probes.get(&inp.id).and_then(|p| p.last.as_ref()).filter(|p| (p.0, p.1) == (x, y));
                let val = probe.and_then(|p| p.2.get(inp.band as usize).copied());
                let line = match val {
                    Some(Some(v)) => {
                        vals.push(v);
                        format!("{name} [{x}, {y}]: {v}{unit}")
                    }
                    Some(None) => {
                        vals.push(f64::NAN);
                        format!("{name} [{x}, {y}]: {}", t("no data"))
                    }
                    None => {
                        vals.push(f64::NAN);
                        format!("{name} [{x}, {y}]: ...")
                    }
                };
                if top.is_none() && li + 1 == p.layers.len() {
                    top = Some(line.clone());
                }
                s += &format!("  {line}\n");
                // The other bands of a multi-band variable.
                if v.bands.len() > 1
                    && let Some(pr) = probe
                {
                    for (b, val) in v.bands.iter().zip(&pr.2) {
                        s += &format!("    {b}: {}\n", val.map_or(t("no data").into(), |v| format!("{v}{unit}")));
                    }
                }
                if let Some(f) = v.fill {
                    s += &format!("    fill value {f}\n");
                }
            }
            let plain = l.trees.iter().all(|t| matches!(t, eo_render::bandmath::Node::Var(_)));
            if !plain && vals.len() == l.inputs.len() {
                for (k, t) in l.trees.iter().enumerate() {
                    let lbl = if l.trees.len() == 3 { ["R", "G", "B"][k] } else { "value" };
                    s += &format!("  {lbl}: {:.6}\n", t.eval(&vals));
                }
            }
        }
        for (l, x, y) in req {
            self.request_probe(l, x, y);
        }
        (s, top)
    }

    /// Draw the visible views: tile requests, composite, then the overlays (swipe line, crosshair,
    /// labels, scale bar). The views that are not visible cancel their tile requests.
    ///
    /// `only`: the detached view to draw (the frame of its window). None: the views of the main window.
    fn paint(&mut self, ctx: &egui::Context, only: Option<u32>) {
        let ppp = ctx.pixels_per_point();
        let t = ctx.input(|i| i.time);
        let mut missing = false;
        let mut next_flip: Option<f64> = None;
        for i in 0..self.panes.len() {
            let id = self.panes[i].id;
            if only.map_or(self.floating.contains(&id) || self.scene_pane() == Some(id), |o| o != id) {
                continue;
            }
            let Some(painter) = self.panes[i].painter.clone() else {
                self.panes[i].v.idle(&self.engine);
                continue;
            };
            let mpp = self.meters_per_px(id);
            // Map overlays of a view that is not in longitude and latitude: the lines in its CRS, and the
            // positions of the names.
            let pv = &self.panes[i];
            let projected = pv.v.space.filter(|&e| e != 4326 && !pv.v.globe && pv.overlays.any());
            let with_names = pv.overlays.names;
            let outline = projected.map(|e| self.outline_points(id, e));
            let names: Vec<Option<[f64; 2]>> = match projected.filter(|_| with_names) {
                Some(e) => crate::outlines::data().labels.iter().map(|l| self.from_lonlat(e, l.0)).collect(),
                None => vec![],
            };
            let cross = match self.cursor {
                Some((g, from, cam)) if g == self.panes[i].link && from != id => self.uncam(i, cam).map(|c| c.0),
                _ => None,
            };
            let Some(win) = &mut self.win else { return };
            let p = &mut self.panes[i];
            if p.layers.is_empty() {
                p.v.idle(&self.engine);
                continue;
            }
            if p.v.gpu.is_none() {
                p.v.gpu = Some(View2d::new(&win.gpu));
                p.luts.clear();
            }
            p.v.clamp_globe();
            if p.v.globe {
                globe_backdrop(&p.v, &painter.with_clip_rect(p.rect), p.rect);
            }
            p.v.full_res = self.prefs.full_res;
            let (miss, changed) = p.v.draws(&mut win.gpu, &self.engine);
            p.missing = miss || p.v.inputs.iter().any(|i| i.warp.is_none());
            missing |= p.missing;
            if changed {
                ctx.request_repaint();
            }
            let inputs = p.v.layer_uniforms();
            let two = p.specs.len() >= 2;
            let cmp = match p.cmp {
                Cmp::Swipe if two => Compare::Swipe,
                Cmp::Difference if two => Compare::Difference,
                Cmp::Flicker if two => Compare::Flicker,
                _ => Compare::Stack,
            };
            let show_b = if cmp == Compare::Flicker {
                let ph = t * p.flicker_hz as f64;
                next_flip = Some(next_flip.map_or(f64::MAX, |n: f64| n).min((ph.floor() + 1.0 - ph) / p.flicker_hz as f64));
                ph as u64 % 2
            } else {
                0
            };
            // The groups of layers: the first with the compare mode, the others over it in the stack mode.
            let mut groups: Vec<(usize, &[eo_render::LayerSpec], &[usize], Compare)> = vec![(0, &p.specs, &p.spec_layer, cmp)];
            if cmp == Compare::Stack {
                groups.extend(p.more.iter().map(|(st, sp, l)| (*st, &sp[..], &l[..], Compare::Stack)));
            }
            let rows_n = eo_render::LUT_ROWS as usize;
            p.luts.resize(groups.len() * rows_n, vec![]);
            let vg = p.v.gpu.as_mut().unwrap();
            vg.keep_groups(&mut win.gpu, groups.len());
            // A weather model has large pixels: they are smooth in a view with a wind layer.
            let wind = groups.iter().flat_map(|g| g.2.iter()).any(|&li| p.layers[li].kind == Kind::Wind);
            vg.smooth = p.smooth || wind;
            if wind {
                next_flip = Some(next_flip.map_or(1.0 / 30.0, |n: f64| n.min(1.0 / 30.0)));
            }
            for (g, &(start, specs, lays, gcmp)) in groups.iter().enumerate() {
                let end = groups.get(g + 1).map_or(inputs.len(), |x| x.0);
                // Color map rows: one for each layer, the last for the difference.
                let mut rows: Vec<(Vec<[u8; 3]>, u32)> = lays.iter().enumerate().map(|(k, &li)| (p.layers[li].stops.clone(), k as u32)).collect();
                if g == 0 {
                    rows.push((CMAPS[p.dcmap].1.iter().map(|&c| layer::hex(c)).collect(), eo_render::LUT_ROWS - 1));
                }
                for (stops, row) in rows {
                    if p.luts[g * rows_n + row as usize] != stops {
                        vg.set_lut(&win.gpu, g, row, &layer::lut(&stops));
                        p.luts[g * rows_n + row as usize] = stops;
                    }
                }
                let mut cu = CompositeUniforms {
                    vo: [p.v.px.min.x, p.v.px.min.y],
                    n: specs.len() as u32,
                    swipe: p.swipe * if p.vertical { p.v.px.width() } else { p.v.px.height() },
                    vertical: p.vertical as u32,
                    show_b: show_b as u32,
                    diff: p.diff,
                    dlo: p.dlo,
                    dhi: if p.dhi == p.dlo { p.dlo + 1e-6 } else { p.dhi },
                    dflags: (p.dinvert as u32) << 3,
                    tmix: p.tmix,
                    ..Default::default()
                };
                for (k, &li) in lays.iter().enumerate().take(4) {
                    cu.l[k] = p.layers[li].params();
                    if p.layers[li].kind == Kind::Wind {
                        // An arrow for each 34 points. The pulse of the arrows goes from the tail to the head in 1.2 s.
                        cu.l[k].pad = [34.0 * ppp, (t / 1.2).fract() as f32];
                    }
                    if g == 0 && k == 1 && p.cmp == Cmp::Blend {
                        cu.l[k].opacity = p.blend;
                    }
                }
                match vg.paint(&mut win.gpu, g, start..end, &inputs, specs, gcmp, &cu, p.rect, (p.v.px.width() as u32, p.v.px.height() as u32)) {
                    Ok(Some(cb)) => drop(painter.add(cb)),
                    Ok(None) => {}
                    Err(e) => p.err = Some(e),
                }
            }
            if p.v.globe {
                graticule(&p.v, &painter.with_clip_rect(p.rect), p.rect);
            }
            // The particles of the wind layers. They ask for the tiles that they need on the CPU: an
            // other client of the engine than the tiles of the view.
            let mut need = vec![];
            p.field_miss = false;
            for li in 0..p.layers.len() {
                if p.layers[li].visible && p.layers[li].kind == Kind::Wind && p.layers[li].particles {
                    p.field_miss |= crate::wind::particles(p, li, &self.fields, &mut need, &painter, ppp, t);
                }
            }
            // The map of a figure: the data only.
            if !p.bare {
                if p.v.space.is_some() || p.v.globe {
                    let labels = &crate::outlines::data().labels;
                    let at = |ll: [f64; 2]| if names.is_empty() { Some(ll) } else { labels.iter().position(|l| l.0 == ll).and_then(|k| names[k]) };
                    crate::outlines::draw(p, &painter, ppp, outline.as_ref().map(|v| &v[..]), &at);
                }
                overlays(p, &painter, ppp, mpp, cross);
                self.tools_paint(i, &painter, ppp, &mut need);
            }
            let p = &mut self.panes[i];
            let keys: Vec<eo_cache::TileKey> = need.iter().map(|n| n.1).collect();
            if keys != p.field_sent {
                self.field_keys.extend(keys.iter().copied());
                self.engine.want_copy(0x8000_0000 | p.id, need);
                p.field_sent = keys;
            }
        }
        // The tiles on the CPU that no view wants now go away.
        if only.is_none() {
            let panes = &self.panes;
            let wanted = |k: &eo_cache::TileKey| panes.iter().any(|p| p.field_sent.contains(k));
            self.fields.retain(|k, _| wanted(k));
            self.field_keys.retain(wanted);
        }
        if let Some(b) = &mut self.bench {
            b.missing = missing;
        }
        if let Some(dt) = next_flip {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(dt.max(0.001)));
        }
    }
}

/// Globe view, below the data: the space, and the globe where no layer has data.
fn globe_backdrop(v: &crate::view::View, pt: &egui::Painter, r: Rect) {
    let rad = v.globe_cam().radius_px() as f32 * r.width() / v.px.width().max(1.0);
    pt.rect_filled(r, 0.0, Color32::from_rgb(5, 7, 12));
    pt.circle_filled(r.center(), rad + 2.0, Color32::from_rgb(52, 84, 128));
    pt.circle_filled(r.center(), rad, Color32::from_rgb(20, 30, 46));
}

/// Globe view, above the data: meridians and parallels. The interval depends on the visible part.
fn graticule(v: &crate::view::View, pt: &egui::Painter, r: Rect) {
    let g = v.globe_cam();
    let (cap, [lon0, lat0]) = (g.cap(), v.center);
    let step = [30.0, 15.0, 10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1, 0.05, 0.02].into_iter().find(|s| cap / s >= 2.5).unwrap_or(0.01);
    let k = r.width() / v.px.width().max(1.0);
    let span = if lat0.abs() + cap >= 89.0 { 180.0 } else { (cap / lat0.to_radians().cos()).min(180.0) };
    let (la0, la1) = ((lat0 - cap).max(-90.0), (lat0 + cap).min(90.0));
    let line = |pts: &mut dyn Iterator<Item = (f64, f64)>, strong: bool| {
        let s = Stroke::new(1.0, Color32::from_white_alpha(if strong { 70 } else { 34 }));
        let mut last: Option<Pos2> = None;
        for (lon, lat) in pts {
            let q = g.project(lon, lat).map(|p| r.min + vec2(p[0] as f32, p[1] as f32) * k);
            if let (Some(a), Some(b)) = (last, q) {
                pt.line_segment([a, b], s);
            }
            last = q;
        }
    };
    const N: usize = 48;
    let mut lon = ((lon0 - span) / step).ceil() * step;
    while lon <= lon0 + span && lon < lon0 - span + 360.0 {
        line(&mut (0..=N).map(|i| (lon, la0 + (la1 - la0) * i as f64 / N as f64)), lon.rem_euclid(360.0) == 0.0);
        lon += step;
    }
    let mut lat = (la0 / step).ceil() * step;
    while lat <= la1 {
        if lat.abs() < 90.0 {
            line(&mut (0..=2 * N).map(|i| (lon0 - span + 2.0 * span * i as f64 / (2 * N) as f64, lat)), lat == 0.0);
        }
        lat += step;
    }
}

fn compare_ui(ui: &mut egui::Ui, p: &mut Pane) {
    if p.specs.len() < 2 {
        ui.colored_label(ui.visuals().error_fg_color, t("Compare needs two visible layers: add a layer (Ctrl+Shift+O, or Shift + drop)."));
        return;
    }
    let name = |k: usize| p.spec_layer.get(k).map_or(String::new(), |&i| p.layers[i].label(36));
    ui.small(format!("A: {}\nB: {}", name(0), name(1)));
    match p.cmp {
        Cmp::Swipe => {
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut p.swipe, 0.0..=1.0).show_value(false)).on_hover_text(t("Line position. You can also drag the line in the view"));
                ui.checkbox(&mut p.vertical, t("Vertical (V)"));
            });
        }
        Cmp::Blend => {
            ui.horizontal(|ui| {
                ui.label(t("B opacity"));
                ui.add(egui::Slider::new(&mut p.blend, 0.0..=1.0));
            });
        }
        Cmp::Flicker => {
            ui.horizontal(|ui| {
                ui.label(t("Rate"));
                ui.add(egui::Slider::new(&mut p.flicker_hz, 0.5..=10.0).suffix(" /s").logarithmic(true));
            });
        }
        Cmp::Difference => {
            ui.horizontal(|ui| {
                let d = p.diff;
                ui.selectable_value(&mut p.diff, 0, "A - B");
                ui.selectable_value(&mut p.diff, 1, "A / B");
                ui.selectable_value(&mut p.diff, 2, "10 log10(A / B)");
                if d != p.diff {
                    p.diff_range();
                }
            });
            ui.horizontal(|ui| {
                let speed = ((p.dhi - p.dlo).abs() / 300.0).max(1e-6) as f64;
                ui.add(egui::DragValue::new(&mut p.dlo).speed(speed).max_decimals(4)).on_hover_text(t("Minimum"));
                ui.add(egui::DragValue::new(&mut p.dhi).speed(speed).max_decimals(4)).on_hover_text(t("Maximum"));
                if ui.button(t("Auto")).clicked() {
                    p.diff_range();
                }
                ui.checkbox(&mut p.dinvert, t("Invert"));
            });
            if let Some(i) = swatches(ui, p.dcmap, p.dinvert) {
                p.dcmap = i;
            }
        }
        Cmp::Off => {}
    }
}

/// Overlays of a view: compare labels, swipe line, crosshair of the linked cursor, scale bar, errors.
fn overlays(p: &Pane, pt: &egui::Painter, ppp: f32, mpp: Option<f64>, cross: Option<[f64; 2]>) {
    let r = p.rect;
    let shadow = Stroke::new(3.0, Color32::from_black_alpha(160));
    let label = |pos: Pos2, align: Align2, s: &str, c: Color32| {
        let g = pt.layout_no_wrap(s.to_string(), FontId::proportional(13.0), c);
        let rr = align.anchor_size(pos, g.size()).expand(4.0);
        pt.rect_filled(rr, 3.0, Color32::from_black_alpha(150));
        pt.galley(rr.min + vec2(4.0, 4.0), g, c);
    };
    if p.specs.len() >= 2 && p.cmp != Cmp::Off {
        let name = |k: usize| p.spec_layer.get(k).map_or(String::new(), |&i| p.layers[i].label(36));
        match p.cmp {
            Cmp::Swipe => {
                let (a, b) = if p.vertical {
                    let x = r.left() + p.swipe * r.width();
                    for (s, w) in [(shadow, 0.0), (Stroke::new(1.5, Color32::WHITE), 0.0)] {
                        pt.line_segment([egui::pos2(x + w, r.top()), egui::pos2(x + w, r.bottom())], s);
                    }
                    pt.circle(egui::pos2(x, r.center().y), 9.0, Color32::from_black_alpha(160), Stroke::new(1.5, Color32::WHITE));
                    (egui::pos2(x - 8.0, r.top() + 34.0), egui::pos2(x + 8.0, r.top() + 34.0))
                } else {
                    let y = r.top() + p.swipe * r.height();
                    for s in [shadow, Stroke::new(1.5, Color32::WHITE)] {
                        pt.line_segment([egui::pos2(r.left(), y), egui::pos2(r.right(), y)], s);
                    }
                    pt.circle(egui::pos2(r.center().x, y), 9.0, Color32::from_black_alpha(160), Stroke::new(1.5, Color32::WHITE));
                    (egui::pos2(r.left() + 8.0, y - 8.0), egui::pos2(r.left() + 8.0, y + 8.0))
                };
                label(a, if p.vertical { Align2::RIGHT_TOP } else { Align2::LEFT_BOTTOM }, &format!("A  {}", name(0)), Color32::WHITE);
                label(b, if p.vertical { Align2::LEFT_TOP } else { Align2::LEFT_TOP }, &format!("B  {}", name(1)), Color32::WHITE);
            }
            Cmp::Flicker => {
                let s = if p.v.inputs.is_empty() { String::new() } else { format!("{}  A {}  /  B {}", t("Flicker"), name(0), name(1)) };
                label(r.left_top() + vec2(8.0, 8.0), Align2::LEFT_TOP, &s, Color32::WHITE);
            }
            Cmp::Blend => label(r.left_top() + vec2(8.0, 8.0), Align2::LEFT_TOP, &format!("{}  B {}  {:.0} %", t("Blend"), name(1), p.blend * 100.0), Color32::WHITE),
            Cmp::Difference => {
                let op = ["A - B", "A / B", "10 log10(A / B)"][p.diff as usize % 3];
                label(r.left_top() + vec2(8.0, 8.0), Align2::LEFT_TOP, &format!("{op}   A {}   B {}", name(0), name(1)), Color32::WHITE);
            }
            Cmp::Off => {}
        }
    }
    if let Some(c) = cross {
        let q = p.v.to_screen(c, r);
        if r.contains(q) {
            for s in [shadow, Stroke::new(1.0, Color32::from_rgb(255, 230, 80))] {
                pt.line_segment([egui::pos2(r.left(), q.y), egui::pos2(r.right(), q.y)], s);
                pt.line_segment([egui::pos2(q.x, r.top()), egui::pos2(q.x, r.bottom())], s);
            }
        }
    }
    // Scale bar: a length of 1, 2 or 5 times a power of 10, at most 120 points.
    let per_pt = match (mpp, p.v.space) {
        (Some(m), _) => Some((m * ppp as f64, "m")),
        (None, None) => Some((ppp as f64 / p.v.scale, "px")),
        _ => None,
    };
    if let Some((u, unit)) = per_pt
        && u.is_finite()
        && u > 0.0
    {
        let raw = u * 120.0;
        let e = 10f64.powf(raw.log10().floor());
        let nice = [5.0, 2.0, 1.0].into_iter().map(|k| k * e).find(|&v| v <= raw).unwrap_or(e);
        let w = (nice / u) as f32;
        let a = r.left_bottom() + vec2(12.0, -14.0);
        let b = a + vec2(w, 0.0);
        let txt = match unit {
            "m" if nice >= 1000.0 => format!("{} km", nice / 1000.0),
            "m" => format!("{nice} m"),
            _ => format!("{nice} px"),
        };
        for s in [shadow, Stroke::new(1.5, Color32::WHITE)] {
            pt.line_segment([a, b], s);
            pt.line_segment([a, a - vec2(0.0, 5.0)], s);
            pt.line_segment([b, b - vec2(0.0, 5.0)], s);
        }
        pt.text(a + vec2(w / 2.0, -6.0), Align2::CENTER_BOTTOM, txt, FontId::proportional(12.0), Color32::WHITE);
    }
    if let Some(e) = &p.err {
        label(r.left_bottom() + vec2(8.0, -30.0), Align2::LEFT_BOTTOM, e, RED);
    }
}

/// File dialogs (they block: after the frame).
pub fn dialogs(app: &mut App) {
    let Some(d) = app.dialog.take() else { return };
    let ws = [WORKSPACE_EXT];
    match d {
        Dialog::Open { pane, add, what } => {
            let d = rfd::FileDialog::new();
            let f = match what {
                What::Kind(k) if OPEN_KINDS[k].1 => d.set_title(tf("Open {}: select the product directories", &[t(OPEN_KINDS[k].0)])).pick_folders(),
                What::Kind(k) => d.set_title(tf("Open {}", &[t(OPEN_KINDS[k].0)])).add_filter(t(OPEN_KINDS[k].0), OPEN_KINDS[k].2).add_filter(t("All files"), &["*"]).pick_files(),
                What::Dirs => d.set_title(t("Open product directories (SAFE, SEN3, Zarr)")).pick_folders(),
                What::Series => d.set_title(t("Open files as the time steps of one layer")).add_filter(t("EO data"), ALL_EXT).add_filter(t("All files"), &["*"]).pick_files(),
                _ => d.add_filter(t("EO data and workspaces"), ALL_EXT).add_filter(t("All files"), &["*"]).pick_files(),
            };
            if let Some(v) = f {
                let v = v.into_iter().map(|p| p.to_string_lossy().into_owned()).collect();
                if what == What::Series { app.open_series(pane, v, add) } else { app.open_many(pane, v, add) }
            }
        }
        Dialog::Save => {
            if let Some(p) = rfd::FileDialog::new().add_filter(t("eoview workspace"), &ws).set_file_name(format!("workspace.{WORKSPACE_EXT}")).save_file() {
                if let Err(e) = app.save_workspace(&p.to_string_lossy()) {
                    app.error = Some(e);
                }
            }
        }
        Dialog::Export(pane) => {
            let name = app.pane(pane).and_then(|p| p.layers.get(p.sel)).map_or("layer".into(), |l| l.comp_name().replace(['/', '\\', ':', ' '], "_"));
            if let Some(p) = rfd::FileDialog::new().set_title(t("Export data (GeoTIFF)")).add_filter("GeoTIFF", &["tif", "tiff"]).set_file_name(format!("{name}.tif")).save_file() {
                app.export(pane, p.to_string_lossy().into_owned());
            }
        }
        Dialog::Figure(pane, ext) => {
            let name = format!("figure.{ext}");
            if let Some(p) = rfd::FileDialog::new().set_title(t("Export the figure")).add_filter(ext.to_uppercase(), &[ext]).set_file_name(name).save_file() {
                let mut path = p.to_string_lossy().into_owned();
                if !path.to_lowercase().ends_with(&format!(".{ext}")) {
                    path = format!("{path}.{ext}");
                }
                app.fig.msg = Some(match app.figure_export(pane, &path) {
                    Ok(p) => tf("Written: {}", &[&p]),
                    Err(e) => e,
                });
            }
        }
        Dialog::PyOpen => {
            if let Some(v) = rfd::FileDialog::new().set_title(t("Open Python scripts or notebooks")).add_filter(t("Python"), &["py", "ipynb"]).pick_files() {
                v.iter().for_each(|f| app.py_open(f));
            }
        }
        Dialog::PySave(k) => {
            let (name, nb) = app.py.as_ref().and_then(|p| p.docs.get(k)).map_or(("script.py".into(), false), |d| (d.name.clone(), d.nb));
            let ext: &[&str] = if nb { &["ipynb"] } else { &["py"] };
            if let Some(f) = rfd::FileDialog::new().set_title(t("Save as")).add_filter(t("Python"), ext).set_file_name(name).save_file() {
                app.py_save(k, Some(f));
            }
        }
        Dialog::RenderOut => {
            let d = rfd::FileDialog::new().set_title(t("Output of the render")).add_filter(t("Video"), crate::render::VIDEO_EXT).set_file_name("eoview.mp4");
            if let Some(p) = d.save_file() {
                app.render_set.out = p.to_string_lossy().into_owned();
            }
        }
        Dialog::Load => {
            if let Some(p) = rfd::FileDialog::new().add_filter(t("eoview workspace"), &ws).pick_file() {
                app.load_workspace(&p.to_string_lossy());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy;

    #[test]
    fn fuzzy_ranks_close_matches_first() {
        assert_eq!(fuzzy("", "Fit"), Some(0));
        assert!(fuzzy("xyz", "Fit").is_none());
        assert!(fuzzy("ndvi", "Preset: NDVI").is_some());
        assert!(fuzzy("vir", "Color map: Viridis").unwrap() < fuzzy("vir", "Compare: Difference").unwrap_or(usize::MAX));
        assert!(fuzzy("fit", "Fit").unwrap() < fuzzy("fit", "Show or hide side panel - first item").unwrap_or(usize::MAX));
    }
}
