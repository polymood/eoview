use egui::{Pos2, Rect, Vec2};

/// The camera of the canvas: the graph point `p` is at the screen point `area.min + off + p * zoom`.
/// Zoom 0: not placed yet (the editor shows all the nodes at the first frame).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Camera {
    pub off: Vec2,
    pub zoom: f32,
}

impl Camera {
    pub fn to_screen(self, area: Rect, p: Pos2) -> Pos2 {
        area.min + self.off + p.to_vec2() * self.zoom
    }

    pub fn to_graph(self, area: Rect, s: Pos2) -> Pos2 {
        ((s - area.min - self.off) / self.zoom).to_pos2()
    }

    /// The screen rectangle of the graph rectangle `r`.
    pub fn rect(self, area: Rect, r: Rect) -> Rect {
        Rect::from_min_max(self.to_screen(area, r.min), self.to_screen(area, r.max))
    }

    /// Zoom by `k`: the graph point under the screen point `s` stays under it.
    pub fn zoom_at(&mut self, area: Rect, s: Pos2, k: f32) {
        let g = self.to_graph(area, s);
        self.zoom = (self.zoom * k).clamp(0.1, 4.0);
        self.off = s - area.min - g.to_vec2() * self.zoom;
    }

    /// Show the graph rectangle `g` in the screen rectangle `area`, with a margin.
    pub fn fit(&mut self, area: Rect, g: Rect) {
        let g = g.expand(30.0);
        self.zoom = (area.width() / g.width()).min(area.height() / g.height()).clamp(0.1, 1.5);
        self.off = (area.size() - g.size() * self.zoom) / 2.0 - g.min.to_vec2() * self.zoom;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zoom keeps the point under the pointer, and the fit shows the whole rectangle.
    #[test]
    fn zoom_and_fit() {
        let area = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0));
        let mut c = Camera { off: Vec2::new(10.0, 20.0), zoom: 1.0 };
        let s = Pos2::new(400.0, 300.0);
        let g = c.to_graph(area, s);
        c.zoom_at(area, s, 1.7);
        assert!((c.to_screen(area, g) - s).length() < 1e-3 && (c.zoom - 1.7).abs() < 1e-6);
        let g = Rect::from_min_size(Pos2::new(-50.0, 0.0), Vec2::new(1000.0, 400.0));
        c.fit(area, g);
        assert!(area.contains(c.to_screen(area, g.min)) && area.contains(c.to_screen(area, g.max)));
    }
}
