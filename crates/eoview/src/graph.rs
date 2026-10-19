//! The Graph tab: a node graph of layers (DESIGN section 18). Sources are layers of the view, operations
//! are the filters, the layer math and the aggregates of the engine, and a Python node runs a function of
//! the user. "Make layer" on an output node builds the computed layer with the engine calls that the Layer
//! menu uses. The graph is a tree of `OpSave` with node positions: the workspace file keeps it.

use crate::app::App;
use crate::lang::{t, tf};
use crate::layer::{Kind, LayerSave, OpSave};
use egui::{Pos2, RichText, Sense, Vec2, vec2};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const NODE_W: f32 = 190.0;

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
    Output { name: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: u32,
    pub pos: [f32; 2],
    pub kind: NodeKind,
    /// The node of each input (None: no link).
    pub inputs: Vec<Option<u32>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
}

/// What "Make layer" does.
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
    pub pan: Vec2,
    /// The node whose output waits for a click on an input.
    pub pending: Option<u32>,
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
            NodeKind::Output { .. } => t("Output").into(),
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
        self.nodes.push(Node { id, pos, kind, inputs });
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

    /// The graph of the saved layer `s`: its tree, and an output node.
    pub fn of_save(s: &LayerSave, name: String) -> Graph {
        let mut g = Graph::default();
        let root = g.add_save(s, Some(name.clone()));
        g.add(NodeKind::Output { name }, [0.0; 2], vec![Some(root)]);
        g.layout();
        g
    }

    /// Put the nodes in columns: the sources at the left, then each operation one column after its inputs.
    pub fn layout(&mut self) {
        fn height(g: &Graph, id: u32) -> usize {
            g.node(id).map_or(0, |n| n.inputs.iter().flatten().map(|&i| 1 + height(g, i)).max().unwrap_or(0))
        }
        let h: Vec<usize> = self.nodes.iter().map(|n| height(self, n.id)).collect();
        let mut rows: HashMap<usize, f32> = HashMap::new();
        for (n, h) in self.nodes.iter_mut().zip(h) {
            let r = rows.entry(h).or_default();
            n.pos = [20.0 + h as f32 * (NODE_W + 50.0), 20.0 + *r * 150.0];
            *r += 1.0;
        }
    }

    /// The saved layer of node `id`: a source, or a layer with the operation of the node and the saved
    /// layers of its inputs.
    pub fn save_of(&self, id: u32) -> Result<LayerSave, String> {
        let n = self.node(id).ok_or("no node")?;
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

    /// What "Make layer" does for the output node `out`.
    pub fn make(&self, out: u32) -> Result<Made, String> {
        let Some(Node { kind: NodeKind::Output { name }, inputs, .. }) = self.node(out) else { return Err(t("Not an output node.").into()) };
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

/// The positions of the ports on the screen, for the links.
#[derive(Default)]
struct Ports {
    ins: HashMap<(u32, usize), Pos2>,
    outs: HashMap<u32, Pos2>,
}

/// What the user did in the nodes.
#[derive(Default)]
struct Act {
    del: Option<u32>,
    make: Option<u32>,
    out: Option<u32>,
    inp: Option<(u32, usize)>,
}

fn port(ui: &mut egui::Ui, on: bool) -> (egui::Response, Pos2) {
    let r = ui.allocate_response(vec2(14.0, 14.0), Sense::click());
    let c = r.rect.center();
    let fill = if on || r.hovered() { ui.visuals().selection.bg_fill } else { ui.visuals().widgets.inactive.bg_fill };
    ui.painter().circle(c, 5.0, fill, egui::Stroke::new(1.0, ui.visuals().text_color()));
    (r, c)
}

fn node_ui(ui: &mut egui::Ui, n: &mut Node, origin: Pos2, pending: Option<u32>, ports: &mut Ports, act: &mut Act) {
    let Node { id, pos, kind, inputs } = n;
    let id = *id;
    let title = kind.title();
    let rect = egui::Rect::from_min_size(origin + vec2(pos[0], pos[1]), vec2(NODE_W + 24.0, 2000.0));
    ui.scope_builder(egui::UiBuilder::new().id_salt(("graph node", id)).max_rect(rect), |ui| {
        egui::Frame::window(ui.style()).show(ui, |ui| {
            ui.set_width(NODE_W);
            ui.horizontal(|ui| {
                let h = ui.add_sized([NODE_W - 24.0, 18.0], egui::Label::new(RichText::new(title).strong()).truncate().sense(Sense::drag())).on_hover_cursor(egui::CursorIcon::Grab);
                if h.dragged() {
                    (pos[0], pos[1]) = (pos[0] + h.drag_delta().x, pos[1] + h.drag_delta().y);
                }
                if ui.small_button("x").on_hover_text(t("Remove the node")).clicked() {
                    act.del = Some(id);
                }
            });
            for k in 0..inputs.len() {
                ui.horizontal(|ui| {
                    let (r, c) = port(ui, false);
                    ports.ins.insert((id, k), c);
                    let label = match kind {
                        NodeKind::Math { names, .. } => names[k].clone(),
                        _ => tf("input {}", &[&(k + 1).to_string()]),
                    };
                    ui.label(label);
                    if r.on_hover_text(t("Click here after the output of a node to link, or alone to remove the link")).clicked() {
                        act.inp = Some((id, k));
                    }
                });
            }
            let mut grow = false;
            match kind {
                NodeKind::Source { .. } => {}
                NodeKind::Filter(f) => {
                    egui::Grid::new(("graph params", id)).num_columns(2).show(ui, |ui| {
                        for (n, v) in f.params() {
                            ui.label(t(n));
                            ui.add(egui::DragValue::new(v).speed(0.1).max_decimals(4));
                            ui.end_row();
                        }
                    });
                }
                NodeKind::Math { expr, .. } => {
                    ui.add(egui::TextEdit::singleline(expr).desired_width(f32::INFINITY));
                    grow = ui.small_button(t("+ input")).clicked();
                }
                NodeKind::Agg { how, first, last, dim } => {
                    egui::ComboBox::from_id_salt(("graph agg", id)).selected_text(t(how)).show_ui(ui, |ui| {
                        for (_, name) in eo_cache::Agg::ALL {
                            ui.selectable_value(how, name.to_string(), t(name));
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(t("Steps"));
                        ui.add(egui::DragValue::new(first));
                        ui.add(egui::DragValue::new(last));
                    });
                    ui.add(egui::TextEdit::singleline(dim).hint_text(t("dimension (empty: time)")).desired_width(f32::INFINITY));
                }
                NodeKind::Python { code } => {
                    ui.add(egui::TextEdit::multiline(code).code_editor().desired_rows(5).desired_width(f32::INFINITY));
                    grow = ui.small_button(t("+ input")).clicked();
                }
                NodeKind::Output { name } => {
                    ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
                    if ui.button(t("Make layer")).on_hover_text(t("Make a computed layer in the active view")).clicked() {
                        act.make = Some(id);
                    }
                }
            }
            if grow {
                if let NodeKind::Math { names, .. } = kind {
                    names.push(crate::app::math_name(names.len()));
                }
                inputs.push(None);
            }
            if !matches!(kind, NodeKind::Output { .. }) {
                ui.allocate_ui_with_layout(vec2(NODE_W, 16.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (r, c) = port(ui, pending == Some(id));
                    ports.outs.insert(id, c);
                    if r.on_hover_text(t("Click here, then on an input of an other node")).clicked() {
                        act.out = Some(id);
                    }
                    ui.label(t("output"));
                });
            }
        });
    });
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
        let mut add: Option<(NodeKind, Vec<Option<u32>>)> = None;
        let mut show = false;
        ui.horizontal(|ui| {
            ui.menu_button(t("Add node"), |ui| {
                ui.menu_button(t("Source"), |ui| {
                    for (name, s) in &layers {
                        if ui.add_enabled(s.is_some(), egui::Button::new(name)).clicked()
                            && let Some(s) = s
                        {
                            add = Some((NodeKind::Source { name: name.clone(), layer: s.clone() }, vec![]));
                            ui.close();
                        }
                    }
                });
                let mut groups: Vec<&str> = crate::layer::FILTERS.iter().map(|f| f.0).collect();
                groups.dedup();
                for gr in groups {
                    ui.menu_button(t(gr), |ui| {
                        for (_, name, f, _) in crate::layer::FILTERS.iter().filter(|f| f.0 == gr) {
                            if ui.button(t(name)).clicked() {
                                add = Some((NodeKind::Filter(f.clone()), vec![]));
                                ui.close();
                            }
                        }
                    });
                }
                let mut item = |ui: &mut egui::Ui, name: &str, k: NodeKind| {
                    if ui.button(name).clicked() {
                        add = Some((k, vec![]));
                        ui.close();
                    }
                };
                item(ui, t("Layer math"), NodeKind::Math { expr: "a + b".into(), names: vec!["a".into(), "b".into()] });
                item(ui, t("Aggregate"), NodeKind::Agg { how: eo_cache::Agg::ALL[0].1.into(), first: 0, last: 0, dim: String::new() });
                item(ui, "Python", NodeKind::Python { code: "def f(a):\n    return a\n".into() });
                item(ui, t("Output"), NodeKind::Output { name: t("Graph").into() });
            });
            show = ui.button(t("Show the selected layer")).on_hover_text(t("The operations of the selected layer as a graph")).clicked();
            if ui.button(t("Clear")).clicked() {
                g = Graph::default();
            }
            if let Some(k) = st.pending {
                ui.label(tf("Click an input to link the output of {}.", &[&g.node(k).map_or(String::new(), |n| n.kind.title())]));
            }
        });
        if let Some((k, ins)) = add {
            let o = st.pan * -1.0 + vec2(20.0, 20.0) + Vec2::splat(24.0 * (g.nodes.len() % 8) as f32);
            g.add(k, [o.x, o.y], ins);
        }
        let canvas = ui.available_rect_before_wrap();
        let bg = ui.allocate_rect(canvas, Sense::click_and_drag());
        st.pan += bg.drag_delta();
        if bg.clicked() {
            st.pending = None;
        }
        let wires = ui.painter().add(egui::Shape::Noop);
        let (mut ports, mut act) = (Ports::default(), Act::default());
        let origin = canvas.min + st.pan;
        let mut nodes = std::mem::take(&mut g.nodes);
        for n in &mut nodes {
            node_ui(ui, n, origin, st.pending, &mut ports, &mut act);
        }
        g.nodes = nodes;
        // The links, below the nodes.
        let mut lines = vec![];
        for n in &g.nodes {
            for (k, i) in n.inputs.iter().enumerate() {
                if let (Some(a), Some(b)) = (i.and_then(|i| ports.outs.get(&i)), ports.ins.get(&(n.id, k))) {
                    let d = (b.x - a.x).abs().max(40.0) * 0.5;
                    let stroke = egui::Stroke::new(2.0, ui.visuals().selection.bg_fill);
                    lines.push(egui::Shape::CubicBezier(egui::epaint::CubicBezierShape::from_points_stroke([*a, *a + vec2(d, 0.0), *b - vec2(d, 0.0), *b], false, egui::Color32::TRANSPARENT, stroke)));
                }
            }
        }
        ui.painter().set(wires, egui::Shape::Vec(lines));
        if let Some(id) = act.del {
            g.remove(id);
            st.pending = st.pending.filter(|&p| p != id);
        }
        if let Some(id) = act.out {
            st.pending = if st.pending == Some(id) { None } else { Some(id) };
        }
        if let Some((id, k)) = act.inp {
            let new = match st.pending {
                // A link that makes a cycle is refused.
                Some(p) if g.depends(p, id) => g.node(id).and_then(|n| n.inputs[k]),
                p => p,
            };
            st.pending = None;
            if let Some(n) = g.nodes.iter_mut().find(|n| n.id == id) {
                n.inputs[k] = new;
            }
        }
        self.graph = g;
        self.graph_ui = st;
        if show {
            self.graph_show(self.active);
        }
        if let Some(o) = act.make {
            self.graph_make(o);
        }
    }

    /// The graph of the selected layer of view `id` (a source, or the tree of its operation).
    pub fn graph_show(&mut self, id: u32) {
        let Some((s, name)) = self.pane(id).and_then(|p| p.layers.get(p.sel)).map(|l| (l.save(), l.label(200))) else { return };
        self.graph = Graph::of_save(&s, name);
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

    /// A graph gives the saved layer with its operation, and the saved layer gives the graph again.
    #[test]
    fn graph_to_op_and_back() {
        let mut g = Graph::default();
        let a = g.add(NodeKind::Source { name: "a".into(), layer: source("1") }, [0.0; 2], vec![]);
        let f = g.add(NodeKind::Filter(eo_cache::Filter::Gaussian { sigma: 2.0 }), [0.0; 2], vec![]);
        let o = g.add(NodeKind::Output { name: "out".into() }, [0.0; 2], vec![Some(f)]);
        assert!(g.make(o).is_err(), "an input without link");
        g.nodes[1].inputs[0] = Some(a);
        let Ok(Made::Layer(name, s)) = g.make(o) else { panic!() };
        assert_eq!(name, "out");
        assert_eq!(s.op, Some(OpSave::Filter { filter: eo_cache::Filter::Gaussian { sigma: 2.0 }, inputs: vec![source("1")] }));
        let back = Graph::of_save(&s, name);
        assert_eq!(back.nodes.len(), 3);
        assert_eq!(back.make(3).unwrap(), Made::Layer("out".into(), s));
        // A link that makes a cycle is refused by `depends`. The workspace file keeps the graph.
        assert!(g.depends(f, a) && !g.depends(a, f));
        assert_eq!(serde_json::from_str::<Graph>(&serde_json::to_string(&g).unwrap()).unwrap(), g);
    }

    /// The script of a Python node reads its sources by name, and refuses code without `def f(`.
    #[test]
    fn python_node_script() {
        let mut g = Graph::default();
        let a = g.add(NodeKind::Source { name: "a - \"x\".tif".into(), layer: source("1") }, [0.0; 2], vec![]);
        let p = g.add(NodeKind::Python { code: "def f(a):\n    return a * 2\n".into() }, [0.0; 2], vec![Some(a)]);
        let o = g.add(NodeKind::Output { name: "double".into() }, [0.0; 2], vec![Some(p)]);
        let Ok(Made::Python(s)) = g.make(o) else { panic!() };
        assert!(s.contains("def f(a):") && s.contains(r#"_names = ["a - \"x\".tif"]"#) && s.contains(r#"name="double""#), "{s}");
        g.nodes[1].kind = NodeKind::Python { code: "x = 1".into() };
        assert!(g.make(o).is_err());
    }
}
