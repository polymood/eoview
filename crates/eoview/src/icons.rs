//! Icons of the interface. Each icon is drawn with lines and shapes: no emoji and no font glyphs, so the
//! icons do not depend on the fonts of the system.
use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

#[derive(Clone, Copy, PartialEq)]
pub enum Icon {
    Open,
    AddLayer,
    Save,
    Fit,
    ZoomIn,
    ZoomOut,
    /// Layout with columns and rows.
    Grid(u8, u8),
    Link,
    Compare,
    Globe,
    Search,
    Up,
    Down,
    Close,
    Play,
    Pause,
    /// One time step back or forward: a triangle and a bar.
    StepBack,
    StepForward,
    /// Sections of the preferences.
    Gear,
    Palette,
    Gauge,
    Cloud,
    Film,
    Keyboard,
    Info,
    /// Tools of a view.
    PixelGrid,
    Graticule,
    Ruler,
    Transect,
    Region,
    Pin,
    /// Code: the Python panel.
    Code,
}

/// Draw `icon` in the square `r`.
pub fn draw(p: &Painter, icon: Icon, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    let (l, t, rt, b, m) = (r.left(), r.top(), r.right(), r.bottom(), r.center());
    let line = |a: Pos2, b: Pos2| {
        p.line_segment([a, b], s);
    };
    let poly = |pts: Vec<Pos2>| {
        p.add(egui::Shape::closed_line(pts, s));
    };
    let fill = |pts: Vec<Pos2>| {
        p.add(egui::Shape::convex_polygon(pts, c, Stroke::NONE));
    };
    let w = r.width();
    match icon {
        Icon::Open => poly(vec![pos2(l, t + 0.2 * w), pos2(l + 0.38 * w, t + 0.2 * w), pos2(l + 0.5 * w, t + 0.34 * w), pos2(rt, t + 0.34 * w), pos2(rt, b - 0.12 * w), pos2(l, b - 0.12 * w)]),
        Icon::AddLayer => {
            // Two sheets, and a plus sign on the front sheet.
            poly(vec![pos2(l + 0.25 * w, t + 0.25 * w), pos2(l + 0.25 * w, t), pos2(rt, t), pos2(rt, b - 0.25 * w), pos2(rt - 0.25 * w, b - 0.25 * w)]);
            p.rect_stroke(Rect::from_min_max(pos2(l, t + 0.25 * w), pos2(rt - 0.25 * w, b)), 0.0, s, StrokeKind::Inside);
            let q = pos2(l + 0.375 * w, t + 0.625 * w);
            line(q - vec2(0.2 * w, 0.0), q + vec2(0.2 * w, 0.0));
            line(q - vec2(0.0, 0.2 * w), q + vec2(0.0, 0.2 * w));
        }
        Icon::Save => {
            poly(vec![pos2(l, t), pos2(rt - 0.22 * w, t), pos2(rt, t + 0.22 * w), pos2(rt, b), pos2(l, b)]);
            p.rect_stroke(Rect::from_min_max(pos2(l + 0.25 * w, t), pos2(rt - 0.32 * w, t + 0.3 * w)), 0.0, s, StrokeKind::Inside);
            p.rect_stroke(Rect::from_min_max(pos2(l + 0.22 * w, b - 0.42 * w), pos2(rt - 0.22 * w, b)), 0.0, s, StrokeKind::Inside);
        }
        Icon::Fit => {
            // The four corners of a frame.
            let k = 0.32 * w;
            for (x, y, dx, dy) in [(l, t, 1.0, 1.0), (rt, t, -1.0, 1.0), (rt, b, -1.0, -1.0), (l, b, 1.0, -1.0)] {
                line(pos2(x, y), pos2(x + dx * k, y));
                line(pos2(x, y), pos2(x, y + dy * k));
            }
            p.rect_filled(Rect::from_center_size(m, vec2(0.22 * w, 0.22 * w)), 0.0, c);
        }
        Icon::ZoomIn | Icon::ZoomOut | Icon::Search => {
            let (q, rad) = (pos2(l + 0.42 * w, t + 0.42 * w), 0.36 * w);
            p.circle_stroke(q, rad, s);
            line(q + vec2(0.72 * rad, 0.72 * rad), pos2(rt, b));
            if icon != Icon::Search {
                line(q - vec2(0.55 * rad, 0.0), q + vec2(0.55 * rad, 0.0));
            }
            if icon == Icon::ZoomIn {
                line(q - vec2(0.0, 0.55 * rad), q + vec2(0.0, 0.55 * rad));
            }
        }
        Icon::Grid(cols, rows) => {
            let g = r.shrink2(vec2(0.0, 0.1 * w));
            p.rect_stroke(g, 1.0, s, StrokeKind::Inside);
            for i in 1..cols {
                let x = g.left() + g.width() * i as f32 / cols as f32;
                line(pos2(x, g.top()), pos2(x, g.bottom()));
            }
            for j in 1..rows {
                let y = g.top() + g.height() * j as f32 / rows as f32;
                line(pos2(g.left(), y), pos2(g.right(), y));
            }
        }
        Icon::Link => {
            // Two rings of a chain.
            let rad = vec2(0.32 * w, 0.2 * w);
            for dx in [-0.2, 0.2] {
                p.rect_stroke(Rect::from_center_size(m + vec2(dx * w, 0.0), 2.0 * rad), 0.2 * w, s, StrokeKind::Middle);
            }
        }
        Icon::Compare => {
            // A frame with a swipe line: the left half is filled.
            p.rect_stroke(r.shrink2(vec2(0.0, 0.1 * w)), 1.0, s, StrokeKind::Inside);
            p.rect_filled(Rect::from_min_max(pos2(l, t + 0.1 * w), pos2(m.x, b - 0.1 * w)), 1.0, c);
        }
        Icon::Globe => {
            p.circle_stroke(m, 0.5 * w, s);
            line(pos2(l, m.y), pos2(rt, m.y));
            let n = 12;
            for k in [0.22, -0.22] {
                let pts: Vec<Pos2> = (0..=n).map(|i| i as f32 / n as f32).map(|u| pos2(m.x + k * w * (std::f32::consts::PI * u).sin() * 1.1, t + u * w)).collect();
                p.add(egui::Shape::line(pts, s));
            }
        }
        Icon::Play => fill(vec![pos2(rt - 0.2 * w, m.y), pos2(l + 0.2 * w, b - 0.15 * w), pos2(l + 0.2 * w, t + 0.15 * w)]),
        Icon::Up => fill(vec![pos2(m.x, t + 0.25 * w), pos2(rt - 0.15 * w, b - 0.25 * w), pos2(l + 0.15 * w, b - 0.25 * w)]),
        Icon::Down => fill(vec![pos2(m.x, b - 0.25 * w), pos2(l + 0.15 * w, t + 0.25 * w), pos2(rt - 0.15 * w, t + 0.25 * w)]),
        Icon::Close => {
            let k = 0.22 * w;
            line(pos2(l + k, t + k), pos2(rt - k, b - k));
            line(pos2(l + k, b - k), pos2(rt - k, t + k));
        }
        Icon::StepBack => {
            fill(vec![pos2(l + 0.3 * w, m.y), pos2(rt - 0.1 * w, t + 0.15 * w), pos2(rt - 0.1 * w, b - 0.15 * w)]);
            p.rect_filled(Rect::from_min_max(pos2(l + 0.08 * w, t + 0.15 * w), pos2(l + 0.24 * w, b - 0.15 * w)), 0.0, c);
        }
        Icon::StepForward => {
            fill(vec![pos2(rt - 0.3 * w, m.y), pos2(l + 0.1 * w, b - 0.15 * w), pos2(l + 0.1 * w, t + 0.15 * w)]);
            p.rect_filled(Rect::from_min_max(pos2(rt - 0.24 * w, t + 0.15 * w), pos2(rt - 0.08 * w, b - 0.15 * w)), 0.0, c);
        }
        Icon::Gear => {
            // A wheel with eight teeth.
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let d = vec2(a.cos(), a.sin());
                line(m + d * 0.3 * w, m + d * 0.5 * w);
            }
            p.circle_stroke(m, 0.3 * w, s);
            p.circle_stroke(m, 0.1 * w, s);
        }
        Icon::Palette => {
            // Three color swatches.
            for (k, dy) in [0.0, 0.36, 0.72].into_iter().enumerate() {
                let rr = Rect::from_min_size(pos2(l + 0.12 * k as f32 * w, t + dy * w), vec2(0.62 * w, 0.26 * w));
                if k == 1 {
                    p.rect_filled(rr, 1.0, c);
                } else {
                    p.rect_stroke(rr, 1.0, s, StrokeKind::Inside);
                }
            }
        }
        Icon::Gauge => {
            // A half circle with a needle.
            let q = pos2(m.x, b - 0.2 * w);
            let pts: Vec<Pos2> = (0..=12).map(|i| std::f32::consts::PI * (1.0 + i as f32 / 12.0)).map(|a| q + vec2(a.cos(), a.sin()) * 0.5 * w).collect();
            p.add(egui::Shape::line(pts, s));
            line(q, q + vec2(0.28 * w, -0.3 * w));
            p.circle_filled(q, 0.08 * w, c);
        }
        Icon::Cloud => {
            p.circle_stroke(pos2(l + 0.32 * w, b - 0.32 * w), 0.2 * w, s);
            p.circle_stroke(pos2(l + 0.56 * w, t + 0.42 * w), 0.26 * w, s);
            p.circle_stroke(pos2(rt - 0.18 * w, b - 0.3 * w), 0.18 * w, s);
            line(pos2(l + 0.3 * w, b - 0.12 * w), pos2(rt - 0.18 * w, b - 0.12 * w));
        }
        Icon::Film => {
            let f = r.shrink2(vec2(0.0, 0.12 * w));
            p.rect_stroke(f, 1.0, s, StrokeKind::Inside);
            for x in [f.left() + 0.18 * w, f.right() - 0.18 * w] {
                line(pos2(x, f.top()), pos2(x, f.bottom()));
            }
        }
        Icon::Keyboard => {
            let f = r.shrink2(vec2(0.0, 0.18 * w));
            p.rect_stroke(f, 1.0, s, StrokeKind::Inside);
            for (y, n) in [(0.33, 4), (0.62, 4)] {
                for i in 0..n {
                    p.rect_filled(Rect::from_center_size(pos2(f.left() + (i as f32 + 0.5) * f.width() / n as f32, f.top() + y * f.height()), vec2(0.1 * w, 0.08 * w)), 0.0, c);
                }
            }
            line(pos2(f.left() + 0.28 * w, f.bottom() - 0.14 * w), pos2(f.right() - 0.28 * w, f.bottom() - 0.14 * w));
        }
        Icon::Info => {
            p.circle_stroke(m, 0.5 * w, s);
            line(pos2(m.x, m.y - 0.05 * w), pos2(m.x, b - 0.22 * w));
            p.circle_filled(pos2(m.x, t + 0.26 * w), 0.07 * w, c);
        }
        Icon::PixelGrid => {
            // A grid of 3 x 3 squares, the center one filled.
            for k in 0..=3 {
                let f = k as f32 / 3.0 * w;
                line(pos2(l + f, t), pos2(l + f, b));
                line(pos2(l, t + f), pos2(rt, t + f));
            }
            p.rect_filled(Rect::from_min_size(pos2(l + w / 3.0, t + w / 3.0), vec2(w / 3.0, w / 3.0)), 0.0, c);
        }
        Icon::Graticule => {
            // Curved meridians and straight parallels.
            for y in [0.3, 0.7] {
                line(pos2(l, t + y * w), pos2(rt, t + y * w));
            }
            for k in [-0.3, 0.0, 0.3] {
                let pts: Vec<Pos2> = (0..=8).map(|i| i as f32 / 8.0).map(|u| pos2(m.x + k * w * (1.0 - 0.4 * (2.0 * u - 1.0).powi(2)), t + u * w)).collect();
                p.add(egui::Shape::line(pts, s));
            }
        }
        Icon::Ruler => {
            // A ruler at 45 degrees with its marks.
            let (a, d) = (pos2(l, b - 0.3 * w), vec2(0.7071, -0.7071));
            let n = vec2(0.7071, 0.7071) * 0.3 * w;
            poly(vec![a, a + d * 1.0 * w, a + d * 1.0 * w + n, a + n]);
            for k in 1..5 {
                let q = a + d * (k as f32 * 0.2 * w);
                line(q, q + n * if k % 2 == 0 { 0.6 } else { 0.35 });
            }
        }
        Icon::Transect => {
            // A line between two points, and a profile above it.
            line(pos2(l, b - 0.1 * w), pos2(rt, b - 0.1 * w));
            p.circle_filled(pos2(l + 0.05 * w, b - 0.1 * w), 0.1 * w, c);
            p.circle_filled(pos2(rt - 0.05 * w, b - 0.1 * w), 0.1 * w, c);
            p.add(egui::Shape::line(vec![pos2(l, t + 0.55 * w), pos2(l + 0.3 * w, t + 0.2 * w), pos2(l + 0.55 * w, t + 0.5 * w), pos2(l + 0.75 * w, t + 0.1 * w), pos2(rt, t + 0.4 * w)], s));
        }
        Icon::Region => {
            // A polygon with its corners.
            let pts = vec![pos2(l + 0.1 * w, t + 0.25 * w), pos2(rt - 0.2 * w, t + 0.05 * w), pos2(rt, b - 0.3 * w), pos2(l + 0.35 * w, b)];
            for q in &pts {
                p.circle_filled(*q, 0.09 * w, c);
            }
            poly(pts);
        }
        Icon::Pin => {
            // A map pin: a circle with a point at the bottom.
            let q = pos2(m.x, t + 0.36 * w);
            p.circle_stroke(q, 0.3 * w, s);
            p.circle_filled(q, 0.1 * w, c);
            line(q + vec2(-0.22 * w, 0.22 * w), pos2(m.x, b));
            line(q + vec2(0.22 * w, 0.22 * w), pos2(m.x, b));
        }
        Icon::Code => {
            // The signs < and >.
            line(pos2(l + 0.3 * w, t + 0.2 * w), pos2(l, m.y));
            line(pos2(l, m.y), pos2(l + 0.3 * w, b - 0.2 * w));
            line(pos2(rt - 0.3 * w, t + 0.2 * w), pos2(rt, m.y));
            line(pos2(rt, m.y), pos2(rt - 0.3 * w, b - 0.2 * w));
            line(pos2(m.x + 0.1 * w, t + 0.1 * w), pos2(m.x - 0.1 * w, b - 0.1 * w));
        }
        Icon::Pause => {
            for x in [l + 0.22 * w, rt - 0.42 * w] {
                p.rect_filled(Rect::from_min_max(pos2(x, t + 0.15 * w), pos2(x + 0.2 * w, b - 0.15 * w)), 0.0, c);
            }
        }
    }
}

