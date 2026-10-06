//! Splash screen: a BeOS window with the icon, the name and the version. It shows from the first frame
//! until the products of the command line are open.

use crate::app::App;
use egui::{Align2, Color32, FontId, Rect, Stroke, StrokeKind, pos2, vec2};
use std::time::{Duration, Instant};

/// Minimum time on the screen, in seconds.
const MIN_S: f32 = 1.2;
/// Side of the icon image (`assets/eoview-256.rgba`, RGBA without premultiplied alpha), in pixels.
const ICON_PX: usize = 256;

pub struct Splash {
    t0: Instant,
    icon: Option<egui::TextureHandle>,
}

impl Splash {
    pub fn start() -> Splash {
        Splash { t0: Instant::now(), icon: None }
    }
}

impl App {
    /// Paint the splash screen on top of the interface. A mouse button or Escape closes it.
    pub fn splash_ui(&mut self, ctx: &egui::Context) {
        let opens = self.opens_pending();
        let Some(s) = &self.splash else { return };
        let skip = ctx.input(|i| i.pointer.any_pressed() || i.key_pressed(egui::Key::Escape));
        if skip || (opens == 0 && s.t0.elapsed().as_secs_f32() >= MIN_S) {
            self.splash = None;
            return;
        }
        let Some(s) = &mut self.splash else { return };
        let icon = s
            .icon
            .get_or_insert_with(|| {
                let px = egui::ColorImage::from_rgba_unmultiplied([ICON_PX, ICON_PX], include_bytes!("../assets/eoview-256.rgba"));
                ctx.load_texture("splash", px, egui::TextureOptions::LINEAR)
            })
            .id();

        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("splash")));
        let screen = ctx.content_rect();
        p.rect_filled(screen, 0.0, Color32::from_black_alpha(150));
        let body = Rect::from_center_size(screen.center(), vec2(470.0, 176.0));
        let tab = Rect::from_min_size(body.left_top() - vec2(0.0, 25.0), vec2(130.0, 26.0));
        let line = Stroke::new(1.5, Color32::BLACK);
        p.rect_filled(body.translate(vec2(6.0, 6.0)), 0.0, Color32::from_black_alpha(90));
        p.rect_filled(body, 0.0, Color32::from_gray(222));
        p.rect_stroke(body, 0.0, line, StrokeKind::Inside);
        p.rect_filled(tab, 0.0, Color32::from_rgb(255, 203, 5));
        p.rect_stroke(tab, 0.0, line, StrokeKind::Inside);
        p.text(tab.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, crate::APP, FontId::proportional(15.0), Color32::BLACK);

        let image = Rect::from_min_size(body.left_top() + vec2(20.0, 24.0), vec2(128.0, 128.0));
        p.image(icon, image, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let x = image.right() + 22.0;
        p.text(pos2(x, body.top() + 22.0), Align2::LEFT_TOP, crate::APP, FontId::proportional(44.0), Color32::BLACK);
        p.text(pos2(x, body.top() + 78.0), Align2::LEFT_TOP, "Fast viewer for Earth observation data", FontId::proportional(15.0), Color32::from_gray(30));
        p.text(pos2(x, body.top() + 100.0), Align2::LEFT_TOP, format!("Version {}", env!("CARGO_PKG_VERSION")), FontId::proportional(12.0), Color32::from_gray(90));
        let status = match opens {
            0 => "Ready".to_string(),
            1 => "Opening 1 product...".to_string(),
            n => format!("Opening {n} products..."),
        };
        p.text(pos2(x, body.bottom() - 18.0), Align2::LEFT_BOTTOM, status, FontId::proportional(13.0), Color32::from_gray(30));
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}
