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
    Performance,
    Network,
    Render,
    Python,
    Keys,
    About,
}

const SECTIONS: [(Section, &str, Icon); 8] = [
    (Section::General, "General", Icon::Gear),
    (Section::Appearance, "Appearance", Icon::Palette),
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

const ITEMS: [(Item, Section, &str, &str); 12] = [
    (Item::Lang, Section::General, "Language", "The language of the interface."),
    (Item::Recent, Section::General, "Recent products", "The list of the recent products and workspaces of the File menu."),
    (Item::Theme, Section::Appearance, "Theme", "Colors, corners, spacing and size of the text. To add a theme, put a JSON file in the themes directory."),
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
        let th = self.theme();
        th.apply(&self.ctx);
        self.wins.iter().for_each(|d| th.apply(&d.ctx));
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
                ui.hyperlink("https://github.com/polymood/eoview");
                false
            }
        }
    }
}
