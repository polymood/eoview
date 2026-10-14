//! Preferences of the user (`prefs.json` in the configuration directory), and the preferences window: a
//! list of sections with icons, a search field, and the settings of the selected section.
use crate::app::App;
use crate::icons::{self, Icon};
use crate::lang::{t, tf};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The views use the finest level of the data, not the level of the zoom.
    pub full_res: bool,
    /// Path of the `ffmpeg` program for the renders. Empty: the directory of the executable, then the
    /// search path of the system.
    pub ffmpeg: String,
    /// Name of the theme. Empty: the first theme (dark).
    pub theme: String,
    /// Language of the interface: "en" or "fr". Empty: English.
    pub lang: String,
    /// Memory budgets in MB, for the next start. 0: the default (a quarter of the RAM, 1 GB of GPU memory,
    /// 10 GB of disk). The environment variables EOVIEW_RAM_MB, EOVIEW_GPU_MB and EOVIEW_DISK_MB have priority.
    pub ram_mb: usize,
    pub gpu_mb: usize,
    pub disk_mb: usize,
    /// Directory of the disk cache, for the next start. Empty: `eoview/remote` in the cache directory of the system.
    pub cache_dir: String,
    /// The Python program for the scripts. Empty: `python3` (`python` on Windows) of the search path.
    pub python: String,
    /// The automatic range of a new layer, and the percent of the clip (0: 2 %).
    pub stretch: crate::layer::StretchInit,
    pub clip: f32,
    /// Each new layer of a view keeps its own automatic range. False: the layers of a view that show the
    /// same bands get the same range (the union of their automatic ranges, or the range of the user).
    pub own_stretch: bool,
    /// The products of the views do not open again when they change on the disk.
    pub no_reload: bool,
    /// Local files without a memory map: an other program can rewrite or replace a product that a view
    /// shows (Windows does not permit this for a mapped file). Slower reads.
    pub no_map: bool,
    /// No automatic update (`update`).
    pub no_update: bool,
    /// More space between the items of the interface (the default is compact, as a GIS).
    pub comfortable: bool,
    /// The tab of the layer panel: 0 Data, 1 Display, 2 Analysis.
    pub side_tab: u8,
    /// The left panel shows the products (else the layers).
    pub side_products: bool,
}

/// The configuration directory of eoview: `%APPDATA%\eoview` on Windows, `$XDG_CONFIG_HOME/eoview` or
/// `~/.config/eoview` on the other systems.
pub fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.map(|d| d.join(crate::APP))
}

