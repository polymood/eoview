//! The Graph tab: a node editor in the style of Blender (DESIGN section 18). Sources are layers of a view,
//! operations are the filters, the layer math and the aggregates of the engine (Rust), a Python node runs a
//! function of the user, and an output node shows its input as a layer of a view.
//!
//! The graph is live: a change in a node makes the layer of each output again (after a short pause, as a
//! slider moves), in place of the old layer. The view keeps the old tiles until the new tiles come. Only a
//! Python node waits for its Run button. The graph is a tree of `OpSave` with node positions: the workspace
//! file keeps it.
//!
//! Keys (the pointer on the graph): Shift+A add a node (with a search field), X or Delete remove the selected
//! nodes, Shift+D duplicate them, M mute them (a muted node gives its first input), H collapse them, A select
//! all, Home show all the nodes. Wheel: zoom. Middle button: pan. Drag from an output to an input: a link.
//! Drag from an input: move or remove its link. Drag on the background: select with a rectangle.

use crate::app::App;
use crate::lang::{t, tf};
use crate::layer::{Kind, LayerSave, OpSave};
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2, vec2};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const NODE_W: f32 = 210.0;
/// The pause after the last change before the layers are made again.
const SETTLE: Duration = Duration::from_millis(150);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node")]
#[allow(clippy::large_enum_variant)] // ponytail: a few nodes, no need to box
pub enum NodeKind {
    /// A layer of a view (one band), with its name for the Python nodes.
    Source { name: String, layer: LayerSave },
    Filter(eo_cache::Filter),
    /// Layer math: the names of the inputs in the expression.
    Math { expr: String, names: Vec<String> },
    /// An aggregate over time (steps `first` to `last`), or along the dimension `dim` if it is not empty.
    Agg { how: String, first: usize, last: usize, #[serde(default)] dim: String },
    /// The code of `def f(a, b, ...)`: it gets one array for each input.
    Python { code: String },
    /// The layer `name` in view `view` (0: the active view at the first change).
    Output { name: String, #[serde(default)] view: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: u32,
    pub pos: [f32; 2],
    pub kind: NodeKind,
    /// The node of each input (None: no link).
    pub inputs: Vec<Option<u32>>,
    #[serde(default)]
    pub collapsed: bool,
    /// A muted node gives its first input.
    #[serde(default)]
    pub muted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
}

/// What an output node makes.
#[derive(Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Made {
    /// A computed layer: its name and its saved layer (with its operation).
    Layer(String, LayerSave),
    /// The script that runs the Python node and makes the layer with `ev.output`.
    Python(String),
}

/// The state of the tab that the workspace file does not keep.
#[derive(Default)]
pub struct GraphUi {
    /// Zoom and pan of the tab (zoom 0: all the nodes at the first frame).
    pub cam: crate::canvas::Canvas,
    sel: HashSet<u32>,
    /// A link that the user drags from the output of this node.
    link: Option<u32>,
    /// The start of a rectangle selection (screen coordinates).
    boxsel: Option<Pos2>,
    /// The add menu: its place in the graph, the search text, and the output to link to the new node.
    add: Option<(Pos2, String, Option<u32>)>,
    /// The time of the last change of a node.
    edit: Option<Instant>,
    /// The operation that each output node asked for, until its layer has it.
    asked: HashMap<u32, Option<OpSave>>,
    /// The message of each output node that cannot make its layer.
    errors: HashMap<u32, String>,
}

impl NodeKind {
    fn n_inputs(&self) -> usize {
        match self {
            NodeKind::Source { .. } => 0,
            NodeKind::Filter(f) => f.inputs(),
            NodeKind::Math { names, .. } => names.len(),
            _ => 1,
        }
    }

    fn title(&self) -> String {
        match self {
            NodeKind::Source { name, .. } => name.clone(),
            NodeKind::Filter(f) => t(f.name()).into(),
            NodeKind::Math { .. } => t("Layer math").into(),
            NodeKind::Agg { .. } => t("Aggregate").into(),
            NodeKind::Python { .. } => "Python".into(),
            NodeKind::Output { name, .. } => name.clone(),
        }
    }

    /// The color of the header, by category (as the node categories of Blender).
    fn color(&self) -> Color32 {
        match self {
            NodeKind::Source { .. } => Color32::from_rgb(131, 50, 50),
            NodeKind::Filter(_) => Color32::from_rgb(52, 84, 140),
            NodeKind::Math { .. } => Color32::from_rgb(40, 110, 100),
            NodeKind::Agg { .. } => Color32::from_rgb(100, 62, 140),
            NodeKind::Python { .. } => Color32::from_rgb(140, 118, 36),
            NodeKind::Output { .. } => Color32::from_rgb(70, 70, 76),
        }
    }
}

/// The name of a source of a saved layer: its file and its band.
fn source_name(s: &LayerSave) -> String {
    let file = std::path::Path::new(&s.path).file_name().map_or(s.path.clone(), |n| n.to_string_lossy().into());
    if s.band.is_empty() { file } else { format!("{} - {file}", s.band) }
}

impl Graph {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn add(&mut self, kind: NodeKind, pos: [f32; 2], inputs: Vec<Option<u32>>) -> u32 {
        let id = self.nodes.iter().map(|n| n.id + 1).max().unwrap_or(1);
        let inputs = if inputs.is_empty() { vec![None; kind.n_inputs()] } else { inputs };
        self.nodes.push(Node { id, pos, kind, inputs, collapsed: false, muted: false });
        id
    }