/// Draw a button in `rect`: the icon, then `text` if it is not empty.
fn paint(ui: &Ui, rect: Rect, resp: &Response, icon: Icon, text: &str, selected: bool, size: f32) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let vis = ui.style().interact_selectable(resp, selected);
    let c = if ui.is_enabled() { vis.fg_stroke.color } else { ui.visuals().widgets.noninteractive.fg_stroke.color.gamma_multiply(0.5) };
    ui.painter().rect(rect.expand(vis.expansion), vis.corner_radius, vis.weak_bg_fill, vis.bg_stroke, StrokeKind::Inside);
    let pad = if text.is_empty() { (rect.width() - size) / 2.0 } else { ui.spacing().button_padding.x };
    let r = Rect::from_center_size(pos2(rect.left() + pad + size / 2.0, rect.center().y), vec2(size, size));
    draw(ui.painter(), icon, r, c);
    if !text.is_empty() {
        let g = ui.painter().layout_no_wrap(text.to_string(), egui::TextStyle::Button.resolve(ui.style()), c);
        ui.painter().galley(pos2(r.right() + 5.0, rect.center().y - g.size().y / 2.0), g, c);
    }
}

/// A button with an icon, and with `text` after the icon if it is not empty.
pub fn button(ui: &mut Ui, icon: Icon, text: &str, selected: bool) -> Response {
    let size = 13.0;
    let pad = ui.spacing().button_padding.x;
    let tw = if text.is_empty() { 0.0 } else { ui.painter().layout_no_wrap(text.to_string(), egui::TextStyle::Button.resolve(ui.style()), Color32::WHITE).size().x + 5.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(size + tw + 2.0 * pad + 2.0, ui.spacing().interact_size.y), Sense::click());
    paint(ui, rect, &resp, icon, text, selected, size);
    resp
}

/// A button with an icon and a text on the full width of the layout: an entry of a list.
pub fn row(ui: &mut Ui, icon: Icon, text: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ui.spacing().interact_size.y + 4.0), Sense::click());
    paint(ui, rect, &resp, icon, text, selected, 14.0);
    resp
}

/// A small button with an icon only, for the rows of lists.
pub fn small(ui: &mut Ui, icon: Icon) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    paint(ui, rect, &resp, icon, "", false, 10.0);
    resp
}

/// A button with an icon in the rectangle `rect`.
pub fn put(ui: &mut Ui, rect: Rect, icon: Icon, selected: bool) -> Response {
    let resp = ui.allocate_rect(rect, Sense::click());
    paint(ui, rect, &resp, icon, "", selected, 12.0);
    resp
}
