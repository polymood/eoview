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
    Left,
    Right,
    Up,
    Down,
    Close,
    Play,
    Pause,
    /// One time step back or forward: a triangle and a bar.
    StepBack,
    StepForward,
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
        Icon::Left => fill(vec![pos2(l + 0.2 * w, m.y), pos2(rt - 0.2 * w, t + 0.15 * w), pos2(rt - 0.2 * w, b - 0.15 * w)]),
        Icon::Right | Icon::Play => fill(vec![pos2(rt - 0.2 * w, m.y), pos2(l + 0.2 * w, b - 0.15 * w), pos2(l + 0.2 * w, t + 0.15 * w)]),
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
