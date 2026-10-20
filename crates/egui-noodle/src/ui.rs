use crate::Camera;
use egui::{Align2, Color32, FontId, Id, Painter, Pos2, Rect, Sense, Stroke, Ui, vec2};
use std::ops::RangeInclusive;

/// The lines in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grid {
    None,
    Lines,
    Dots,
}

/// The shape of a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// A smooth curve.
    Curve,
    Straight,
    /// Horizontal and vertical lines.
    Square,
}

/// The shape of a socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Socket {
    Circle,
    Square,
    Diamond,
}

/// How the header of a node shows the color of the node (`Viewer::color`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Header {
    /// The header is in the color.
    Filled,
    /// A thin line of the color at the top of the node.
    Stripe,
    /// The border of the node is in the color.
    Border,
}

/// The look of the editor: colors, shapes and sizes. The sizes are in graph units (screen points at zoom 1).
/// Start from a preset (`Theme::studio`, `light`, `blueprint`, `minimal`) and change what you want.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: Color32,
    pub grid: Color32,
    pub grid_style: Grid,
    /// Size of the grid.
    pub grid_step: f32,
    pub node: Color32,
    /// The border of a node (not selected).
    pub border: Color32,
    pub border_width: f32,
    /// The shadow under a node (transparent: no shadow).
    pub shadow: Color32,
    pub header_style: Header,
    /// The title on a filled header.
    pub title: Color32,
    pub field: Color32,
    pub field_hover: Color32,
    pub button: Color32,
    /// The bar of a slider, a check box that is on.
    pub fill: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub error: Color32,
    pub wire: Color32,
    pub wire_muted: Color32,
    pub wire_style: Wire,
    pub wire_width: f32,
    pub select: Color32,
    pub socket: Color32,
    pub socket_hover: Color32,
    pub socket_shape: Socket,
    pub socket_radius: f32,
    /// Font size of the text.
    pub font: f32,
    /// Height of a row (a field, a socket).
    pub row: f32,
    pub header: f32,
    /// Radius of the corners of the nodes (the fields have 60 % of it).
    pub radius: f32,
    /// Space between the edge of a node and its content.
    pub pad: f32,
    /// Space between two rows.
    pub gap: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Theme::studio()
    }
}

impl Theme {
    /// Dark grey nodes with headers in color, curves.
    pub fn studio() -> Self {
        Theme {
            background: Color32::from_rgb(29, 29, 32),
            grid: Color32::from_rgb(38, 38, 43),
            grid_style: Grid::Lines,
            grid_step: 40.0,
            node: Color32::from_rgb(48, 48, 52),
            border: Color32::from_black_alpha(200),
            border_width: 1.0,
            shadow: Color32::from_black_alpha(60),
            header_style: Header::Filled,
            title: Color32::WHITE,
            field: Color32::from_rgb(34, 34, 37),
            field_hover: Color32::from_rgb(58, 58, 64),
            button: Color32::from_rgb(64, 64, 70),
            fill: Color32::from_rgb(71, 114, 179),
            text: Color32::from_gray(225),
            weak: Color32::from_gray(160),
            error: Color32::from_rgb(255, 120, 120),
            wire: Color32::from_rgb(170, 170, 185),
            wire_muted: Color32::from_gray(90),
            wire_style: Wire::Curve,
            wire_width: 2.5,
            select: Color32::from_rgb(240, 160, 60),
            socket: Color32::from_rgb(160, 160, 170),
            socket_hover: Color32::from_rgb(230, 200, 90),
            socket_shape: Socket::Circle,
            socket_radius: 5.5,
            font: 13.0,
            row: 20.0,
            header: 24.0,
            radius: 6.0,
            pad: 10.0,
            gap: 4.0,
        }
    }

    /// White nodes on a light grey background with dots, a line of color at the top of each node.
    pub fn light() -> Self {
        Theme {
            background: Color32::from_rgb(236, 237, 240),
            grid: Color32::from_rgb(200, 203, 210),
            grid_style: Grid::Dots,
            grid_step: 24.0,
            node: Color32::WHITE,
            border: Color32::from_rgb(214, 216, 222),
            shadow: Color32::from_black_alpha(22),
            header_style: Header::Stripe,
            title: Color32::from_gray(30),
            field: Color32::from_rgb(240, 241, 244),
            field_hover: Color32::from_rgb(226, 229, 236),
            button: Color32::from_rgb(230, 232, 238),
            fill: Color32::from_rgb(66, 133, 244),
            text: Color32::from_gray(35),
            weak: Color32::from_gray(115),
            error: Color32::from_rgb(200, 40, 40),
            wire: Color32::from_rgb(130, 136, 150),
            wire_muted: Color32::from_gray(200),
            select: Color32::from_rgb(255, 140, 0),
            socket: Color32::from_rgb(130, 136, 150),
            socket_hover: Color32::from_rgb(255, 140, 0),
            radius: 8.0,
            ..Theme::studio()
        }
    }