impl Prefs {
    pub fn load() -> Prefs {
        config_dir().and_then(|d| std::fs::read_to_string(d.join("prefs.json")).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Section {
    General,
    Appearance,
    ColorMaps,
    Performance,
    Network,
    Render,
    Python,
    Keys,
    About,
}

const SECTIONS: [(Section, &str, Icon); 9] = [
    (Section::General, "General", Icon::Gear),
    (Section::Appearance, "Appearance", Icon::Palette),
    (Section::ColorMaps, "Color maps", Icon::ColorBar),
    (Section::Performance, "Performance", Icon::Gauge),
    (Section::Network, "Network", Icon::Cloud),
    (Section::Render, "Render", Icon::Film),
    (Section::Python, "Python", Icon::Code),
    (Section::Keys, "Keys", Icon::Keyboard),
    (Section::About, "About", Icon::Info),
];

/// The settings: section, name, explanation. The search field finds a setting by its name and its explanation.
#[derive(Clone, Copy, PartialEq)]
enum Item {
    Lang,
    Recent,
    Theme,
    Density,
    ColorMaps,
    Stretch,
    SameStretch,
    Reload,
    Update,
    NoMap,
    FullRes,
    Ram,
    Gpu,
    Disk,
    CacheDir,
    Ffmpeg,
    Python,
    Keys,
    About,
}

const ITEMS: [(Item, Section, &str, &str); 19] = [
    (Item::Lang, Section::General, "Language", "The language of the interface."),
    (Item::Recent, Section::General, "Recent products", "The list of the recent products and workspaces of the File menu."),
    (Item::Theme, Section::Appearance, "Theme", "Colors, corners, spacing and size of the text. To add a theme, put a JSON file in the themes directory."),
    (Item::ColorMaps, Section::ColorMaps, "Color maps", "The color maps of eoview (matplotlib, ColorBrewer, cmocean, the Scientific colour maps of Crameri) and your color maps. Import files of other programs (GMT .cpt, Adobe .act, NCL .rgb, GIMP .gpl and .ggr, JASC .pal, text files of RGB rows, for example the tables of Panoply), or make a map with the editor."),
    (Item::Density, Section::Appearance, "Density", "The space between the items of the interface. Compact: more on the screen, as in a GIS."),
    (Item::Stretch, Section::Appearance, "Range of a new layer", "The automatic stretch of a new layer, and of the Auto button (A). Minimum to maximum: all the values of the data. Clip: without a part of the values at each end, for more contrast. Data type: 0 to 255 for 8-bit data, 0 to 65535 for 16-bit data (with the scale and the offset of the file)."),
    (Item::SameStretch, Section::Appearance, "Same range for the layers of a view", "The layers of a view that show the same bands (for example the orbits of a day, or the tiles of an area) get the same range: no visible edge between them. The range is the union of their automatic ranges, or the range that you set on one of them."),
    (Item::Reload, Section::General, "Reload the products that change", "A product of a view that a program writes again (for example the output of a processing) opens again in the view, with the same settings. eoview checks the local products each second."),
    (Item::Update, Section::General, "Automatic updates", "At the start, eoview downloads and installs its latest version. The new version runs at the next start. For the next start."),
    (Item::NoMap, Section::Performance, "Other programs can rewrite open products", "eoview reads the local files without a memory map. On Windows, a program can then rewrite or replace a product that a view shows. The products load slower. For the products that open after the change."),
    (Item::FullRes, Section::Performance, "Full resolution at all zoom levels", "The views use the finest level of the data, not the level of the zoom. If the GPU memory does not have room for the tiles of a view, the view uses the finest level that has room."),
    (Item::Ram, Section::Performance, "Memory budget (MB)", "Memory for the data in RAM. 0: a quarter of the RAM of the system. For the next start."),
    (Item::Gpu, Section::Performance, "GPU memory budget (MB)", "Memory for the tiles on the GPU. 0: 1024 MB. For the next start."),
    (Item::Disk, Section::Network, "Disk cache budget (MB)", "Disk space for the remote data. 0: 10 GB. For the next start."),
    (Item::CacheDir, Section::Network, "Disk cache directory", "Empty: the cache directory of the system. For the next start."),
    (Item::Ffmpeg, Section::Render, "Path of ffmpeg", "The program that writes the video files of a render. Empty: next to eoview, then the search path."),
    (Item::Python, Section::Python, "Python program", "The Python of the scripts, with numpy (and xarray, matplotlib if the scripts use them). Empty: python3 on the search path. A path of a virtual environment or of conda is possible."),
    (Item::Keys, Section::Keys, "Keys", "The keys of the commands."),
    (Item::About, Section::About, "About", "Version, GPU, license."),
];

/// State of the preferences window.
pub struct PrefsWin {
    pub section: Section,
    pub search: String,
}

impl Default for PrefsWin {
    fn default() -> Self {
        PrefsWin { section: Section::General, search: String::new() }
    }
}

impl App {
    /// The color maps of the preferences: import, the editor, and the library by group.
    fn cmaps_ui(&mut self, ui: &mut egui::Ui) {
        let all = crate::colormaps::all();
        ui.horizontal(|ui| {
            if ui.button(t("Import files...")).on_hover_text(t("GMT .cpt, Adobe .act, NCL .rgb, GIMP .gpl and .ggr, JASC .pal, text files of RGB rows")).clicked() {
                self.dialog = Some(crate::app::Dialog::ImportCmaps);
            }
            if ui.button(t("New color map")).clicked() {
                self.cmap_edit = Some((t("My color map").into(), vec![[0, 0, 128], [255, 255, 255], [128, 0, 0]]));
            }
            if let Some(d) = crate::colormaps::user_dir() {
                ui.small(d.to_string_lossy()).on_hover_text(t("The folder of your color maps (JSON files)"));
            }
        });
        if let Some(m) = &self.cmap_msg {
            ui.small(m);
        }
        // The editor: a name, the colors (at equal distances), the gradient.
        if let Some((name, stops)) = &mut self.cmap_edit {
            let mut done = None;
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(t("Name"));
                    ui.text_edit_singleline(name);
                });
                let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(420.0), 20.0), egui::Sense::hover());
                crate::ui::gradient_on(ui.painter(), r, stops, false);
                ui.horizontal_wrapped(|ui| {
                    for s in stops.iter_mut() {
                        ui.color_edit_button_srgb(s);
                    }
                    if ui.small_button("+").on_hover_text(t("Add a color")).clicked() {
                        stops.push(*stops.last().unwrap());
                    }
                    if stops.len() > 2 && ui.small_button("-").on_hover_text(t("Remove the last color")).clicked() {
                        stops.pop();
                    }
                    if ui.small_button(t("Reverse")).clicked() {
                        stops.reverse();
                    }
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("cmap from").selected_text(t("Start from a map")).show_ui(ui, |ui| {
                        for c in all.iter() {
                            if ui.selectable_label(false, &c.name).clicked() {
                                *stops = crate::colormaps::thin(c.stops.clone(), 12);
                            }
                        }
                    });
                    if ui.button(t("Save")).clicked() {
                        done = Some(true);
                    }
                    if ui.button(t("Cancel")).clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    self.cmap_msg = Some(match crate::colormaps::save_user(name, stops, "eoview") {
                        Ok(()) => tf("Saved: {}", &[name]),
                        Err(e) => e,
                    });
                    self.cmap_edit = None;
                }
                Some(false) => self.cmap_edit = None,
                None => {}
            }
        }
        // The library, by group: your maps first.
        let groups = crate::ui::cmap_groups();
        for (g, gname) in groups.iter().rev().take(1).chain(groups.iter().take(groups.len() - 1)) {
            let ids = crate::ui::cmaps_of(&all, g);
            if ids.is_empty() {
                continue;
            }
            egui::CollapsingHeader::new(format!("{gname}  ({})", ids.len())).id_salt(("cmaps", g)).default_open(*g == "User").show(ui, |ui| {
                for i in ids {
                    let c = &all[i];
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(150.0, 14.0), egui::Sense::hover());
                        crate::ui::gradient_on(ui.painter(), r, &c.stops, false);
                        ui.label(&c.name).on_hover_text(&c.source);
                        if ui.small_button(if c.group == "User" { t("Edit") } else { t("Copy to edit") }).clicked() {
                            let n = if c.group == "User" { c.name.trim_end_matches(" (user)").to_string() } else { format!("{} 2", c.name) };
                            self.cmap_edit = Some((n, crate::colormaps::thin(c.stops.clone(), 24)));
                        }
                        if c.group == "User" && ui.small_button(t("Delete")).clicked() {
                            crate::colormaps::delete_user(&c.name);
                            self.cmap_msg = Some(tf("Deleted: {}", &[&c.name]));
                        }
                    });
                }
            });
        }
    }

    /// Write the preferences next to the list of the recent products (not in the tests and the benchmarks).
    pub fn save_prefs(&self) {
        let Some(f) = self.recent_file.as_ref().map(|f| f.with_file_name("prefs.json")) else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.prefs) {
            let _ = std::fs::write(f, s);
        }
    }

    /// The theme of the preferences (the first theme if no theme has this name).
    pub fn theme(&self) -> &crate::theme::Theme {
        self.themes.iter().find(|t| t.name == self.prefs.theme).unwrap_or(&self.themes[0])
    }

    /// Use the theme and the language of the preferences in all windows.
    pub fn apply_prefs(&self) {
        crate::lang::set(&self.prefs.lang);
        eo_io::source::set_no_map(self.prefs.no_map);
        crate::layer::set_stretch_init(self.prefs.stretch, if self.prefs.clip > 0.0 { self.prefs.clip } else { 2.0 });
        let th = self.theme();
        th.apply(&self.ctx, !self.prefs.comfortable);
        self.wins.iter().for_each(|d| th.apply(&d.ctx, !self.prefs.comfortable));
    }

    /// Preferences window. A change goes to the preferences file immediately.
    pub fn prefs_ui(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.prefs_open.take() else { return };
        let (mut open, mut changed) = (true, false);
        egui::Window::new(t("Preferences")).open(&mut open).collapsible(false).resizable(true).default_size([660.0, 440.0]).show(ctx, |ui| {
            ui.add(egui::TextEdit::singleline(&mut w.search).hint_text(t("Search the settings")).desired_width(f32::INFINITY));
            ui.separator();
            let q = w.search.trim().to_lowercase();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    for (s, name, icon) in SECTIONS {
                        if icons::row(ui, icon, t(name), q.is_empty() && w.section == s).clicked() {
                            (w.section, w.search) = (s, String::new());
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // With a search: the settings of all sections that match, with the name of their section.
                    let hits: Vec<_> = ITEMS.iter().filter(|i| if q.is_empty() { i.1 == w.section } else { [t(i.2), t(i.3), i.2, i.3].iter().any(|s| s.to_lowercase().contains(&q)) }).collect();
                    if hits.is_empty() {
                        ui.weak(t("No setting has this text."));
                    }
                    for &&(item, sec, name, help) in &hits {
                        if !q.is_empty() {
                            ui.small(t(SECTIONS.iter().find(|s| s.0 == sec).map_or("", |s| s.1)));
                        }
                        if !matches!(item, Item::Keys | Item::About) {
                            ui.strong(t(name));
                        }
                        changed |= self.item_ui(ui, item);
                        if !matches!(item, Item::Keys | Item::About) {
                            ui.label(egui::RichText::new(t(help)).small().weak());
                        }
                        ui.add_space(10.0);
                    }
                }));
            });
        });
        if changed {
            self.apply_prefs();
            self.save_prefs();
        }
        self.prefs_open = open.then_some(w);
    }

    /// The control of one setting. True if the setting changed.
    fn item_ui(&mut self, ui: &mut egui::Ui, item: Item) -> bool {
        let p = &mut self.prefs;
        let mb = |ui: &mut egui::Ui, v: &mut usize| ui.add(egui::DragValue::new(v).range(0..=1 << 22).speed(16)).changed();
        match item {
            Item::Lang => {
                let mut c = false;
                ui.horizontal(|ui| {
                    for (code, name) in crate::lang::LANGS {
                        let on = p.lang == code || (p.lang.is_empty() && code == "en");
                        if ui.selectable_label(on, name).clicked() && !on {
                            p.lang = code.into();
                            c = true;
                        }
                    }
                });
                c
            }
            Item::Recent => {
                let n = self.recent.len();
                let b = ui.add_enabled(n > 0, egui::Button::new(t("Clear the list")));
                if b.clicked() {
                    self.recent.clear();
                    if let Some(f) = &self.recent_file {
                        let _ = std::fs::write(f, "[]");
                    }
                }
                false
            }
            Item::Theme => {
                let mut c = false;
                let cur = self.themes.iter().find(|x| x.name == p.theme).unwrap_or(&self.themes[0]).name.clone();
                ui.horizontal_wrapped(|ui| {
                    for th in &self.themes {
                        if ui.selectable_label(th.name == cur, &th.name).clicked() && th.name != cur {
                            p.theme = th.name.clone();
                            c = true;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if let Some(d) = config_dir().map(|d| d.join("themes")) {
                        ui.small(d.to_string_lossy());
                    }
                    if ui.small_button(t("Read the themes again")).clicked() {
                        self.themes = crate::theme::all(config_dir().map(|d| d.join("themes")).as_deref());
                        c = true;
                    }
                });
                c
            }
            Item::ColorMaps => {
                self.cmaps_ui(ui);
                false
            }
            Item::Density => {
                let mut c = false;
                ui.horizontal(|ui| {
                    c |= ui.selectable_value(&mut p.comfortable, false, t("Compact")).changed();
                    c |= ui.selectable_value(&mut p.comfortable, true, t("Comfortable")).changed();
                });
                c
            }
            Item::Stretch => {
                use crate::layer::StretchInit as S;
                let mut c = false;
                ui.horizontal(|ui| {
                    for (v, n) in [(S::MinMax, "Minimum to maximum"), (S::Clip, "Clip"), (S::Type, "Data type")] {
                        c |= ui.selectable_value(&mut p.stretch, v, t(n)).changed();
                    }
                });
                if p.stretch == S::Clip {
                    if p.clip <= 0.0 {
                        p.clip = 2.0;
                    }
                    c |= ui.add(egui::Slider::new(&mut p.clip, 0.1..=10.0).suffix(" %")).changed();
                }
                c
            }
            Item::Reload => {
                let mut on = !p.no_reload;
                let c = ui.checkbox(&mut on, t("On")).changed();
                p.no_reload = !on;
                c
            }
            Item::Update => {
                let mut on = !p.no_update;
                let c = ui.checkbox(&mut on, t("On")).changed();
                p.no_update = !on;
                c
            }
            Item::NoMap => ui.checkbox(&mut p.no_map, t("On")).changed(),
            Item::SameStretch => {
                let mut on = !p.own_stretch;
                let c = ui.checkbox(&mut on, t("On")).changed();
                p.own_stretch = !on;
                c
            }
            Item::FullRes => ui.checkbox(&mut p.full_res, t("On")).changed(),
            Item::Ram => mb(ui, &mut p.ram_mb),
            Item::Gpu => mb(ui, &mut p.gpu_mb),
            Item::Disk => mb(ui, &mut p.disk_mb),
            Item::CacheDir => ui.add(egui::TextEdit::singleline(&mut p.cache_dir).desired_width(f32::INFINITY)).changed(),
            Item::Ffmpeg => {
                let c = ui.add(egui::TextEdit::singleline(&mut p.ffmpeg).hint_text(t("Empty: next to eoview, then the search path")).desired_width(f32::INFINITY)).changed();
                // `ffmpeg` runs one time to find it, not at each frame.
                if self.ffmpeg_found.is_none() || c {
                    self.ffmpeg_found = Some(crate::render::ffmpeg(&self.prefs.ffmpeg).is_some());
                }
                match self.ffmpeg_found {
                    Some(true) => ui.small(t("ffmpeg runs.")),
                    _ => ui.colored_label(ui.visuals().warn_fg_color, t("ffmpeg was not found: the renders write PNG files.")),
                };
                c
            }
            Item::Python => {
                let c = ui.add(egui::TextEdit::singleline(&mut p.python).hint_text(if cfg!(windows) { "python" } else { "python3" }).desired_width(f32::INFINITY)).changed();
                if let Some(d) = self.py.as_ref().and_then(|p| p.path.as_ref()) {
                    ui.small(tf("The module eoview for the notebooks: {} (or pip install the directory python of eoview)", &[&d.display().to_string()]));
                }
                c
            }
            Item::Keys => {
                crate::ui::keys_grid(self, ui);
                false
            }
            Item::About => {
                ui.heading(format!("{} {}", crate::APP, env!("CARGO_PKG_VERSION")));
                ui.label(t("Fast viewer for Earth observation data."));
                if let Some(w) = &self.win {
                    ui.label(format!("{} {}", t("GPU:"), w.name));
                }
                ui.label(format!("{} MIT OR Apache-2.0", t("License:")));
                match self.update.as_ref().and_then(|s| s.lock().unwrap().clone()) {
                    Some(Ok(v)) => drop(ui.label(tf("eoview {} is installed. It runs at the next start.", &[&v]))),
                    Some(Err(e)) => drop(ui.label(format!("{} {e}", t("Update:")))),
                    None => {}
                }
                ui.hyperlink("https://github.com/polymood/eoview");
                false
            }
        }
    }
}