    /// Remove node `id` and the links to it.
    pub fn remove(&mut self, id: u32) {
        self.nodes.retain(|n| n.id != id);
        self.nodes.iter_mut().flat_map(|n| n.inputs.iter_mut()).filter(|i| **i == Some(id)).for_each(|i| *i = None);
    }

    /// True if node `a` is node `b`, or uses it (a link from `b` to `a` or to a node that `a` uses).
    pub fn depends(&self, a: u32, b: u32) -> bool {
        a == b || self.node(a).is_some_and(|n| n.inputs.iter().flatten().any(|&i| self.depends(i, b)))
    }

    /// Add the tree of the saved layer `s`: a source, or an operation with the nodes of its inputs.
    pub fn add_save(&mut self, s: &LayerSave, name: Option<String>) -> u32 {
        let (kind, ins): (NodeKind, Vec<LayerSave>) = match &s.op {
            None => return self.add(NodeKind::Source { name: name.unwrap_or_else(|| source_name(s)), layer: s.clone() }, [0.0; 2], vec![]),
            Some(OpSave::Filter { filter, inputs }) => (NodeKind::Filter(filter.clone()), inputs.clone()),
            Some(OpSave::Math { expr, names, inputs }) => (NodeKind::Math { expr: expr.clone(), names: names.clone() }, inputs.clone()),
            Some(OpSave::Agg { how, source, first, last }) => (NodeKind::Agg { how: how.clone(), first: *first, last: *last, dim: String::new() }, vec![(**source).clone()]),
            Some(OpSave::AggDim { how, source, dim }) => (NodeKind::Agg { how: how.clone(), first: 0, last: 0, dim: dim.clone() }, vec![(**source).clone()]),
        };
        let ids = ins.iter().map(|i| Some(self.add_save(i, None))).collect();
        self.add(kind, [0.0; 2], ids)
    }

    /// The graph of the saved layer `s` of view `view`: its tree, and an output node.
    pub fn of_save(s: &LayerSave, name: String, view: u32) -> Graph {
        let mut g = Graph::default();
        let root = g.add_save(s, Some(name.clone()));
        g.add(NodeKind::Output { name, view }, [0.0; 2], vec![Some(root)]);
        g.layout();
        g
    }

    /// Put the nodes in columns: the sources at the left, then each operation one column after its inputs.
    pub fn layout(&mut self) {
        fn height(g: &Graph, id: u32) -> usize {
            g.node(id).map_or(0, |n| n.inputs.iter().flatten().map(|&i| 1 + height(g, i)).max().unwrap_or(0))
        }
        // ponytail: the heights of the nodes are estimates (the real heights come with the first frame).
        let est = |k: &NodeKind| match k {
            NodeKind::Source { .. } => 60.0,
            NodeKind::Filter(f) => 90.0 + 26.0 * f.clone().params().len() as f32 + 22.0 * f.inputs() as f32,
            NodeKind::Math { .. } => 130.0,
            NodeKind::Agg { .. } => 150.0,
            NodeKind::Python { .. } => 220.0,
            NodeKind::Output { .. } => 300.0,
        };
        let h: Vec<usize> = self.nodes.iter().map(|n| height(self, n.id)).collect();
        let mut rows: HashMap<usize, f32> = HashMap::new();
        for (n, h) in self.nodes.iter_mut().zip(h) {
            let y = rows.entry(h).or_default();
            n.pos = [20.0 + h as f32 * (NODE_W + 60.0), 20.0 + *y];
            *y += est(&n.kind) + 30.0;
        }
    }

    /// The saved layer of node `id`: a source, or a layer with the operation of the node and the saved
    /// layers of its inputs. A muted node gives its first input.
    pub fn save_of(&self, id: u32) -> Result<LayerSave, String> {
        let n = self.node(id).ok_or("no node")?;
        if n.muted && let Some(&Some(i)) = n.inputs.first() {
            return self.save_of(i);
        }
        let mut ins = vec![];
        for i in &n.inputs {
            ins.push(self.save_of(i.ok_or_else(|| tf("The node {} has an input without link.", &[&n.kind.title()]))?)?);
        }
        let op = match &n.kind {
            NodeKind::Source { layer, .. } => return Ok(layer.clone()),
            NodeKind::Filter(f) => OpSave::Filter { filter: f.clone(), inputs: ins.clone() },
            NodeKind::Math { expr, names } => OpSave::Math { expr: expr.clone(), names: names.clone(), inputs: ins.clone() },
            // ponytail: an aggregate takes a source, as the Layer menu does. An aggregate of a computed layer needs the engine to aggregate layers that are not steps of a file.
            NodeKind::Agg { how, first, last, dim } => match ins.first().filter(|s| s.op.is_none()) {
                Some(s) if dim.is_empty() => OpSave::Agg { how: how.clone(), source: Box::new(s.clone()), first: *first, last: *last },
                Some(s) => OpSave::AggDim { how: how.clone(), source: Box::new(s.clone()), dim: dim.clone() },
                None => return Err(t("An aggregate takes a source.").into()),
            },
            NodeKind::Python { .. } | NodeKind::Output { .. } => return Err(t("Only an output uses the result of a Python node.").into()),
        };
        // The colors of a new layer come from its first input.
        let mut s = ins.into_iter().next().ok_or(t("An operation needs an input."))?;
        (s.path, s.op, s.series, s.step) = (n.kind.title(), Some(op), vec![], 0);
        Ok(s)
    }