    /// Dark blue paper with a fine grid, square links and sockets, nodes with a border in color.
    pub fn blueprint() -> Self {
        Theme {
            background: Color32::from_rgb(16, 38, 72),
            grid: Color32::from_rgb(30, 60, 105),
            grid_step: 24.0,
            node: Color32::from_rgba_unmultiplied(22, 50, 92, 235),
            border: Color32::from_rgb(120, 170, 230),
            border_width: 1.2,
            shadow: Color32::TRANSPARENT,
            header_style: Header::Border,
            title: Color32::WHITE,
            field: Color32::from_rgb(18, 42, 80),
            field_hover: Color32::from_rgb(34, 72, 124),
            button: Color32::from_rgb(30, 64, 112),
            fill: Color32::from_rgb(90, 170, 255),
            text: Color32::from_rgb(220, 235, 255),
            weak: Color32::from_rgb(150, 180, 220),
            wire: Color32::from_rgb(140, 200, 255),
            wire_muted: Color32::from_rgb(60, 90, 130),
            wire_style: Wire::Square,
            wire_width: 1.6,
            select: Color32::from_rgb(255, 220, 90),
            socket: Color32::from_rgb(140, 200, 255),
            socket_hover: Color32::from_rgb(255, 220, 90),
            socket_shape: Socket::Square,
            socket_radius: 4.5,
            radius: 2.0,
            ..Theme::studio()
        }
    }

    /// Black on white, no grid, straight links.
    pub fn minimal() -> Self {
        Theme {
            background: Color32::from_gray(250),
            grid_style: Grid::None,
            node: Color32::WHITE,
            border: Color32::from_gray(190),
            shadow: Color32::TRANSPARENT,
            header_style: Header::Stripe,
            title: Color32::from_gray(20),
            field: Color32::from_gray(244),
            field_hover: Color32::from_gray(232),
            button: Color32::from_gray(236),
            fill: Color32::from_gray(175),
            text: Color32::from_gray(20),
            weak: Color32::from_gray(120),
            error: Color32::from_rgb(200, 40, 40),
            wire: Color32::from_gray(60),
            wire_muted: Color32::from_gray(200),
            wire_style: Wire::Straight,
            wire_width: 1.4,
            select: Color32::from_rgb(0, 120, 255),
            socket: Color32::from_gray(60),
            socket_hover: Color32::from_rgb(0, 120, 255),
            socket_shape: Socket::Diamond,
            socket_radius: 5.0,
            radius: 3.0,
            ..Theme::studio()
        }
    }

    /// The presets with their names (for a menu).
    pub fn presets() -> Vec<(&'static str, Theme)> {
        vec![("Studio", Theme::studio()), ("Light", Theme::light()), ("Blueprint", Theme::blueprint()), ("Minimal", Theme::minimal())]
    }
}

/// A text that the user edits now (one at a time in the editor).
#[derive(Clone, Debug)]
pub(crate) struct Edit {
    pub key: Id,
    pub buf: String,
    pub focus: bool,
    /// The text at the start of the edit (Escape gives it back).
    pub orig: String,
}

/// The body of a node: the fields that `Viewer::body` adds, one under the other. All the fields are drawn
/// with the painter from graph coordinates: they move and zoom with the node, with no rounding of a layout.
/// A text field shows an egui text edit only while the user types.
pub struct NodeUi<'a> {
    pub(crate) ui: &'a mut Ui,
    pub(crate) painter: &'a Painter,
    pub(crate) cam: Camera,
    pub(crate) area: Rect,
    /// Left side, cursor and width of the content, in graph coordinates.
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) id: Id,
    pub(crate) n: u64,
    pub(crate) edit: &'a mut Option<Edit>,
    pub(crate) popup: &'a mut Option<Id>,
    pub(crate) theme: &'a Theme,
    pub(crate) changed: bool,
}

/// A font size on the screen: by quarters of a point (few sizes in the font atlas of egui).
pub(crate) fn font_size(size: f32) -> f32 {
    ((size * 4.0).round() / 4.0).max(2.0)
}

