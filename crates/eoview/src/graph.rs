//! The Graph tab (DESIGN section 18): the node editor of the crate `egui-noodle`, with the nodes of eoview.
//! Sources are layers of a view, operations are the filters, the layer math and the aggregates of the engine
//! (Rust), a Python node runs a function of the user, and an output node shows its input as a layer of a view.
//!
//! The graph is live: a change in a node makes the layer of each output again (after a short pause, as a
//! slider moves), in place of the old layer. The view keeps the old tiles until the new tiles come. Only a
//! Python node waits for its Run button. The workspace file keeps the graph.

use crate::app::App;
use crate::lang::{t, tf};
use crate::layer::{Kind, LayerSave, OpSave};
use egui::Color32;
use egui_noodle::{Editor, NodeUi, Viewer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// The code of `def f(a, b, ...)`: it gets one array for each parameter of `f`.
    Python { code: String },
    /// The layer `name` in view `view` (0: the active view at the first change).
    Output { name: String, #[serde(default)] view: u32 },
}

pub type Graph = egui_noodle::Graph<NodeKind>;

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
    editor: Editor,
    /// The time of the last change of a node.
    edit: Option<Instant>,
    /// The operation that each output node asked for, until its layer has it.
    asked: HashMap<u32, Option<OpSave>>,
    /// The message of each output node that cannot make its layer.
    errors: HashMap<u32, String>,
}

/// The parameters of `def f(...)` in the code of a Python node: one input for each.
fn py_args(code: &str) -> Vec<String> {
    let Some(l) = code.lines().find(|l| l.starts_with("def f(")) else { return vec![] };
    let args = l["def f(".len()..].split(')').next().unwrap_or("");
    args.split(',').map(|a| a.split([':', '=']).next().unwrap_or("").trim().to_string()).filter(|a| !a.is_empty()).collect()
}

