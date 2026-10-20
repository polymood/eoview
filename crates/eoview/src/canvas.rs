//! A canvas with zoom and pan for the node graph. The widgets are drawn at the size of the zoom (a scaled
//! style, `scaled`), not drawn at zoom 1 and then scaled as a picture: the text and the edges stay sharp at
//! any zoom.

use egui::{CornerRadius, Margin, Pos2, Rect, Style, Vec2};

/// The camera of the canvas: the graph point p is at the screen point `rect.min + off + p * zoom`. Zoom 0:
/// not placed yet (the first frame shows all the nodes).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Canvas {
    pub off: Vec2,
    pub zoom: f32,
}

impl Canvas {
    pub fn to_screen(&self, r: Rect, p: Pos2) -> Pos2 {
        r.min + self.off + p.to_vec2() * self.zoom
    }

    pub fn to_graph(&self, r: Rect, s: Pos2) -> Pos2 {
        ((s - r.min - self.off) / self.zoom).to_pos2()
    }

    /// Zoom by `k`: the graph point under the screen point `s` stays under it.
    pub fn zoom_at(&mut self, r: Rect, s: Pos2, k: f32) {
        let g = self.to_graph(r, s);
        self.zoom = (self.zoom * k).clamp(0.2, 3.0);
        self.off = s - r.min - g.to_vec2() * self.zoom;
    }

    /// Show the graph rectangle `g` in the screen rectangle `r`, with a margin.
    pub fn fit(&mut self, r: Rect, g: Rect) {
        let g = g.expand(30.0);
        self.zoom = (r.width() / g.width()).min(r.height() / g.height()).clamp(0.2, 1.5);
        self.off = (r.size() - g.size() * self.zoom) / 2.0 - g.min.to_vec2() * self.zoom;
    }
}

/// The style `base` at zoom `z`: the font sizes (by half points: few sizes in the font atlas), the spacing,
/// the widget sizes and the corners. egui draws the widgets at this size.
pub fn scaled(base: &Style, z: f32) -> Style {
    let mut s = base.clone();
    for f in s.text_styles.values_mut() {
        f.size = ((f.size * z * 2.0).round() / 2.0).max(3.0);
    }
    let m = |m: Margin| {
        let k = |v: i8| (v as f32 * z).round().clamp(-128.0, 127.0) as i8;
        Margin { left: k(m.left), right: k(m.right), top: k(m.top), bottom: k(m.bottom) }
    };
    let sp = &mut s.spacing;
    (sp.item_spacing, sp.button_padding, sp.interact_size) = (sp.item_spacing * z, sp.button_padding * z, sp.interact_size * z);
    (sp.window_margin, sp.menu_margin) = (m(sp.window_margin), m(sp.menu_margin));
    for v in [&mut sp.indent, &mut sp.slider_width, &mut sp.slider_rail_height, &mut sp.combo_width, &mut sp.text_edit_width, &mut sp.icon_width, &mut sp.icon_width_inner, &mut sp.icon_spacing, &mut sp.combo_height] {
        *v *= z;
    }
    let c = |c: CornerRadius| {
        let k = |v: u8| (v as f32 * z).round().clamp(0.0, 255.0) as u8;
        CornerRadius { nw: k(c.nw), ne: k(c.ne), sw: k(c.sw), se: k(c.se) }
    };
    let w = &mut s.visuals.widgets;
    for v in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        v.corner_radius = c(v.corner_radius);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zoom keeps the point under the pointer, and the fit shows the whole rectangle.
    #[test]
    fn zoom_and_fit() {
        let r = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0));
        let mut c = Canvas { off: Vec2::new(10.0, 20.0), zoom: 1.0 };
        let s = Pos2::new(400.0, 300.0);
        let g = c.to_graph(r, s);
        c.zoom_at(r, s, 1.7);
        assert!((c.to_screen(r, g) - s).length() < 1e-3 && (c.zoom - 1.7).abs() < 1e-6);
        let g = Rect::from_min_size(Pos2::new(-50.0, 0.0), Vec2::new(1000.0, 400.0));
        c.fit(r, g);
        assert!(r.contains(c.to_screen(r, g.min)) && r.contains(c.to_screen(r, g.max)));
        let st = scaled(&Style::default(), 2.0);
        assert_eq!(st.spacing.item_spacing, Style::default().spacing.item_spacing * 2.0);
    }
}
