//! Themes of the interface. A theme is a data file (JSON): colors, corner radius, spacing and font size.
//! eoview has three themes (dark, light, high contrast). The user can add themes: a `.json` file in the
//! `themes` directory of the configuration directory. A field that is not in a file has the value of
//! the dark theme.
use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub name: String,
    /// The theme starts from the dark colors of egui (else the light colors).
    pub dark: bool,
    /// Panels, windows and menus.
    pub background: [u8; 3],
    /// Text fields, histograms, timeline.
    pub field: [u8; 3],
    pub text: [u8; 3],
    /// Lines of the separators and of the windows, and the frame of the buttons.
    pub border: [u8; 3],
    /// Selection, links, the frame of the active view.
    pub accent: [u8; 3],
    pub warning: [u8; 3],
    pub error: [u8; 3],
    /// Corner radius of the buttons and of the windows, in points.
    pub radius: f32,
    /// Space between the items, in points.
    pub spacing: f32,
    /// Size of the text, in points.
    pub font: f32,
}

impl Default for Theme {
    fn default() -> Theme {
        Theme {
            name: "Dark".into(),
            dark: true,
            background: [27, 27, 30],
            field: [12, 12, 14],
            text: [200, 200, 205],
            border: [60, 60, 66],
            accent: [90, 170, 255],
            warning: [225, 165, 40],
            error: [255, 110, 110],
            radius: 3.0,
            spacing: 6.0,
            font: 13.0,
        }
    }
}

const BUILT_IN: [&str; 3] = [include_str!("../assets/themes/dark.json"), include_str!("../assets/themes/light.json"), include_str!("../assets/themes/high-contrast.json")];

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// The themes of eoview, then the themes of the user (`dir`). A theme of the user with the name of an
/// other theme replaces it.
pub fn all(dir: Option<&std::path::Path>) -> Vec<Theme> {
    let mut out: Vec<Theme> = BUILT_IN.iter().filter_map(|s| serde_json::from_str(s).ok()).collect();
    let mut files: Vec<_> = dir.and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    for f in files {
        match std::fs::read_to_string(&f).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str::<Theme>(&s).map_err(|e| e.to_string())) {
            Ok(t) => {
                out.retain(|o| o.name != t.name);
                out.push(t);
            }
            Err(e) => eprintln!("{}: {e}", f.display()),
        }
    }
    out
}

impl Theme {
    /// The egui style of the theme.
    pub fn style(&self) -> egui::Style {
        let mut s = egui::Style { visuals: if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() }, ..Default::default() };
        let (bg, field, text, border, accent) = (rgb(self.background), rgb(self.field), rgb(self.text), rgb(self.border), rgb(self.accent));
        let v = &mut s.visuals;
        (v.panel_fill, v.window_fill, v.extreme_bg_color, v.code_bg_color) = (bg, bg, field, field);
        v.faint_bg_color = bg.lerp_to_gamma(text, 0.04);
        (v.warn_fg_color, v.error_fg_color, v.hyperlink_color) = (rgb(self.warning), rgb(self.error), accent);
        v.selection.bg_fill = accent.gamma_multiply(if self.dark { 0.45 } else { 0.35 });
        v.selection.stroke = Stroke::new(1.0, if self.dark { Color32::WHITE } else { Color32::BLACK });
        v.window_stroke = Stroke::new(1.0, border);
        let r = CornerRadius::same(self.radius.round().clamp(0.0, 255.0) as u8);
        (v.window_corner_radius, v.menu_corner_radius) = (r, r);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = bg;
        w.noninteractive.weak_bg_fill = bg;
        w.noninteractive.bg_stroke = Stroke::new(1.0, border);
        w.noninteractive.fg_stroke.color = text;
        w.inactive.fg_stroke.color = text;
        w.inactive.bg_stroke = Stroke::new(if border == bg { 0.0 } else { 0.5 }, border);
        w.inactive.weak_bg_fill = bg.lerp_to_gamma(text, 0.10);
        w.inactive.bg_fill = bg.lerp_to_gamma(text, 0.16);
        w.hovered.weak_bg_fill = bg.lerp_to_gamma(text, 0.22);
        w.hovered.bg_fill = w.hovered.weak_bg_fill;
        w.hovered.bg_stroke = Stroke::new(1.0, accent);
        for x in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            x.corner_radius = r;
        }
        let sp = self.spacing.clamp(0.0, 30.0).round();
        s.spacing.item_spacing = egui::vec2(sp + 2.0, (sp * 0.66).round());
        s.spacing.button_padding = egui::vec2(sp, (sp * 0.33).round());
        let f = self.font.clamp(8.0, 30.0);
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(f * 0.75)),
            (TextStyle::Body, FontId::proportional(f)),
            (TextStyle::Button, FontId::proportional(f)),
            (TextStyle::Heading, FontId::proportional(f * 1.4)),
            (TextStyle::Monospace, FontId::monospace(f * 0.95)),
        ]
        .into();
        s
    }

    /// Use the theme in context `ctx` (the main window or the window of a detached view).
    pub fn apply(&self, ctx: &egui::Context) {
        let t = if self.dark { egui::Theme::Dark } else { egui::Theme::Light };
        ctx.set_theme(t);
        ctx.set_style_of(t, self.style());
    }

    /// The color of the background of a frame, for the clear of the GPU target.
    pub fn clear(&self) -> wgpu::Color {
        let c = |v: u8| v as f64 / 255.0;
        wgpu::Color { r: c(self.background[0]), g: c(self.background[1]), b: c(self.background[2]), a: 1.0 }
    }
}

#[cfg(test)]
mod tests {
    /// The themes of eoview read without an error, and they have different names.
    #[test]
    fn built_in_themes_read() {
        let t = super::all(None);
        assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Dark", "Light", "High contrast"]);
        assert!(!t[1].dark && t[1].style().visuals.panel_fill != t[0].style().visuals.panel_fill);
    }
}
