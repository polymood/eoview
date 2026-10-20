use crate::ui::{Edit, Grid, Header, NodeUi, Socket, Theme, Wire, font_size};
use crate::{Camera, Graph};
use egui::{Align2, Color32, CornerRadius, FontId, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, vec2};
use std::collections::{HashMap, HashSet};

/// What the application gives for its nodes. `N` is the data of a node.
pub trait Viewer<N> {
    /// The title in the header of the node.
    fn title(&mut self, node: &N) -> String;
    /// The color of the node (its category): the header, a line or the border, see `Theme::header_style`.
    fn color(&mut self, _node: &N) -> Color32 {
        Color32::from_rgb(70, 70, 76)
    }
    /// The labels of the inputs: one input for each label. The number can change (the editor adds or
    /// removes inputs at the end).
    fn inputs(&mut self, node: &N) -> Vec<String>;
    /// The label of the output, or None for a node without output (a result, a view).
    fn output(&mut self, _node: &N) -> Option<String> {
        Some("Result".into())
    }
    /// The fields of the node, between its output and its inputs.
    fn body(&mut self, _id: u32, _node: &mut N, _ui: &mut NodeUi) {}
    /// The nodes of the add menu (Shift+A): category, name and the new node.
    fn catalog(&mut self) -> Vec<(String, String, N)>;
    /// The width of the node (graph units).
    fn width(&mut self, _node: &N) -> f32 {
        200.0
    }
}

/// The texts of the editor (for a translation).
#[derive(Clone, Debug)]
pub struct Labels {
    pub search: String,
    pub muted: String,
}

impl Default for Labels {
    fn default() -> Self {
        Labels { search: "Search".into(), muted: "muted".into() }
    }
}

/// What the user did in the editor in this frame.
pub struct Response {
    /// A field, a link or the nodes changed: a live application computes its outputs again.
    pub changed: bool,
    /// The response of the background of the editor.
    pub response: egui::Response,
}

/// The node editor: its camera, its selection and the state of the drags. Keep it between the frames.
///
/// Mouse: wheel to zoom at the pointer, middle button to pan, drag a header to move the selected nodes, drag
/// from an output socket to an input socket to link, drag from an input to move or remove its link, drop a
/// link on the background to add a node with this link, drag on the background to select with a rectangle,
/// right click to add a node.
/// Keys (the pointer on the editor): Shift+A add a node, X or Delete remove the selected nodes, Shift+D
/// duplicate them, M mute them, H collapse them, A select all, Home show all the nodes.
#[derive(Default)]
pub struct Editor {
    pub camera: Camera,
    pub theme: Theme,
    pub labels: Labels,
    pub selected: HashSet<u32>,
    link: Option<u32>,
    boxsel: Option<Pos2>,
    add: Option<(Pos2, String, Option<u32>)>,
    edit: Option<Edit>,
    popup: Option<Id>,
    heights: HashMap<u32, f32>,
}

/// A link from `a` to `b` in the style of the theme.
fn wire(a: Pos2, b: Pos2, color: Color32, z: f32, t: &Theme) -> Shape {
    let stroke = Stroke::new((t.wire_width * z).max(1.0), color);
    match t.wire_style {
        Wire::Curve => {
            let d = ((b.x - a.x).abs() * 0.5).max(40.0 * z);
            Shape::CubicBezier(egui::epaint::CubicBezierShape::from_points_stroke([a, a + vec2(d, 0.0), b - vec2(d, 0.0), b], false, Color32::TRANSPARENT, stroke))
        }
        Wire::Straight => Shape::line_segment([a, b], stroke),
        Wire::Square => {
            let x = if b.x > a.x + 20.0 * z { (a.x + b.x) / 2.0 } else { a.x + 20.0 * z };
            Shape::line(vec![a, Pos2::new(x, a.y), Pos2::new(x, b.y), b], stroke)
        }
    }
}