    /// What the output node `out` makes.
    pub fn make(&self, out: u32) -> Result<Made, String> {
        let Some(Node { kind: NodeKind::Output { name, .. }, inputs, .. }) = self.node(out) else { return Err(t("Not an output node.").into()) };
        let src = inputs.first().copied().flatten().ok_or(t("Link the output to a node."))?;
        let n = self.node(src).ok_or("no node")?;
        if let NodeKind::Python { code } = &n.kind {
            let mut names = vec![];
            for i in &n.inputs {
                match i.and_then(|i| self.node(i)).map(|s| &s.kind) {
                    Some(NodeKind::Source { name, .. }) => names.push(name.clone()),
                    _ => return Err(t("A Python node takes sources, all linked.").into()),
                }
            }
            return Ok(Made::Python(script(code, &names, name)?));
        }
        let s = self.save_of(src)?;
        if s.op.is_none() {
            return Err(t("Link the output to an operation.").into());
        }
        Ok(Made::Layer(name.clone(), s))
    }
}

/// The script of a Python node: the code of the user, then the call of `f` on the whole layers at level 0
/// (the layers are read by name in the active view), and the result as a new layer.
// ponytail: the whole layer at level 0. `ev.layer` refuses a size above the memory budget. The next step is
// tile by tile with a halo (DESIGN section 18).
fn script(code: &str, names: &[String], out: &str) -> Result<String, String> {
    if !code.lines().any(|l| l.starts_with("def f(")) {
        return Err(t("The code of a Python node defines def f(...).").into());
    }
    Ok(format!(
        "import numpy as np\nimport eoview as ev\n\n{code}\n\n_names = {}\n_args = [np.asarray(ev.layer(n, extent=\"all\", level=0)) for n in _names]\nev.output(f(*_args), name={})\n",
        serde_json::to_string(names).unwrap_or_default(),
        serde_json::to_string(out).unwrap_or_default()
    ))
}

/// The places of the sockets and of the nodes in the graph, for the links and the selection.
#[derive(Default)]
struct Ports {
    ins: HashMap<(u32, usize), Pos2>,
    outs: HashMap<u32, Pos2>,
    rects: Vec<(u32, Rect)>,
}

/// What the user did in the nodes.
#[derive(Default)]
struct Act {
    /// A node was clicked (with Shift: add it to the selection).
    pick: Option<(u32, bool)>,
    /// The selected nodes move by this.
    moved: Option<(u32, Vec2)>,
    /// A drag of a link from the output of this node starts.
    from: Option<u32>,
    /// A drag from input (node, k) starts: its link moves.
    detach: Option<(u32, usize)>,
    /// A value changed: the layers are made again after a pause.
    changed: bool,
    run: Option<u32>,
}

/// A socket at `c`: a circle, and the response of a drag on it.
fn socket(ui: &mut egui::Ui, c: Pos2, id: egui::Id, linked: bool, z: f32) -> egui::Response {
    let r = ui.interact(Rect::from_center_size(c, vec2(16.0, 16.0) * z.max(0.6)), id, Sense::drag());
    let fill = if r.hovered() || r.dragged() { Color32::from_rgb(230, 200, 90) } else if linked { Color32::from_rgb(160, 160, 170) } else { Color32::from_rgb(110, 110, 120) };
    ui.painter().circle(c, 5.5 * z, fill, Stroke::new(1.0, Color32::from_black_alpha(200)));
    r.on_hover_cursor(egui::CursorIcon::Crosshair)
}

/// A link from `a` to `b`: a curve, as in Blender.
fn wire(a: Pos2, b: Pos2, color: Color32, z: f32) -> egui::Shape {
    let d = ((b.x - a.x).abs() * 0.5).max(40.0 * z);
    egui::Shape::CubicBezier(egui::epaint::CubicBezierShape::from_points_stroke([a, a + vec2(d, 0.0), b - vec2(d, 0.0), b], false, Color32::TRANSPARENT, Stroke::new((2.5 * z).max(1.0), color)))
}

/// The output node: the layer of the output in its view, and its display settings.
struct OutLook<'a> {
    views: &'a [(u32, String)],
    layer: Option<&'a mut crate::layer::MapLayer>,
    error: Option<&'a String>,
}