/// A number with at most 4 decimals, without the zeros at the end.
fn number(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

impl NodeUi<'_> {
    /// The zoom of the editor.
    pub fn zoom(&self) -> f32 {
        self.cam.zoom
    }

    /// The egui `Ui` of the editor (for a popup or a tooltip of the application).
    pub fn ui(&mut self) -> &mut Ui {
        self.ui
    }

    /// True if a field changed its value (the editor gives it in `Response::changed`).
    pub fn mark_changed(&mut self) {
        self.changed = true;
    }

    fn font(&self, k: f32, mono: bool) -> FontId {
        let s = font_size(self.theme.font * k * self.cam.zoom);
        if mono { FontId::monospace(s) } else { FontId::proportional(s) }
    }

    /// The next row of height `h` (graph units): its screen rectangle and its id.
    fn alloc(&mut self, h: f32) -> (Rect, Id) {
        let g = Rect::from_min_size(Pos2::new(self.x, self.y), vec2(self.w, h));
        self.y += h + self.theme.gap;
        self.n += 1;
        (self.cam.rect(self.area, g), self.id.with(self.n))
    }

    fn interact(&mut self, r: Rect, id: Id, sense: Sense) -> egui::Response {
        self.ui.interact(r.intersect(self.area), id, sense)
    }

    fn field(&self, r: Rect, hover: bool) {
        let c = if hover { self.theme.field_hover } else { self.theme.field };
        self.painter.rect_filled(r, self.theme.radius * 0.6 * self.cam.zoom, c);
    }

    fn text(&self, p: Pos2, anchor: Align2, s: &str, color: Color32) {
        self.painter.text(p, anchor, s, self.font(1.0, false), color);
    }

    /// An egui text edit on the field `r` while the user types. Returns the end of the edit: Some(true) on
    /// Enter or a click elsewhere, Some(false) on Escape.
    fn typing(&mut self, r: Rect, multiline: bool, mono: bool) -> Option<bool> {
        let font = self.font(1.0, mono);
        let pad = self.theme.pad * 0.5 * self.cam.zoom;
        let e = self.edit.as_mut()?;
        let tid = e.key.with("text");
        self.painter.rect_filled(r, self.theme.radius * 0.6 * self.cam.zoom, self.theme.field_hover);
        let te = if multiline { egui::TextEdit::multiline(&mut e.buf).code_editor() } else { egui::TextEdit::singleline(&mut e.buf) };
        let resp = self.ui.put(r.shrink2(vec2(pad, 0.0)), te.id(tid).font(font).frame(egui::Frame::NONE).margin(egui::Margin::ZERO).text_color(self.theme.text).desired_width(r.width() - 2.0 * pad));
        if e.focus {
            self.ui.memory_mut(|m| m.request_focus(tid));
            e.focus = false;
            return None;
        }
        if self.ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            return Some(false);
        }
        (resp.lost_focus() || (!multiline && self.ui.input(|i| i.key_pressed(egui::Key::Enter)))).then_some(true)
    }

    /// A number: drag left or right to change it (`speed` for each screen point, Ctrl: 10 times slower),
    /// double-click to type it.
    pub fn value(&mut self, label: &str, v: &mut f64, speed: f64) -> bool {
        let (r, id) = self.alloc(self.theme.row);
        if self.edit.as_ref().is_some_and(|e| e.key == id) {
            if let Some(done) = self.typing(r, false, false) {
                let e = self.edit.take().unwrap();
                if let Some(x) = e.buf.trim().parse::<f64>().ok().filter(|x| x.is_finite() && done) {
                    *v = x;
                    self.changed = true;
                    return true;
                }
            }
            return false;
        }
        let resp = self.interact(r, id, Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let mut changed = false;
        if resp.dragged() {
            let k = if self.ui.input(|i| i.modifiers.ctrl) { 0.1 } else { 1.0 };
            let d = resp.drag_delta().x as f64 * speed * k;
            if d != 0.0 {
                *v += d;
                changed = true;
            }
        }
        if resp.double_clicked() {
            *self.edit = Some(Edit { key: id, buf: number(*v), focus: true, orig: String::new() });
        }
        self.field(r, resp.hovered() || resp.dragged());
        let pad = self.theme.pad * 0.6 * self.cam.zoom;
        self.text(r.left_center() + vec2(pad, 0.0), Align2::LEFT_CENTER, label, self.theme.weak);
        self.text(r.right_center() - vec2(pad, 0.0), Align2::RIGHT_CENTER, &number(*v), self.theme.text);
        self.changed |= changed;
        changed
    }

    /// An integer: as `value`.
    pub fn int(&mut self, label: &str, v: &mut i64, speed: f64) -> bool {
        let mut f = *v as f64;
        let c = self.value(label, &mut f, speed);
        let new = f.round() as i64;
        if new != *v {
            *v = new;
        }
        c
    }

    /// A number in `range`, with a bar: click or drag to set it, double-click to type it.
    pub fn slider(&mut self, label: &str, v: &mut f64, range: RangeInclusive<f64>) -> bool {
        let (r, id) = self.alloc(self.theme.row);
        let (lo, hi) = (*range.start(), *range.end());
        if self.edit.as_ref().is_some_and(|e| e.key == id) {
            if let Some(done) = self.typing(r, false, false) {
                let e = self.edit.take().unwrap();
                if let Some(x) = e.buf.trim().parse::<f64>().ok().filter(|x| x.is_finite() && done) {
                    *v = x.clamp(lo, hi);
                    self.changed = true;
                    return true;
                }
            }
            return false;
        }
        let resp = self.interact(r, id, Sense::click_and_drag());
        let mut changed = false;
        if (resp.dragged() || resp.clicked())
            && let Some(p) = resp.interact_pointer_pos()
        {
            let x = (lo + ((p.x - r.left()) / r.width()).clamp(0.0, 1.0) as f64 * (hi - lo)).clamp(lo, hi);
            if x != *v {
                (*v, changed) = (x, true);
            }
        }
        if resp.double_clicked() {
            *self.edit = Some(Edit { key: id, buf: number(*v), focus: true, orig: String::new() });
        }
        self.field(r, resp.hovered() || resp.dragged());
        let f = if hi > lo { ((*v - lo) / (hi - lo)).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let fr = Rect::from_min_max(r.min, Pos2::new(r.left() + r.width() * f, r.bottom()));
        self.painter.rect_filled(fr, self.theme.radius * 0.6 * self.cam.zoom, self.theme.fill.gamma_multiply(0.8));
        let pad = self.theme.pad * 0.6 * self.cam.zoom;
        self.text(r.left_center() + vec2(pad, 0.0), Align2::LEFT_CENTER, label, self.theme.text);
        self.text(r.right_center() - vec2(pad, 0.0), Align2::RIGHT_CENTER, &number(*v), self.theme.text);
        self.changed |= changed;
        changed
    }

    /// A check box and its label.
    pub fn toggle(&mut self, label: &str, v: &mut bool) -> bool {
        let (r, id) = self.alloc(self.theme.row);
        let resp = self.interact(r, id, Sense::click());
        if resp.clicked() {
            *v ^= true;
            self.changed = true;
        }
        let z = self.cam.zoom;
        let b = Rect::from_center_size(r.left_center() + vec2(8.0 * z, 0.0), vec2(14.0, 14.0) * z);
        self.painter.rect_filled(b, 3.0 * z, if *v { self.theme.fill } else if resp.hovered() { self.theme.field_hover } else { self.theme.field });
        if *v {
            let s = Stroke::new(1.8 * z, Color32::WHITE);
            self.painter.line_segment([b.left_center() + vec2(3.0 * z, 0.0), b.center_bottom() - vec2(1.0 * z, 3.5 * z)], s);
            self.painter.line_segment([b.center_bottom() - vec2(1.0 * z, 3.5 * z), b.right_top() + vec2(-3.0 * z, 3.5 * z)], s);
        }
        self.text(b.right_center() + vec2(6.0 * z, 0.0), Align2::LEFT_CENTER, label, self.theme.text);
        resp.clicked()
    }

    /// One of `options`: a click opens the list (a popup at the size of the interface).
    pub fn choice(&mut self, label: &str, v: &mut usize, options: &[String]) -> bool {
        let (r, id) = self.alloc(self.theme.row);
        let resp = self.interact(r, id, Sense::click());
        self.field(r, resp.hovered() || *self.popup == Some(id));
        let z = self.cam.zoom;
        let pad = self.theme.pad * 0.6 * z;
        let cur = options.get(*v).map_or("", |s| s.as_str());
        let text = if label.is_empty() { cur.to_string() } else { format!("{label}: {cur}") };
        self.painter.with_clip_rect(r.intersect(self.area)).text(r.left_center() + vec2(pad, 0.0), Align2::LEFT_CENTER, text, self.font(1.0, false), self.theme.text);
        // A small triangle at the right.
        let c = r.right_center() - vec2(pad + 4.0 * z, 0.0);
        self.painter.add(egui::Shape::convex_polygon(vec![c + vec2(-4.0, -2.0) * z, c + vec2(4.0, -2.0) * z, c + vec2(0.0, 3.0) * z], self.theme.weak, Stroke::NONE));
        if resp.clicked() {
            *self.popup = if *self.popup == Some(id) { None } else { Some(id) };
        }
        let mut changed = false;
        if *self.popup == Some(id) {
            let a = egui::Area::new(id.with("popup")).fixed_pos(r.left_bottom()).order(egui::Order::Foreground).show(self.ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(r.width().max(120.0));
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        for (k, o) in options.iter().enumerate() {
                            if ui.selectable_label(k == *v, o).clicked() {
                                (*v, changed) = (k, true);
                            }
                        }
                    });
                });
            });
            if changed || (a.response.clicked_elsewhere() && !resp.clicked()) {
                *self.popup = None;
            }
        }
        self.changed |= changed;
        changed
    }

    /// A line of text: a click edits it (live).
    pub fn text_field(&mut self, v: &mut String) -> bool {
        self.text_box(v, 1, false)
    }

    /// Text on `rows` lines in a fixed width font (code): a click edits it (live).
    pub fn code(&mut self, v: &mut String, rows: usize) -> bool {
        self.text_box(v, rows.max(1), true)
    }

    fn text_box(&mut self, v: &mut String, rows: usize, mono: bool) -> bool {
        let line = self.theme.font * 1.35;
        let h = if rows == 1 { self.theme.row } else { rows as f32 * line + 8.0 };
        let (r, id) = self.alloc(h);
        if self.edit.as_ref().is_some_and(|e| e.key == id) {
            let before = self.edit.as_ref().map(|e| e.buf.clone()).unwrap_or_default();
            let end = self.typing(r, rows > 1, mono);
            let now = self.edit.as_ref().map(|e| e.buf.clone()).unwrap_or_default();
            if now != before {
                *v = now;
                self.changed = true;
            }
            if let Some(keep) = end {
                let e = self.edit.take().unwrap();
                if !keep && *v != e.orig {
                    // Escape: the text of the start of the edit.
                    *v = e.orig;
                    self.changed = true;
                }
            }
            return self.changed;
        }
        let resp = self.interact(r, id, Sense::click()).on_hover_cursor(egui::CursorIcon::Text);
        if resp.clicked() {
            *self.edit = Some(Edit { key: id, buf: v.clone(), focus: true, orig: v.clone() });
        }
        self.field(r, resp.hovered());
        let z = self.cam.zoom;
        let p = self.painter.with_clip_rect(r.intersect(self.area));
        let pad = self.theme.pad * 0.6 * z;
        if rows == 1 {
            p.text(r.left_center() + vec2(pad, 0.0), Align2::LEFT_CENTER, v.lines().next().unwrap_or(""), self.font(1.0, mono), self.theme.text);
        } else {
            for (k, l) in v.lines().take(rows).enumerate() {
                p.text(r.left_top() + vec2(pad, (4.0 + k as f32 * line) * z), Align2::LEFT_TOP, l, self.font(1.0, mono), self.theme.text);
            }
        }
        false
    }

    /// A button: true on a click.
    pub fn button(&mut self, label: &str) -> bool {
        let (r, id) = self.alloc(self.theme.row);
        let resp = self.interact(r, id, Sense::click());
        let c = if resp.hovered() { self.theme.field_hover } else { self.theme.button };
        self.painter.rect_filled(r, self.theme.radius * 0.6 * self.cam.zoom, c);
        self.text(r.center(), Align2::CENTER_CENTER, label, self.theme.text);
        resp.clicked()
    }

    /// Text that wraps in the width of the node.
    pub fn label(&mut self, s: &str) {
        self.wrapped(s, self.theme.weak);
    }

    /// A message in red, that wraps in the width of the node.
    pub fn error(&mut self, s: &str) {
        self.wrapped(s, self.theme.error);
    }

    fn wrapped(&mut self, s: &str, color: Color32) {
        let z = self.cam.zoom;
        let g = self.painter.layout(s.to_string(), self.font(0.9, false), color, self.w * z);
        let (r, _) = self.alloc(g.size().y / z);
        self.painter.galley(r.min, g, color);
    }

    /// A line between two groups of fields.
    pub fn separator(&mut self) {
        let (r, _) = self.alloc(2.0);
        self.painter.line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, self.theme.field_hover));
    }
}