/// A socket at `c` in the shape of the theme.
fn socket_shape(c: Pos2, r: f32, fill: Color32, t: &Theme) -> Shape {
    let stroke = Stroke::new(1.0, Color32::from_black_alpha(160));
    match t.socket_shape {
        Socket::Circle => Shape::circle_filled(c, r, fill),
        Socket::Square => Shape::rect_filled(Rect::from_center_size(c, vec2(1.7, 1.7) * r), r * 0.2, fill),
        Socket::Diamond => Shape::convex_polygon(vec![c + vec2(0.0, -1.25 * r), c + vec2(1.25 * r, 0.0), c + vec2(0.0, 1.25 * r), c + vec2(-1.25 * r, 0.0)], fill, stroke),
    }
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put the nodes of `g` in columns (see `Graph::layout`) with the heights that the editor measured at the
    /// last frame (120 for a node that it did not show yet). `width` is the width of the columns.
    pub fn arrange<N>(&self, g: &mut Graph<N>, width: f32) {
        g.layout(width, |n| self.heights.get(&n.id).copied().unwrap_or(120.0));
    }

    /// Show all the nodes at the next frame.
    pub fn show_all(&mut self) {
        self.camera.zoom = 0.0;
    }

    /// An editor with the theme `theme` (a preset such as `Theme::light()`, or your own).
    pub fn with_theme(theme: Theme) -> Self {
        Editor { theme, ..Self::default() }
    }

    /// Show the graph `g` in all the space that is left in `ui`.
    pub fn show<N: Clone>(&mut self, ui: &mut Ui, g: &mut Graph<N>, v: &mut impl Viewer<N>) -> Response {
        let (area, bg) = ui.allocate_exact_size(ui.available_size_before_wrap(), Sense::click_and_drag());
        let t = self.theme.clone();
        let mut cam = self.camera;
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| area.contains(*p));
        if cam.zoom == 0.0 {
            cam.zoom = 1.0;
            let r = g.nodes.iter().map(|n| Rect::from_min_size(Pos2::new(n.pos[0], n.pos[1]), vec2(v.width(&n.data), self.heights.get(&n.id).copied().unwrap_or(120.0)))).reduce(|a, b| a.union(b));
            if let Some(r) = r {
                cam.fit(area, r);
            }
        }
        // The wheel zooms at the pointer, the middle button pans.
        if let Some(p) = hover {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                cam.zoom_at(area, p, (dy / 400.0).exp());
                ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            }
        }
        if bg.dragged_by(egui::PointerButton::Middle) {
            cam.off += bg.drag_delta();
        }
        let z = cam.zoom;
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, t.background);
        let step = t.grid_step * z;
        let o = area.min + vec2(cam.off.x.rem_euclid(step), cam.off.y.rem_euclid(step));
        match t.grid_style {
            Grid::Lines if step > 8.0 => {
                let line = Stroke::new(1.0, t.grid);
                let mut x = o.x;
                while x < area.right() {
                    painter.line_segment([Pos2::new(x, area.top()), Pos2::new(x, area.bottom())], line);
                    x += step;
                }
                let mut y = o.y;
                while y < area.bottom() {
                    painter.line_segment([Pos2::new(area.left(), y), Pos2::new(area.right(), y)], line);
                    y += step;
                }
            }
            Grid::Dots if step > 12.0 => {
                let r = (1.4 * z).clamp(0.8, 2.0);
                let mut y = o.y;
                while y < area.bottom() {
                    let mut x = o.x;
                    while x < area.right() {
                        painter.circle_filled(Pos2::new(x, y), r, t.grid);
                        x += step;
                    }
                    y += step;
                }
            }
            _ => {}
        }
        let wires = painter.add(Shape::Noop);
        let font = |k: f32| FontId::proportional(font_size(t.font * k * z));
        let mut changed = false;
        let (mut ins, mut outs): (HashMap<(u32, usize), Pos2>, HashMap<u32, Pos2>) = (HashMap::new(), HashMap::new());
        let mut rects: Vec<(u32, Rect)> = vec![];
        // A click on a node (Shift: add it to the selection), a move of the selected nodes, the start of a
        // link from an output, the start of a drag from an input.
        let mut pick: Option<(u32, bool)> = None;
        let mut moved: Option<(u32, Vec2)> = None;
        let (mut from, mut detach): (Option<u32>, Option<(u32, usize)>) = (None, None);
        let shift = ui.input(|i| i.modifiers.shift);
        for n in g.nodes.iter_mut() {
            let (id, w) = (n.id, v.width(&n.data));
            let top = Pos2::new(n.pos[0], n.pos[1]);
            let old_h = self.heights.get(&id).copied().unwrap_or(t.header);
            // The node area (a click selects it), then the frame, below the fields.
            let full = cam.rect(area, Rect::from_min_size(top, vec2(w, old_h)));
            if ui.interact(full.intersect(area), Id::new(("noodle node", id)), Sense::click()).clicked() {
                pick = Some((id, shift));
            }
            let frame = painter.add(Shape::Noop);
            // Header: drag to move, the sign at the left to collapse.
            let hr = cam.rect(area, Rect::from_min_size(top, vec2(w, t.header)));
            let head = ui.interact(hr.intersect(area), Id::new(("noodle head", id)), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::Grab);
            if head.drag_started() || head.clicked() {
                pick = Some((id, shift));
            }
            if head.dragged() {
                moved = Some((id, head.drag_delta() / z));
            }
            let cr = Rect::from_center_size(hr.left_center() + vec2(12.0 * z, 0.0), vec2(16.0, 16.0) * z);
            if ui.interact(cr.intersect(area), Id::new(("noodle collapse", id)), Sense::click()).clicked() {
                n.collapsed ^= true;
            }
            let c = cr.center();
            let tri = if n.collapsed { vec![c + vec2(-3.0, -4.0) * z, c + vec2(4.0, 0.0) * z, c + vec2(-3.0, 4.0) * z] } else { vec![c + vec2(-4.0, -3.0) * z, c + vec2(4.0, -3.0) * z, c + vec2(0.0, 4.0) * z] };
            let title = if n.muted { format!("{}  ({})", v.title(&n.data), self.labels.muted) } else { v.title(&n.data) };
            let mut y = top.y + t.header;
            let mut body = vec![];
            if !n.collapsed {
                y += t.gap;
                // The output first, then the fields, then the inputs.
                if let Some(o) = v.output(&n.data) {
                    let r = cam.rect(area, Rect::from_min_size(Pos2::new(top.x + t.pad, y), vec2(w - 2.0 * t.pad, t.row)));
                    body.push((r.right_center(), o, Align2::RIGHT_CENTER));
                    outs.insert(id, Pos2::new(cam.to_screen(area, Pos2::new(top.x + w, 0.0)).x, r.center().y));
                    y += t.row + t.gap;
                }
                let mut nu = NodeUi { ui, painter: &painter, cam, area, x: top.x + t.pad, y, w: w - 2.0 * t.pad, id: Id::new(("noodle body", id)), n: 0, edit: &mut self.edit, popup: &mut self.popup, theme: &t, changed: false };
                v.body(id, &mut n.data, &mut nu);
                (changed, y) = (changed | nu.changed, nu.y);
            }
            let labels = v.inputs(&n.data);
            if n.inputs.len() != labels.len() {
                n.inputs.resize(labels.len(), None);
                changed = true;
            }
            for (k, l) in labels.into_iter().enumerate() {
                if n.collapsed {
                    ins.insert((id, k), hr.left_center());
                    continue;
                }
                let r = cam.rect(area, Rect::from_min_size(Pos2::new(top.x + t.pad, y), vec2(w - 2.0 * t.pad, t.row)));
                body.push((r.left_center(), l, Align2::LEFT_CENTER));
                ins.insert((id, k), Pos2::new(cam.to_screen(area, Pos2::new(top.x, 0.0)).x, r.center().y));
                y += t.row + t.gap;
            }
            if n.collapsed {
                outs.entry(id).and_modify(|p| *p = hr.right_center());
                if v.output(&n.data).is_some() {
                    outs.insert(id, hr.right_center());
                }
            }
            let h = if n.collapsed { t.header } else { y - top.y + t.pad * 0.5 };
            self.heights.insert(id, h);
            let nr = cam.rect(area, Rect::from_min_size(top, vec2(w, h)));
            rects.push((id, nr));
            // The frame: body, header with the color of the category, border (the selection).
            let k = (t.radius * z).round().clamp(0.0, 255.0) as u8;
            let sel = self.selected.contains(&id);
            let hc = if n.muted { v.color(&n.data).gamma_multiply(0.35) } else { v.color(&n.data) };
            let mut shapes = vec![];
            if t.shadow.a() > 0 {
                shapes.push(Shape::rect_filled(nr.translate(vec2(0.0, 3.0 * z)), k, t.shadow));
            }
            shapes.push(Shape::rect_filled(nr, k, t.node));
            let top_corners = CornerRadius { nw: k, ne: k, sw: if n.collapsed { k } else { 0 }, se: if n.collapsed { k } else { 0 } };
            let title_color = match t.header_style {
                Header::Filled => {
                    shapes.push(Shape::rect_filled(hr, top_corners, hc));
                    t.title
                }
                Header::Stripe => {
                    shapes.push(Shape::rect_filled(Rect::from_min_size(hr.min, vec2(hr.width(), (3.0 * z).max(2.0))), CornerRadius { nw: k, ne: k, sw: 0, se: 0 }, hc));
                    t.text
                }
                Header::Border => t.text,
            };
            shapes.push(Shape::convex_polygon(tri, title_color, Stroke::NONE));
            let border = match (sel, t.header_style) {
                (true, _) => Stroke::new(2.0, t.select),
                (false, Header::Border) => Stroke::new(t.border_width.max(1.5), hc),
                _ => Stroke::new(t.border_width, t.border),
            };
            shapes.push(Shape::rect_stroke(nr, k, border, egui::StrokeKind::Outside));
            painter.set(frame, Shape::Vec(shapes));
            painter.with_clip_rect(hr.shrink2(vec2(4.0 * z, 0.0)).intersect(area)).text(hr.left_center() + vec2(24.0 * z, 0.0), Align2::LEFT_CENTER, title, font(1.0), title_color);
            for (p, l, a) in body {
                painter.text(p, a, l, font(1.0), t.text);
            }
        }
        // The sockets, on the nodes.
        let socket = |ui: &mut Ui, c: Pos2, id: Id, on: bool| {
            let r = ui.interact(Rect::from_center_size(c, vec2(16.0, 16.0) * z.max(0.6)).intersect(area), id, Sense::drag()).on_hover_cursor(egui::CursorIcon::Crosshair);
            let fill = if r.hovered() || r.dragged() { t.socket_hover } else if on { t.socket } else { t.socket.gamma_multiply(0.6) };
            painter.add(socket_shape(c, t.socket_radius * z, fill, &t));
            r
        };
        for (&(id, k), &c) in &ins {
            let on = g.node(id).is_some_and(|n| n.inputs.get(k).copied().flatten().is_some());
            if socket(ui, c, Id::new(("noodle in", id, k)), on).drag_started() {
                detach = Some((id, k));
            }
        }
        for (&id, &c) in &outs {
            if socket(ui, c, Id::new(("noodle out", id)), true).drag_started() {
                from = Some(id);
            }
        }
        // The links, the link that the user drags, the selection rectangle.
        let mut lines = vec![];
        for n in &g.nodes {
            for (k, i) in n.inputs.iter().enumerate() {
                if let (Some(a), Some(b)) = (i.and_then(|i| outs.get(&i)), ins.get(&(n.id, k))) {
                    let muted = i.and_then(|i| g.node(i)).is_some_and(|m| m.muted);
                    lines.push(wire(*a, *b, if muted { t.wire_muted } else { t.wire }, z, &t));
                }
            }
        }
        let pointer = ui.ctx().pointer_latest_pos();
        if let (Some(a), Some(p)) = (self.link.and_then(|f| outs.get(&f)), pointer) {
            lines.push(wire(*a, p, t.socket_hover, z, &t));
        }
        if let (Some(a), Some(p)) = (self.boxsel, pointer) {
            let r = Rect::from_two_pos(a, p);
            lines.push(Shape::rect_filled(r, 2.0, t.select.gamma_multiply(0.1)));
            lines.push(Shape::rect_stroke(r, 2.0, Stroke::new(1.0, t.select), egui::StrokeKind::Inside));
        }
        painter.set(wires, Shape::Vec(lines));
        // Selection and moves.
        if let Some((id, add)) = pick {
            if !add && !self.selected.contains(&id) {
                self.selected.clear();
            }
            self.selected.insert(id);
        }
        if let Some((id, d)) = moved {
            let ids: Vec<u32> = if self.selected.contains(&id) { self.selected.iter().copied().collect() } else { vec![id] };
            for n in g.nodes.iter_mut().filter(|n| ids.contains(&n.id)) {
                (n.pos[0], n.pos[1]) = (n.pos[0] + d.x, n.pos[1] + d.y);
            }
        }
        if bg.clicked() {
            self.selected.clear();
            self.popup = None;
        }
        if bg.drag_started_by(egui::PointerButton::Primary) {
            self.boxsel = bg.interact_pointer_pos();
        }
        if bg.drag_stopped()
            && let (Some(a), Some(p)) = (self.boxsel.take(), bg.interact_pointer_pos())
        {
            let r = Rect::from_two_pos(a, p);
            if !shift {
                self.selected.clear();
            }
            self.selected.extend(rects.iter().filter(|(_, n)| n.intersects(r)).map(|(id, _)| *id));
        }
        if bg.secondary_clicked() {
            self.add = bg.interact_pointer_pos().map(|p| (cam.to_graph(area, p), String::new(), None));
        }
        // Links: from an output, or the link of an input that the user takes. The end on an input links, on
        // the background opens the add menu (the new node gets the link).
        if let Some(f) = from {
            self.link = Some(f);
        }
        if let Some((id, k)) = detach
            && let Some(n) = g.node_mut(id)
        {
            self.link = n.inputs[k].take();
            changed = true;
        }
        if self.link.is_some() && ui.input(|i| i.pointer.any_released()) {
            let f = self.link.take().unwrap_or_default();
            let hit = pointer.and_then(|p| ins.iter().filter(|(_, c)| c.distance(p) < 16.0 * z.max(0.6)).min_by(|a, b| a.1.distance(p).total_cmp(&b.1.distance(p))).map(|(k, _)| *k));
            match hit {
                Some((id, k)) => changed |= g.link(f, id, k),
                None if pointer.is_some_and(|p| area.contains(p)) => self.add = pointer.map(|p| (cam.to_graph(area, p), String::new(), Some(f))),
                None => {}
            }
        }
        // Keys, when no text field has the keyboard.
        if hover.is_some() && self.edit.is_none() && !ui.ctx().egui_wants_keyboard_input() {
            let (a, x, del, d, m, h, home) = ui.input(|i| (i.key_pressed(egui::Key::A), i.key_pressed(egui::Key::X), i.key_pressed(egui::Key::Delete), i.key_pressed(egui::Key::D), i.key_pressed(egui::Key::M), i.key_pressed(egui::Key::H), i.key_pressed(egui::Key::Home)));
            if shift && a {
                self.add = hover.map(|p| (cam.to_graph(area, p), String::new(), None));
            } else if a {
                self.selected = g.nodes.iter().map(|n| n.id).collect();
            }
            if (x || del) && !self.selected.is_empty() {
                for id in std::mem::take(&mut self.selected) {
                    g.remove(id);
                }
                changed = true;
            }
            if shift && d {
                let copies: Vec<_> = g.nodes.iter().filter(|n| self.selected.contains(&n.id)).cloned().collect();
                self.selected.clear();
                for c in copies {
                    let id = g.add(c.data, [c.pos[0] + 30.0, c.pos[1] + 30.0], 0, c.inputs);
                    self.selected.insert(id);
                }
                changed = true;
            }
            if (m || h) && !self.selected.is_empty() {
                for n in g.nodes.iter_mut().filter(|n| self.selected.contains(&n.id)) {
                    n.muted ^= m;
                    n.collapsed ^= h;
                }
                changed |= m;
            }
            if home && let Some(r) = rects.iter().map(|r| Rect::from_min_max(cam.to_graph(area, r.1.min), cam.to_graph(area, r.1.max))).reduce(|a, b| a.union(b)) {
                cam.fit(area, r);
            }
        }
        self.camera = cam;
        changed |= self.add_menu(ui, g, v, area);
        Response { changed, response: bg }
    }

    /// The add menu: a search field and the nodes of the catalog by category.
    fn add_menu<N>(&mut self, ui: &mut Ui, g: &mut Graph<N>, v: &mut impl Viewer<N>, area: Rect) -> bool {
        let cam = self.camera;
        let Some((at, q, link)) = self.add.as_mut() else { return false };
        let mut pick = None;
        let mut close = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let search = self.labels.search.clone();
        let a = egui::Area::new(Id::new("noodle add")).fixed_pos(cam.to_screen(area, *at)).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(240.0);
                let r = ui.add(egui::TextEdit::singleline(q).hint_text(search));
                r.request_focus();
                let ql = q.to_lowercase();
                let items: Vec<(String, String, N)> = v.catalog().into_iter().filter(|(c, n, _)| ql.is_empty() || n.to_lowercase().contains(&ql) || c.to_lowercase().contains(&ql)).collect();
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let mut first = None;
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    let mut last = String::new();
                    for (k, (cat, name, _)) in items.iter().enumerate() {
                        first.get_or_insert(k);
                        if *cat != last {
                            ui.label(egui::RichText::new(cat).small().strong());
                            last = cat.clone();
                        }
                        if ui.selectable_label(false, format!("   {name}")).clicked() {
                            pick = Some(k);
                        }
                    }
                });
                if enter {
                    pick = pick.or(first);
                }
                pick.and_then(|k| items.into_iter().nth(k).map(|x| x.2))
            })
            .inner
        });
        if a.response.clicked_elsewhere() {
            close = true;
        }
        let mut changed = false;
        if let Some(data) = a.inner {
            let n = v.inputs(&data).len();
            let id = g.add(data, [at.x, at.y], n, vec![]);
            if let Some(f) = *link {
                g.link(f, id, 0);
            }
            self.selected = HashSet::from([id]);
            (changed, close) = (true, true);
        }
        if close {
            self.add = None;
        }
        changed
    }
}