#[allow(clippy::too_many_arguments)]
fn node_ui(ui: &mut egui::Ui, n: &mut Node, sel: bool, ports: &mut Ports, act: &mut Act, mut out: Option<OutLook>, cv: &crate::canvas::Canvas, area: Rect, style: &std::sync::Arc<egui::Style>) {
    let id = n.id;
    // The node at the size of the zoom: its widgets get the scaled style (sharp at any zoom).
    let z = cv.zoom;
    let w = NODE_W * z;
    let top = cv.to_screen(area, Pos2::new(n.pos[0], n.pos[1]));
    let linked: Vec<bool> = n.inputs.iter().map(|i| i.is_some()).collect();
    let frame = egui::Frame::new()
        .fill(Color32::from_rgb(48, 48, 52))
        .stroke(Stroke::new(if sel { 2.0 } else { 1.0 }, if sel { Color32::from_rgb(240, 160, 60) } else { Color32::from_black_alpha(220) }))
        .corner_radius(6.0 * z)
        .inner_margin(egui::Margin::ZERO);
    let mut rows: Vec<(usize, f32)> = vec![];
    let mut out_y = None;
    let r = ui.scope_builder(egui::UiBuilder::new().id_salt(("graph node", id)).max_rect(Rect::from_min_size(top, vec2(w, 4000.0 * z))), |ui| {
        ui.set_style(style.clone());
        frame.show(ui, |ui| {
            ui.set_width(w);
            ui.spacing_mut().item_spacing.y = 0.0;
            // Header: the color of the category, collapse arrow, title, mute mark. Drag: move the node.
            let hr = ui.allocate_exact_size(vec2(w, 24.0 * z), Sense::hover()).0;
            let alpha = if n.muted { 0.35 } else { 1.0 };
            let k = (6.0 * z).round() as u8;
            ui.painter().rect_filled(hr, egui::CornerRadius { nw: k, ne: k, sw: if n.collapsed { k } else { 0 }, se: if n.collapsed { k } else { 0 } }, n.kind.color().gamma_multiply(alpha));
            let head = ui.interact(hr, ui.id().with("head"), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::Grab);
            if head.drag_started() || head.clicked() {
                act.pick = Some((id, ui.input(|i| i.modifiers.shift)));
            }
            if head.dragged() {
                act.moved = Some((id, head.drag_delta() / z));
            }
            ui.new_child(egui::UiBuilder::new().max_rect(hr.shrink2(vec2(6.0, 3.0) * z)).layout(egui::Layout::left_to_right(egui::Align::Center))).scope(|ui| {
                {
                    if ui.add(egui::Button::new(RichText::new(if n.collapsed { "+" } else { "-" }).color(Color32::WHITE).strong()).frame(false)).clicked() {
                        n.collapsed ^= true;
                    }
                    let title = if n.muted { format!("{}  ({})", n.kind.title(), t("muted")) } else { n.kind.title() };
                    ui.add(egui::Label::new(RichText::new(title).strong().color(Color32::WHITE)).truncate().selectable(false));
                }
            });
            if n.collapsed {
                return;
            }
            egui::Frame::new().inner_margin(egui::Margin::symmetric((10.0 * z) as i8, (4.0 * z) as i8)).show(ui, |ui| {
                ui.set_width(w - 20.0 * z);
                ui.spacing_mut().item_spacing.y = 4.0 * z;
                // The output first, as in Blender.
                if !matches!(n.kind, NodeKind::Output { .. }) {
                    let r = ui.allocate_ui_with_layout(vec2(w - 20.0 * z, 18.0 * z), egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(t("Result"))).response;
                    out_y = Some(r.rect.center().y);
                }
                let mut grow = false;
                let ch = &mut act.changed;
                match &mut n.kind {
                    NodeKind::Source { .. } => {}
                    NodeKind::Filter(f) => {
                        for (name, v) in f.params() {
                            let speed = (v.abs() * 0.01).max(0.01);
                            *ch |= ui.add_sized([w - 20.0 * z, 20.0 * z], egui::DragValue::new(v).speed(speed).max_decimals(4).prefix(format!("{}: ", t(name)))).changed();
                        }
                    }
                    NodeKind::Math { expr, .. } => {
                        *ch |= ui.add(egui::TextEdit::singleline(expr).desired_width(f32::INFINITY).code_editor()).changed();
                        grow = ui.small_button(t("+ input")).clicked();
                    }
                    NodeKind::Agg { how, first, last, dim } => {
                        egui::ComboBox::from_id_salt(("graph agg", id)).width(w - 20.0 * z).selected_text(t(how)).show_ui(ui, |ui| {
                            for (_, name) in eo_cache::Agg::ALL {
                                *ch |= ui.selectable_value(how, name.to_string(), t(name)).changed();
                            }
                        });
                        ui.horizontal(|ui| {
                            *ch |= ui.add(egui::DragValue::new(first).prefix(format!("{}: ", t("first")))).changed();
                            *ch |= ui.add(egui::DragValue::new(last).prefix(format!("{}: ", t("last")))).changed();
                        });
                        *ch |= ui.add(egui::TextEdit::singleline(dim).hint_text(t("dimension (empty: time)")).desired_width(f32::INFINITY)).changed();
                    }
                    NodeKind::Python { code } => {
                        ui.add(egui::TextEdit::multiline(code).code_editor().desired_rows(5).desired_width(f32::INFINITY));
                        ui.horizontal(|ui| {
                            grow = ui.small_button(t("+ input")).clicked();
                            if ui.button(t("Run")).on_hover_text(t("Run the function and make the layer of the outputs of this node")).clicked() {
                                act.run = Some(id);
                            }
                        });
                    }
                    NodeKind::Output { name, view } => {
                        *ch |= ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY)).changed();
                        if let Some(o) = out.as_mut() {
                            let cur = o.views.iter().find(|v| v.0 == *view).map_or(t("active view").to_string(), |v| v.1.clone());
                            egui::ComboBox::from_id_salt(("graph view", id)).width(w - 20.0 * z).selected_text(cur).show_ui(ui, |ui| {
                                for (v, title) in o.views {
                                    *ch |= ui.selectable_value(view, *v, title).changed();
                                }
                            });
                            if let Some(e) = o.error {
                                ui.colored_label(Color32::from_rgb(255, 120, 120), e);
                            }
                            if let Some(l) = o.layer.as_deref_mut() {
                                ui.separator();
                                let maps = crate::colormaps::all();
                                egui::ComboBox::from_id_salt(("graph cmap", id)).width(w - 20.0 * z).selected_text(maps.get(l.cmap).map_or("", |c| c.name.as_str())).show_ui(ui, |ui| {
                                    for (i, c) in maps.iter().enumerate() {
                                        if ui.selectable_label(l.cmap == i, &c.name).clicked() {
                                            l.set_cmap(i);
                                        }
                                    }
                                });
                                let st = &mut l.st[0];
                                let speed = ((st.hi - st.lo).abs() as f64 * 0.005).max(1e-6);
                                ui.add_sized([w - 20.0 * z, 20.0 * z], egui::DragValue::new(&mut st.lo).speed(speed).prefix(format!("{}: ", t("min"))));
                                ui.add_sized([w - 20.0 * z, 20.0 * z], egui::DragValue::new(&mut st.hi).speed(speed).prefix(format!("{}: ", t("max"))));
                                ui.horizontal(|ui| {
                                    if ui.small_button(t("Auto")).on_hover_text(t("Automatic stretch (A)")).clicked() {
                                        l.auto();
                                    }
                                    ui.checkbox(&mut l.invert, t("Invert"));
                                    ui.checkbox(&mut l.visible, t("Visible"));
                                });
                                ui.add(egui::Slider::new(&mut l.opacity, 0.0..=1.0).text(t("Opacity")));
                            } else if o.error.is_none() {
                                ui.label(RichText::new(t("computing...")).weak());
                            }
                        }
                    }
                }
                // The inputs.
                let names: Vec<String> = match &n.kind {
                    NodeKind::Math { names, .. } => names.clone(),
                    NodeKind::Python { .. } => (0..n.inputs.len()).map(crate::app::math_name).collect(),
                    _ if n.inputs.len() == 1 => vec![t("Input").into()],
                    _ => (0..n.inputs.len()).map(|k| tf("Input {}", &[&(k + 1).to_string()])).collect(),
                };
                for (k, name) in names.iter().enumerate() {
                    let r = ui.label(name);
                    rows.push((k, r.rect.center().y));
                }
                if grow {
                    if let NodeKind::Math { names, .. } = &mut n.kind {
                        names.push(crate::app::math_name(names.len()));
                    }
                    n.inputs.push(None);
                    *ch = true;
                }
            });
            ui.add_space(4.0);
        });
    });
    let rect = r.response.rect;
    ports.rects.push((id, rect));
    // The sockets on the edges: inputs at the left, the output at the right.
    let mid = rect.top() + 12.0 * z;
    for (k, y) in rows.iter().copied().chain(if n.collapsed { (0..n.inputs.len()).map(|k| (k, mid)).collect::<Vec<_>>() } else { vec![] }) {
        let c = Pos2::new(rect.left(), y);
        ports.ins.insert((id, k), c);
        if socket(ui, c, egui::Id::new(("graph in", id, k)), linked.get(k).copied().unwrap_or(false), z).drag_started() {
            act.detach = Some((id, k));
        }
    }
    if !matches!(n.kind, NodeKind::Output { .. }) {
        let c = Pos2::new(rect.right(), if n.collapsed { mid } else { out_y.unwrap_or(mid) });
        ports.outs.insert(id, c);
        if socket(ui, c, egui::Id::new(("graph out", id)), true, z).drag_started() {
            act.from = Some(id);
        }
    }
}