impl NodeKind {
    fn n_inputs(&self) -> usize {
        match self {
            NodeKind::Source { .. } => 0,
            NodeKind::Filter(f) => f.inputs(),
            NodeKind::Math { names, .. } => names.len(),
            NodeKind::Python { code } => py_args(code).len(),
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

    /// An estimate of the height of the node, for the layout.
    fn height(&self) -> f32 {
        match self {
            NodeKind::Source { .. } => 60.0,
            NodeKind::Filter(f) => 70.0 + 24.0 * (f.clone().params().len() + f.inputs()) as f32,
            NodeKind::Math { .. } => 130.0,
            NodeKind::Agg { .. } => 150.0,
            NodeKind::Python { .. } => 220.0,
            NodeKind::Output { .. } => 300.0,
        }
    }
}

/// The name of a source of a saved layer: its file and its band.
fn source_name(s: &LayerSave) -> String {
    let file = std::path::Path::new(&s.path).file_name().map_or(s.path.clone(), |n| n.to_string_lossy().into());
    if s.band.is_empty() { file } else { format!("{} - {file}", s.band) }
}

/// The operations of eoview on a graph of `NodeKind`.
pub trait Ops {
    /// Add a node of `kind` (with its inputs, or the links `inputs`).
    fn add_kind(&mut self, kind: NodeKind, pos: [f32; 2], inputs: Vec<Option<u32>>) -> u32;
    /// Add the tree of the saved layer `s`: a source, or an operation with the nodes of its inputs.
    fn add_save(&mut self, s: &LayerSave, name: Option<String>) -> u32;
    /// The saved layer of node `id`: a source, or a layer with the operation of the node and the saved layers of
    /// its inputs. A muted node gives its first input.
    fn save_of(&self, id: u32) -> Result<LayerSave, String>;
    /// What the output node `out` makes.
    fn make(&self, out: u32) -> Result<Made, String>;
    /// Put the nodes in columns.
    fn arrange(&mut self);
}

/// The graph of the saved layer `s` of view `view`: its tree, and an output node.
pub fn of_save(s: &LayerSave, name: String, view: u32) -> Graph {
    let mut g = Graph::default();
    let root = g.add_save(s, Some(name.clone()));
    g.add_kind(NodeKind::Output { name, view }, [0.0; 2], vec![Some(root)]);
    g.arrange();
    g
}

impl Ops for Graph {
    fn add_kind(&mut self, kind: NodeKind, pos: [f32; 2], inputs: Vec<Option<u32>>) -> u32 {
        let n = kind.n_inputs();
        self.add(kind, pos, n, inputs)
    }

    fn add_save(&mut self, s: &LayerSave, name: Option<String>) -> u32 {
        let (kind, ins): (NodeKind, Vec<LayerSave>) = match &s.op {
            None => return self.add_kind(NodeKind::Source { name: name.unwrap_or_else(|| source_name(s)), layer: s.clone() }, [0.0; 2], vec![]),
            Some(OpSave::Filter { filter, inputs }) => (NodeKind::Filter(filter.clone()), inputs.clone()),
            Some(OpSave::Math { expr, names, inputs }) => (NodeKind::Math { expr: expr.clone(), names: names.clone() }, inputs.clone()),
            Some(OpSave::Agg { how, source, first, last }) => (NodeKind::Agg { how: how.clone(), first: *first, last: *last, dim: String::new() }, vec![(**source).clone()]),
            Some(OpSave::AggDim { how, source, dim }) => (NodeKind::Agg { how: how.clone(), first: 0, last: 0, dim: dim.clone() }, vec![(**source).clone()]),
        };
        let ids = ins.iter().map(|i| Some(self.add_save(i, None))).collect();
        self.add_kind(kind, [0.0; 2], ids)
    }

    fn save_of(&self, id: u32) -> Result<LayerSave, String> {
        let n = self.node(id).ok_or("no node")?;
        if n.muted && let Some(&Some(i)) = n.inputs.first() {
            return self.save_of(i);
        }
        let mut ins = vec![];
        for i in &n.inputs {
            ins.push(self.save_of(i.ok_or_else(|| tf("The node {} has an input without link.", &[&n.data.title()]))?)?);
        }
        let op = match &n.data {
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
        (s.path, s.op, s.series, s.step) = (n.data.title(), Some(op), vec![], 0);
        Ok(s)
    }

    fn make(&self, out: u32) -> Result<Made, String> {
        let Some(egui_noodle::Node { data: NodeKind::Output { name, .. }, inputs, .. }) = self.node(out) else { return Err(t("Not an output node.").into()) };
        let src = inputs.first().copied().flatten().ok_or(t("Link the output to a node."))?;
        let n = self.node(src).ok_or("no node")?;
        if let NodeKind::Python { code } = &n.data {
            let mut names = vec![];
            for i in &n.inputs {
                match i.and_then(|i| self.node(i)).map(|s| &s.data) {
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

    fn arrange(&mut self) {
        self.layout(NODE_W, |n| n.data.height());
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

/// The nodes of eoview for the editor: the views and their layers (the output nodes edit the display of their
/// layer), the sources of the active view for the add menu.
struct Nodes<'a> {
    panes: &'a mut Vec<crate::app::Pane>,
    active: u32,
    views: Vec<(u32, String)>,
    layers: Vec<(String, Option<LayerSave>)>,
    errors: &'a HashMap<u32, String>,
    /// A Python node to run (its Run button).
    run: Option<u32>,
}

impl Viewer<NodeKind> for Nodes<'_> {
    fn title(&mut self, n: &NodeKind) -> String {
        n.title()
    }

    fn color(&mut self, n: &NodeKind) -> Color32 {
        n.color()
    }

    fn width(&mut self, _: &NodeKind) -> f32 {
        NODE_W
    }

    fn inputs(&mut self, n: &NodeKind) -> Vec<String> {
        match n {
            NodeKind::Math { names, .. } => names.clone(),
            NodeKind::Python { code } => py_args(code),
            k if k.n_inputs() == 1 => vec![t("Input").into()],
            k => (1..=k.n_inputs()).map(|i| tf("Input {}", &[&i.to_string()])).collect(),
        }
    }

    fn output(&mut self, n: &NodeKind) -> Option<String> {
        (!matches!(n, NodeKind::Output { .. })).then(|| t("Result").into())
    }

    fn catalog(&mut self) -> Vec<(String, String, NodeKind)> {
        let mut v: Vec<(String, String, NodeKind)> = self.layers.iter().filter_map(|(n, s)| Some((t("Source").to_string(), n.clone(), NodeKind::Source { name: n.clone(), layer: s.clone()? }))).collect();
        for (gr, name, f, _) in crate::layer::FILTERS {
            v.push((t(gr).to_string(), t(name).to_string(), NodeKind::Filter(f.clone())));
        }
        v.push((t("Math").into(), t("Layer math").into(), NodeKind::Math { expr: "a + b".into(), names: vec!["a".into(), "b".into()] }));
        v.push((t("Time").into(), t("Aggregate").into(), NodeKind::Agg { how: eo_cache::Agg::ALL[0].1.into(), first: 0, last: 0, dim: String::new() }));
        v.push(("Python".into(), "Python".into(), NodeKind::Python { code: "def f(a):\n    return a\n".into() }));
        v.push((t("Output").into(), t("Output").into(), NodeKind::Output { name: t("Graph").into(), view: 0 }));
        v
    }

    fn body(&mut self, id: u32, n: &mut NodeKind, ui: &mut NodeUi) {
        match n {
            NodeKind::Source { .. } => {}
            NodeKind::Filter(f) => {
                for (name, v) in f.params() {
                    let speed = (v.abs() * 0.005).max(0.005);
                    ui.value(t(name), v, speed);
                }
            }
            NodeKind::Math { expr, names } => {
                ui.text_field(expr);
                if ui.button(t("+ input")) {
                    names.push(crate::app::math_name(names.len()));
                    ui.mark_changed();
                }
            }
            NodeKind::Agg { how, first, last, dim } => {
                let all: Vec<String> = eo_cache::Agg::ALL.iter().map(|a| t(a.1).to_string()).collect();
                let mut k = eo_cache::Agg::ALL.iter().position(|a| a.1 == how).unwrap_or(0);
                if ui.choice("", &mut k, &all) {
                    *how = eo_cache::Agg::ALL[k].1.into();
                }
                let (mut a, mut b) = (*first as i64, *last as i64);
                ui.int(t("first"), &mut a, 0.1);
                ui.int(t("last"), &mut b, 0.1);
                (*first, *last) = (a.max(0) as usize, b.max(0) as usize);
                ui.label(t("dimension (empty: time)"));
                ui.text_field(dim);
            }
            NodeKind::Python { code } => {
                ui.code(code, 6);
                if ui.button(t("Run")) {
                    self.run = Some(id);
                }
            }
            NodeKind::Output { name, view } => {
                ui.text_field(name);
                let titles: Vec<String> = self.views.iter().map(|v| v.1.clone()).collect();
                let cur = if *view == 0 { self.active } else { *view };
                let mut k = self.views.iter().position(|v| v.0 == cur).unwrap_or(0);
                if ui.choice(t("View"), &mut k, &titles) && let Some(v) = self.views.get(k) {
                    *view = v.0;
                }
                if let Some(e) = self.errors.get(&id) {
                    ui.error(e);
                }
                let layer = self.panes.iter_mut().find(|p| p.id == cur).and_then(|p| p.layers.iter_mut().find(|l| l.path == *name && l.op.is_some()));
                match layer {
                    Some(l) => {
                        ui.separator();
                        let maps: Vec<String> = crate::colormaps::all().iter().map(|c| c.name.clone()).collect();
                        let mut c = l.cmap;
                        if ui.choice(t("Colors"), &mut c, &maps) {
                            l.set_cmap(c);
                        }
                        let speed = ((l.st[0].hi - l.st[0].lo).abs() as f64 * 0.002).max(1e-6);
                        let (mut lo, mut hi) = (l.st[0].lo as f64, l.st[0].hi as f64);
                        ui.value(t("min"), &mut lo, speed);
                        ui.value(t("max"), &mut hi, speed);
                        (l.st[0].lo, l.st[0].hi) = (lo as f32, hi as f32);
                        if ui.button(t("Automatic stretch")) {
                            l.auto();
                        }
                        ui.toggle(t("Invert"), &mut l.invert);
                        ui.toggle(t("Visible"), &mut l.visible);
                        let mut o = l.opacity as f64;
                        ui.slider(t("Opacity"), &mut o, 0.0..=1.0);
                        l.opacity = o as f32;
                    }
                    None if self.errors.get(&id).is_none() => ui.label(t("computing...")),
                    None => {}
                }
            }
        }
    }
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
        let mut show = false;
        ui.horizontal(|ui| {
            show = ui.button(t("Show the selected layer")).on_hover_text(t("The operations of the selected layer as a graph")).clicked();
            if ui.button(t("Clear")).clicked() {
                g = Graph::default();
                st = GraphUi::default();
            }
            ui.label(egui::RichText::new(t("Shift+A: add a node. Wheel: zoom. Middle button: pan. Drag from a socket: link. X: remove, M: mute, H: collapse, Shift+D: duplicate, Home: all.")).small().weak());
        });
        st.editor.labels = egui_noodle::Labels { search: t("Search").into(), muted: t("muted").into() };
        let mut nodes = Nodes { panes: &mut self.panes, active: self.active, views, layers, errors: &st.errors, run: None };
        let r = st.editor.show(ui, &mut g, &mut nodes);
        let run = nodes.run;
        if r.changed {
            st.edit = Some(Instant::now());
        }
        self.graph = g;
        self.graph_ui = st;
        if show {
            self.graph_show(self.active);
        }
        if let Some(p) = run {
            let outs: Vec<u32> = self.graph.nodes.iter().filter(|n| matches!(n.data, NodeKind::Output { .. }) && n.inputs.first() == Some(&Some(p))).map(|n| n.id).collect();
            for o in outs {
                self.graph_make(o);
            }
        }
        self.graph_live(ui.ctx());
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
        let outs: Vec<(u32, String, u32)> = self.graph.nodes.iter().filter_map(|n| match &n.data {
            NodeKind::Output { name, view } => Some((n.id, name.clone(), *view)),
            _ => None,
        }).collect();
        for (id, name, view) in outs {
            let s = match self.graph.make(id) {
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
            if let Some(NodeKind::Output { view: v, .. }) = self.graph.node_mut(id).map(|n| &mut n.data) {
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
        self.graph = of_save(&s, name, id);
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
        let a = g.add_kind(NodeKind::Source { name: "a".into(), layer: source("1") }, [0.0; 2], vec![]);
        let f = g.add_kind(NodeKind::Filter(eo_cache::Filter::Gaussian { sigma: 2.0 }), [0.0; 2], vec![]);
        let o = g.add_kind(NodeKind::Output { name: "out".into(), view: 0 }, [0.0; 2], vec![Some(f)]);
        assert!(g.make(o).is_err(), "an input without link");
        g.nodes[1].inputs[0] = Some(a);
        let Ok(Made::Layer(name, s)) = g.make(o) else { panic!() };
        assert_eq!(name, "out");
        assert_eq!(s.op, Some(OpSave::Filter { filter: eo_cache::Filter::Gaussian { sigma: 2.0 }, inputs: vec![source("1")] }));
        let back = of_save(&s, name, 0);
        assert_eq!(back.nodes.len(), 3);
        assert_eq!(back.make(3).unwrap(), Made::Layer("out".into(), s));
        // The workspace file keeps the graph.
        assert_eq!(serde_json::from_str::<Graph>(&serde_json::to_string(&g).unwrap()).unwrap(), g);
        g.nodes[1].muted = true;
        assert!(g.make(o).is_err(), "a muted operation gives the source: not an operation");
    }

    /// The script of a Python node reads its sources by name, and refuses code without `def f(`. The
    /// parameters of `f` are the inputs.
    #[test]
    fn python_node_script() {
        assert_eq!(py_args("import x\ndef f(a, b: float, c=2):\n    pass"), ["a", "b", "c"]);
        let mut g = Graph::default();
        let a = g.add_kind(NodeKind::Source { name: "a - \"x\".tif".into(), layer: source("1") }, [0.0; 2], vec![]);
        let p = g.add_kind(NodeKind::Python { code: "def f(a):\n    return a * 2\n".into() }, [0.0; 2], vec![Some(a)]);
        let o = g.add_kind(NodeKind::Output { name: "double".into(), view: 0 }, [0.0; 2], vec![Some(p)]);
        let Ok(Made::Python(s)) = g.make(o) else { panic!() };
        assert!(s.contains("def f(a):") && s.contains(r#"_names = ["a - \"x\".tif"]"#) && s.contains(r#"name="double""#), "{s}");
        g.nodes[1].data = NodeKind::Python { code: "x = 1".into() };
        assert!(g.make(o).is_err());
    }
}