/// The nodes that the add menu gives: name, category, and the node.
fn catalog(layers: &[(String, Option<LayerSave>)]) -> Vec<(String, String, NodeKind)> {
    let mut v: Vec<(String, String, NodeKind)> = layers.iter().filter_map(|(n, s)| Some((n.clone(), t("Source").to_string(), NodeKind::Source { name: n.clone(), layer: s.clone()? }))).collect();
    for (gr, name, f, _) in crate::layer::FILTERS {
        v.push((t(name).to_string(), t(gr).to_string(), NodeKind::Filter(f.clone())));
    }
    v.push((t("Layer math").into(), t("Math").into(), NodeKind::Math { expr: "a + b".into(), names: vec!["a".into(), "b".into()] }));
    v.push((t("Aggregate").into(), t("Time").into(), NodeKind::Agg { how: eo_cache::Agg::ALL[0].1.into(), first: 0, last: 0, dim: String::new() }));
    v.push(("Python".into(), "Python".into(), NodeKind::Python { code: "def f(a):\n    return a\n".into() }));
    v.push((t("Output").into(), t("Output").into(), NodeKind::Output { name: t("Graph").into(), view: 0 }));
    v
}

impl App {
    /// The Graph tab.
    pub fn graph_tab(&mut self, ui: &mut egui::Ui) {
        let mut g = std::mem::take(&mut self.graph);
        let mut st = std::mem::take(&mut self.graph_ui);
        // The one band layers of the active view: name, and the saved layer if it can be a source.
        let layers: Vec<(String, Option<LayerSave>)> = self.pane(self.active).map_or(vec![], |p| {
            p.layers.iter().rev().map(|l| (l.label(200), (l.kind == Kind::Band && l.inputs.len() == 1 && l.saveable()).then(|| l.save()))).collect()
        });
        let views: Vec<(u32, String)> = self.panes.iter().map(|p| (p.id, p.title())).collect();
        let (mut show, mut want_add) = (false, false);
        ui.horizontal(|ui| {
            want_add = ui.button(t("Add node (Shift+A)")).clicked();
            show = ui.button(t("Show the selected layer")).on_hover_text(t("The operations of the selected layer as a graph")).clicked();
            if ui.button(t("Clear")).clicked() {
                g = Graph::default();
                st = GraphUi::default();
            }
            ui.label(RichText::new(t("Wheel: zoom. Middle button: pan. Drag from a socket: link. X: remove, M: mute, H: collapse, Shift+D: duplicate.")).small().weak());
        });
        let (area, bg) = ui.allocate_exact_size(ui.available_size_before_wrap(), Sense::click_and_drag());
        let mut cv = st.cam;
        let hover = ui.input(|i| i.pointer.hover_pos()).filter(|p| area.contains(*p));
        // The first frame: all the nodes (near 210 x 150 each at zoom 1).
        if cv.zoom == 0.0 {
            cv.zoom = 1.0;
            if let Some(r) = g.nodes.iter().map(|n| Rect::from_min_size(Pos2::new(n.pos[0], n.pos[1]), vec2(NODE_W, 150.0))).reduce(|a, b| a.union(b)) {
                cv.fit(area, r);
            }
        }
        // Blender: the wheel zooms at the pointer, the middle button pans.
        if let Some(p) = hover {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy != 0.0 {
                cv.zoom_at(area, p, (dy / 400.0).exp());
                ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
            }
        }
        if bg.dragged_by(egui::PointerButton::Middle) {
            cv.off += bg.drag_delta();
        }
        if want_add {
            st.add = Some((cv.to_graph(area, area.center()), String::new(), None));
        }
        let keys = hover.is_some() && !ui.ctx().egui_wants_keyboard_input();
        let (mut ports, mut act) = (Ports::default(), Act::default());
        // The background and its grid, as in Blender.
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, Color32::from_rgb(29, 29, 32));
        let step = 40.0 * cv.zoom;
        if step > 8.0 {
            let (o, line) = (area.min + vec2(cv.off.x.rem_euclid(step), cv.off.y.rem_euclid(step)), Stroke::new(1.0, Color32::from_rgb(38, 38, 43)));
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
        let wires = painter.add(egui::Shape::Noop);
        let style = std::sync::Arc::new(crate::canvas::scaled(ui.style(), cv.zoom));
        let mut nodes = std::mem::take(&mut g.nodes);
        let clip = ui.clip_rect();
        ui.set_clip_rect(area.intersect(clip));
        for n in &mut nodes {
            let out = match &n.kind {
                NodeKind::Output { name, view } => {
                    let view = if *view == 0 { self.active } else { *view };
                    let layer = self.panes.iter_mut().find(|p| p.id == view).and_then(|p| p.layers.iter_mut().find(|l| l.path == *name && l.op.is_some()));
                    Some(OutLook { views: &views, layer, error: st.errors.get(&n.id) })
                }
                _ => None,
            };
            node_ui(ui, n, st.sel.contains(&n.id), &mut ports, &mut act, out, &cv, area, &style);
        }
        ui.set_clip_rect(clip);
        // The links, below the nodes, then the link that the user drags and the selection rectangle.
        let z = cv.zoom;
        let mut lines = vec![];
        for n in &nodes {
            for (k, i) in n.inputs.iter().enumerate() {
                if let (Some(a), Some(b)) = (i.and_then(|i| ports.outs.get(&i)), ports.ins.get(&(n.id, k))) {
                    let muted = nodes.iter().any(|m| Some(m.id) == *i && m.muted);
                    lines.push(wire(*a, *b, if muted { Color32::from_gray(90) } else { Color32::from_rgb(170, 170, 185) }, z));
                }
            }
        }
        let pointer = ui.ctx().pointer_latest_pos();
        if let (Some(from), Some(p)) = (st.link.and_then(|f| ports.outs.get(&f)), pointer) {
            lines.push(wire(*from, p, Color32::from_rgb(230, 200, 90), z));
        }
        if let (Some(a), Some(p)) = (st.boxsel, pointer) {
            let r = Rect::from_two_pos(a, p);
            lines.push(egui::Shape::rect_filled(r, 2.0, Color32::from_rgba_unmultiplied(240, 160, 60, 25)));
            lines.push(egui::Shape::rect_stroke(r, 2.0, Stroke::new(1.0, Color32::from_rgb(240, 160, 60)), egui::StrokeKind::Inside));
        }
        painter.set(wires, egui::Shape::Vec(lines));
        g.nodes = nodes;
        // Selection: a click on a node, Shift adds it; a click on the background clears; a drag on the
        // background selects with a rectangle.
        if let Some((id, add)) = act.pick {
            if !add && !st.sel.contains(&id) {
                st.sel.clear();
            }
            st.sel.insert(id);
        }
        if let Some((id, d)) = act.moved {
            let ids: Vec<u32> = if st.sel.contains(&id) { st.sel.iter().copied().collect() } else { vec![id] };
            for n in g.nodes.iter_mut().filter(|n| ids.contains(&n.id)) {
                (n.pos[0], n.pos[1]) = (n.pos[0] + d.x, n.pos[1] + d.y);
            }
        }
        if bg.clicked() {
            st.sel.clear();
        }
        if bg.drag_started_by(egui::PointerButton::Primary) {
            st.boxsel = bg.interact_pointer_pos();
        }
        if bg.drag_stopped()
            && let (Some(a), Some(p)) = (st.boxsel.take(), bg.interact_pointer_pos())
        {
            let r = Rect::from_two_pos(a, p);
            if !ui.input(|i| i.modifiers.shift) {
                st.sel.clear();
            }
            st.sel.extend(ports.rects.iter().filter(|(_, n)| n.intersects(r)).map(|(id, _)| *id));
        }
        if bg.secondary_clicked() {
            st.add = bg.interact_pointer_pos().map(|p| (cv.to_graph(area, p), String::new(), None));
        }
        // Links: from an output, or the link of an input that the user takes. The end on an input links,
        // on the background opens the add menu (the new node gets the link).
        if let Some(f) = act.from {
            st.link = Some(f);
        }
        if let Some((id, k)) = act.detach
            && let Some(n) = g.nodes.iter_mut().find(|n| n.id == id)
        {
            st.link = n.inputs[k].take();
            act.changed = true;
        }
        if st.link.is_some() && ui.input(|i| i.pointer.any_released()) {
            let from = st.link.take().unwrap_or_default();
            let hit = pointer.and_then(|p| ports.ins.iter().filter(|(_, c)| c.distance(p) < 16.0 * z.max(0.6)).min_by(|a, b| a.1.distance(p).total_cmp(&b.1.distance(p))).map(|(k, _)| *k));
            match hit {
                // A link that makes a cycle is refused.
                Some((id, k)) if !g.depends(from, id) => {
                    if let Some(n) = g.nodes.iter_mut().find(|n| n.id == id) {
                        n.inputs[k] = Some(from);
                        act.changed = true;
                    }
                }
                Some(_) => {}
                None if pointer.is_some_and(|p| area.contains(p)) => st.add = pointer.map(|p| (cv.to_graph(area, p), String::new(), Some(from))),
                None => {}
            }
        }
        // Keys of Blender.
        if keys {
            let (shift, a, x, del, d, m, h, home) = ui.input(|i| (i.modifiers.shift, i.key_pressed(egui::Key::A), i.key_pressed(egui::Key::X), i.key_pressed(egui::Key::Delete), i.key_pressed(egui::Key::D), i.key_pressed(egui::Key::M), i.key_pressed(egui::Key::H), i.key_pressed(egui::Key::Home)));
            if shift && a {
                st.add = hover.map(|p| (cv.to_graph(area, p), String::new(), None));
            } else if a {
                st.sel = g.nodes.iter().map(|n| n.id).collect();
            }
            if x || del {
                for id in std::mem::take(&mut st.sel) {
                    g.remove(id);
                }
                act.changed = true;
            }
            if shift && d {
                let copies: Vec<Node> = g.nodes.iter().filter(|n| st.sel.contains(&n.id)).cloned().collect();
                st.sel.clear();
                for c in copies {
                    let id = g.add(c.kind, [c.pos[0] + 30.0, c.pos[1] + 30.0], c.inputs);
                    st.sel.insert(id);
                }
            }
            if m || h {
                for n in g.nodes.iter_mut().filter(|n| st.sel.contains(&n.id)) {
                    n.muted ^= m;
                    n.collapsed ^= h;
                }
                act.changed |= m;
            }
            if home && let Some(r) = ports.rects.iter().map(|r| Rect::from_min_max(cv.to_graph(area, r.1.min), cv.to_graph(area, r.1.max))).reduce(|a, b| a.union(b)) {
                cv.fit(area, r);
            }
        }
        st.cam = cv;
        self.add_menu(ui, &mut g, &mut st, &layers, area);
        if act.changed {
            st.edit = Some(Instant::now());
        }
        let run = act.run;
        self.graph = g;
        self.graph_ui = st;
        if show {
            self.graph_show(self.active);
        }
        if let Some(p) = run {
            let outs: Vec<u32> = self.graph.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Output { .. }) && n.inputs.first() == Some(&Some(p))).map(|n| n.id).collect();
            for o in outs {
                self.graph_make(o);
            }
        }
        self.graph_live(ui.ctx());
    }

    /// The add menu (Shift+A, a right click, or the end of a link on the background): a search field and
    /// the nodes by category.
    fn add_menu(&mut self, ui: &mut egui::Ui, g: &mut Graph, st: &mut GraphUi, layers: &[(String, Option<LayerSave>)], area: Rect) {
        let cam = st.cam;
        let Some((at, q, link)) = st.add.as_mut() else { return };
        let screen = cam.to_screen(area, *at);
        let mut pick: Option<NodeKind> = None;
        let mut close = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let area = egui::Area::new(egui::Id::new("graph add")).fixed_pos(screen).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(240.0);
                let r = ui.add(egui::TextEdit::singleline(q).hint_text(t("Search")));
                r.request_focus();
                let ql = q.to_lowercase();
                let items: Vec<_> = catalog(layers).into_iter().filter(|(n, c, _)| ql.is_empty() || n.to_lowercase().contains(&ql) || c.to_lowercase().contains(&ql)).collect();
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    pick = items.first().map(|x| x.2.clone());
                }
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    let mut last = String::new();
                    for (name, cat, k) in items {
                        if cat != last {
                            ui.label(RichText::new(&cat).small().strong().color(k.color().gamma_multiply(1.6)));
                            last = cat.clone();
                        }
                        if ui.selectable_label(false, format!("   {name}")).clicked() {
                            pick = Some(k);
                        }
                    }
                });
            });
        });
        if area.response.clicked_elsewhere() && !ui.input(|i| i.pointer.any_released() && st.link.is_some()) {
            close = true;
        }
        if let Some(k) = pick {
            let (pos, from) = ([at.x, at.y], *link);
            let n = k.n_inputs();
            let id = g.add(k, pos, vec![]);
            if let Some(f) = from.filter(|_| n > 0)
                && let Some(node) = g.nodes.iter_mut().find(|x| x.id == id)
            {
                node.inputs[0] = Some(f);
            }
            st.sel = HashSet::from([id]);
            st.edit = Some(Instant::now());
            close = true;
        }
        if close {
            st.add = None;
        }
    }

    /// Make the layer of each output node again when its operation changed, after a pause in the changes.
    /// The new layer takes the place of the old one (same colors, the view keeps the old tiles until the new
    /// ones come). A Python node waits for its Run button.
    pub(crate) fn graph_live(&mut self, ctx: &egui::Context) {
        if let Some(e) = self.graph_ui.edit.filter(|e| e.elapsed() < SETTLE) {
            ctx.request_repaint_after(SETTLE.saturating_sub(e.elapsed()));
            return;
        }
        self.graph_ui.edit = None;
        let outs: Vec<(u32, String, u32)> = self.graph.nodes.iter().filter_map(|n| match &n.kind {
            NodeKind::Output { name, view } => Some((n.id, name.clone(), *view)),
            _ => None,
        }).collect();
        for (id, name, view) in outs {
            let made = self.graph.make(id);
            let s = match made {
                Ok(Made::Layer(_, s)) => s,
                Ok(Made::Python(_)) => continue,
                Err(e) => {
                    self.graph_ui.errors.insert(id, e);
                    continue;
                }
            };
            self.graph_ui.errors.remove(&id);
            // The view of the output: the active view the first time.
            let view = if self.pane(view).is_some() { view } else { self.active };
            if let Some(NodeKind::Output { view: v, .. }) = self.graph.nodes.iter_mut().find(|n| n.id == id).map(|n| &mut n.kind) {
                *v = view;
            }
            let Some(p) = self.pane(view) else { continue };
            let old = p.layers.iter().find(|l| l.path == name && l.op.is_some()).map(|l| (l.uid, l.order, l.save(), l.op == s.op));
            if old.as_ref().is_some_and(|o| o.3) {
                self.graph_ui.asked.remove(&id);
                continue;
            }
            if self.graph_ui.asked.get(&id) == Some(&s.op) {
                continue;
            }
            self.graph_ui.asked.insert(id, s.op.clone());
            match old {
                // The same colors and stretch as the old layer.
                Some((uid, order, mut keep, _)) => {
                    (keep.path, keep.op) = (name, s.op);
                    self.load_layer_as(view, order, keep, None, Some(uid));
                }
                None => self.load_layer_as(view, usize::MAX, LayerSave { path: name, ..s }, None, None),
            }
        }
    }

    /// The graph of the selected layer of view `id` (a source, or the tree of its operation).
    pub fn graph_show(&mut self, id: u32) {
        let Some((s, name)) = self.pane(id).and_then(|p| p.layers.get(p.sel)).map(|l| (l.save(), l.path.clone())) else { return };
        self.graph = Graph::of_save(&s, name, id);
        self.graph_ui = GraphUi::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn source(band: &str) -> LayerSave {
        let st = json!({ "lo": 0.0, "hi": 1.0, "gamma": 1.0, "db": false });
        serde_json::from_value(json!({
            "path": "a.tif", "kind": "Band", "band": band, "rgb": ["", "", ""], "expr": "", "st": [st, st, st],
            "clip": 0.0, "cmap": "", "stops": [], "invert": false, "opacity": 1.0, "visible": true
        }))
        .unwrap()
    }

    /// A graph gives the saved layer with its operation, and the saved layer gives the graph again. A muted
    /// node gives its input.
    #[test]
    fn graph_to_op_and_back() {
        let mut g = Graph::default();
        let a = g.add(NodeKind::Source { name: "a".into(), layer: source("1") }, [0.0; 2], vec![]);
        let f = g.add(NodeKind::Filter(eo_cache::Filter::Gaussian { sigma: 2.0 }), [0.0; 2], vec![]);
        let o = g.add(NodeKind::Output { name: "out".into(), view: 0 }, [0.0; 2], vec![Some(f)]);
        assert!(g.make(o).is_err(), "an input without link");
        g.nodes[1].inputs[0] = Some(a);
        let Ok(Made::Layer(name, s)) = g.make(o) else { panic!() };
        assert_eq!(name, "out");
        assert_eq!(s.op, Some(OpSave::Filter { filter: eo_cache::Filter::Gaussian { sigma: 2.0 }, inputs: vec![source("1")] }));
        let back = Graph::of_save(&s, name, 0);
        assert_eq!(back.nodes.len(), 3);
        assert_eq!(back.make(3).unwrap(), Made::Layer("out".into(), s));
        // A link that makes a cycle is refused by `depends`. The workspace file keeps the graph.
        assert!(g.depends(f, a) && !g.depends(a, f));
        assert_eq!(serde_json::from_str::<Graph>(&serde_json::to_string(&g).unwrap()).unwrap(), g);
        g.nodes[1].muted = true;
        assert!(g.make(o).is_err(), "a muted operation gives the source: not an operation");
    }

    /// The script of a Python node reads its sources by name, and refuses code without `def f(`.
    #[test]
    fn python_node_script() {
        let mut g = Graph::default();
        let a = g.add(NodeKind::Source { name: "a - \"x\".tif".into(), layer: source("1") }, [0.0; 2], vec![]);
        let p = g.add(NodeKind::Python { code: "def f(a):\n    return a * 2\n".into() }, [0.0; 2], vec![Some(a)]);
        let o = g.add(NodeKind::Output { name: "double".into(), view: 0 }, [0.0; 2], vec![Some(p)]);
        let Ok(Made::Python(s)) = g.make(o) else { panic!() };
        assert!(s.contains("def f(a):") && s.contains(r#"_names = ["a - \"x\".tif"]"#) && s.contains(r#"name="double""#), "{s}");
        g.nodes[1].kind = NodeKind::Python { code: "x = 1".into() };
        assert!(g.make(o).is_err());
    }
}
