//! One layer of a view: a product and its composite (one band, RGB or band math), with its own stretch,
//! color map and opacity. A layer can have time steps: the steps of the time dimension of its product, or
//! a list of products (one for each step).
use eo_cache::Layer;
use eo_render::bandmath::{self, Node};
use eo_render::{LayerParams, LayerSpec, Mode};
use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const CMAPS: &[(&str, &[u32])] = &[
    ("Gray", &[0x000000, 0xFFFFFF]),
    ("Viridis", &[0x440154, 0x482878, 0x3E4A89, 0x31688E, 0x26828E, 0x1F9E89, 0x35B779, 0x6DCD59, 0xB4DE2C, 0xFDE725]),
    ("Magma", &[0x000004, 0x180F3D, 0x440F76, 0x721F81, 0x9E2F7F, 0xCD4071, 0xF1605D, 0xFD9668, 0xFECA8D, 0xFCFDBF]),
    ("Inferno", &[0x000004, 0x1B0C41, 0x4A0C6B, 0x781C6D, 0xA52C60, 0xCF4446, 0xED6925, 0xFB9B06, 0xF7D13D, 0xFCFFA4]),
    ("Plasma", &[0x0D0887, 0x46039F, 0x7201A8, 0x9C179E, 0xBD3786, 0xD8576B, 0xED7953, 0xFB9F3A, 0xFDCA26, 0xF0F921]),
    ("Cividis", &[0x00224E, 0x123570, 0x3B496C, 0x575D6D, 0x707173, 0x8A8779, 0xA69D75, 0xC4B56C, 0xE4CF5B, 0xFEE838]),
    ("Turbo", &[0x30123B, 0x4662D7, 0x36AAF9, 0x1AE4B6, 0x72FE5E, 0xC8EF34, 0xFABA39, 0xF66B19, 0xCA2A04, 0x7A0403]),
    ("Jet", &[0x00007F, 0x0000FF, 0x007FFF, 0x00FFFF, 0x7FFF7F, 0xFFFF00, 0xFF7F00, 0xFF0000, 0x7F0000]),
    ("Hot", &[0x000000, 0xE60000, 0xFFD200, 0xFFFFFF]),
    ("Terrain", &[0x333399, 0x0294FA, 0x20D073, 0xFEFE98, 0x805C54, 0xFFFFFF]),
    ("RdBu", &[0x67001F, 0xB2182B, 0xD6604D, 0xF4A582, 0xFDDBC7, 0xF7F7F7, 0xD1E5F0, 0x92C5DE, 0x4393C3, 0x2166AC, 0x053061]),
    ("RdYlGn", &[0xA50026, 0xD73027, 0xF46D43, 0xFDAE61, 0xFEE08B, 0xFFFFBF, 0xD9EF8B, 0xA6D96A, 0x66BD63, 0x1A9850, 0x006837]),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Kind {
    Band,
    Rgb,
    Expr,
}

/// Presets: name, kind, expressions, color map, dB for each channel.
pub const PRESETS: &[(&str, Kind, [&str; 3], &str, bool)] = &[
    ("True color", Kind::Rgb, ["B04", "B03", "B02"], "Gray", false),
    ("False color", Kind::Rgb, ["B08", "B04", "B03"], "Gray", false),
    ("NDVI", Kind::Expr, ["(B08 - B04) / (B08 + B04)", "", ""], "RdYlGn", false),
    ("NDWI", Kind::Expr, ["(B03 - B08) / (B03 + B08)", "", ""], "RdBu", false),
    ("Dual-pol SAR", Kind::Rgb, ["VV", "VH", "VV / VH"], "Gray", true),
    ("OLCI true color", Kind::Rgb, ["Oa08_radiance", "Oa06_radiance", "Oa04_radiance"], "Gray", false),
];

/// Presets that a new layer uses first, if its bands exist.
const DEFAULT_PRESETS: &[&str] = &["True color", "Dual-pol SAR", "OLCI true color"];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stretch {
    pub lo: f32,
    pub hi: f32,
    pub gamma: f32,
    pub db: bool,
}

impl Default for Stretch {
    fn default() -> Self {
        Stretch { lo: 0.0, hi: 1.0, gamma: 1.0, db: false }
    }
}

/// A value that a composite can use: one band of one variable.
#[derive(Clone)]
pub struct Chan {
    /// Name in expressions, for example B04 or VV.
    pub id: String,
    pub var: usize,
    pub choice: usize,
}

/// Name in expressions: letters, digits and '_'. "Band 3" becomes "b3".
fn ident(s: &str) -> String {
    let s = s.rsplit('/').next().unwrap_or(s);
    let s = s.strip_prefix("Band ").map_or(s.to_string(), |n| format!("b{n}"));
    let s: String = s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let s = s.trim_matches('_').to_string();
    if s.starts_with(|c: char| c.is_ascii_digit()) { format!("b{s}") } else { s }
}

/// Node of the product tree of the side panel: a group of the product, with its groups and its channels.
/// The readers give the groups (`Variable::group`): they depend on the format.
#[derive(Clone, Default)]
pub struct Group {
    pub name: String,
    pub groups: Vec<Group>,
    pub chans: Vec<usize>,
    /// The group is a color image (see `is_color_var`): its red, green and blue channels.
    pub color: Option<[usize; 3]>,
}

impl Group {
    fn child(&mut self, name: &str) -> &mut Group {
        let i = match self.groups.iter().position(|g| g.name == name) {
            Some(i) => i,
            None => {
                self.groups.push(Group { name: name.into(), ..Default::default() });
                self.groups.len() - 1
            }
        };
        &mut self.groups[i]
    }

    /// Number of channels in the group and its groups.
    pub fn count(&self) -> usize {
        self.chans.len() + self.groups.iter().map(Group::count).sum::<usize>()
    }

    /// True if the group or one of its groups contains one of `chans`.
    pub fn has_any(&self, chans: &[usize]) -> bool {
        self.chans.iter().any(|c| chans.contains(c)) || self.groups.iter().any(|g| g.has_any(chans))
    }
}

/// True if the bands of a variable are the colors of an image: the reader names the first three bands
/// red, green and blue (TIFF photometric interpretation, JP2 colourspace, NITF IREP, Sentinel-2 TCI).
/// The number of bands is not a sign: three bands can be data, for example angles.
fn is_color_var(v: &eo_core::Variable) -> bool {
    v.bands.len() >= 3 && v.bands.iter().zip(["red", "green", "blue"]).all(|(b, c)| b.eq_ignore_ascii_case(c))
}

/// Product tree: the group of each variable is its `group`, or the directory part of its name. A variable
/// with more than one band is a group of its bands.
fn contents(p: &eo_core::Product, chans: &[Chan]) -> Group {
    let mut root = Group::default();
    for (var, v) in p.vars.iter().enumerate() {
        let (dir, leaf) = v.name.rsplit_once('/').unwrap_or(("", &v.name));
        let mut g = &mut root;
        for part in (if v.group.is_empty() { dir } else { &v.group }).split('/').filter(|s| !s.is_empty()) {
            g = g.child(part);
        }
        let cs: Vec<usize> = (0..chans.len()).filter(|&i| chans[i].var == var).collect();
        if cs.len() > 1 {
            let sub = g.child(leaf);
            sub.color = is_color_var(v).then(|| [cs[0], cs[1], cs[2]]);
            sub.chans = cs;
        } else {
            g.chans.extend(cs);
        }
    }
    root
}

fn channels(p: &eo_core::Product) -> Vec<Chan> {
    let mut out: Vec<Chan> = vec![];
    for (var, v) in p.vars.iter().enumerate() {
        let ch = Layer::choices(v);
        for (choice, c) in ch.iter().enumerate() {
            let base = if ch.len() == 1 { v.name.split([' ', '(']).next().unwrap_or(&v.name).to_string() } else { c.clone() };
            let mut id = ident(&base);
            if out.iter().any(|o| o.id.eq_ignore_ascii_case(&id)) {
                id = format!("{id}_{var}");
            }
            out.push(Chan { id, var, choice });
        }
    }
    out
}

fn db(v: f32) -> f32 {
    20.0 * v.abs().max(1e-10).log10()
}

pub fn hex(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

pub fn lut(stops: &[[u8; 3]]) -> Vec<[u8; 4]> {
    let n = stops.len() - 1;
    (0..256)
        .map(|i| {
            let t = i as f32 / 255.0 * n as f32;
            let j = (t as usize).min(n - 1);
            let f = t - j as f32;
            let (a, b) = (stops[j], stops[j + 1]);
            let m = |k: usize| (a[k] as f32 + (b[k] as f32 - a[k] as f32) * f).round() as u8;
            [m(0), m(1), m(2), 255]
        })
        .collect()
}

/// Bins of the histograms of the stretch panel.
pub const BINS: usize = 128;

/// A time step of a layer.
#[derive(Clone)]
pub struct Step {
    /// Product of the step (a path or a URL). Empty: the step is on the time dimension of the product of
    /// the layer.
    pub path: String,
    /// Time of the step (seconds since 1970, see `eo_core::time`). NaN: not known.
    pub t: f64,
    /// A layer of the product of the step, when the product is open (a step with its own product).
    pub ds: Option<Arc<Layer>>,
}

/// Steps of the time dimension of a product: the variable with the most steps gives them.
fn time_steps(p: &eo_core::Product) -> Vec<Step> {
    let Some(v) = p.vars.iter().max_by_key(|v| v.steps()).filter(|v| v.steps() > 1) else { return vec![] };
    (0..v.steps() as usize).map(|i| Step { path: String::new(), t: v.times.get(i).copied().unwrap_or(f64::NAN), ds: None }).collect()
}

#[derive(Clone)]
pub struct MapLayer {
    pub uid: u64,
    /// Position in the stack while a workspace loads (the layers can arrive in a different order).
    pub order: usize,
    /// File path or URL.
    pub path: String,
    pub name: String,
    pub chans: Vec<Chan>,
    /// Layers that are ready, by (time step, variable, choice).
    pub cache: HashMap<(usize, usize, usize), Arc<Layer>>,
    /// Time steps. Empty or one step: the layer has no timeline.
    pub steps: Vec<Step>,
    /// The step that the user selected, and the step that the layer shows (the selected step shows when
    /// its layers are ready).
    pub step: usize,
    pub shown: usize,
    /// Interval in steps between the selected step and the steps that the view loads ahead: 1, or the
    /// interval between the frames of a render.
    pub stride: usize,
    pub kind: Kind,
    pub band: usize,
    pub rgb: [String; 3],
    pub expr: String,
    pub err: Option<String>,
    pub trees: Vec<Node>,
    /// Channels of the inputs, in input order.
    pub used: Vec<usize>,
    pub st: [Stretch; 3],
    pub clip: f32,
    pub cmap: usize,
    pub stops: Vec<[u8; 3]>,
    pub invert: bool,
    pub opacity: f32,
    pub visible: bool,
    /// Layers of the used channels, when all are ready.
    pub inputs: Vec<Arc<Layer>>,
    /// Stretch again when the inputs are ready.
    pub auto_pending: bool,
    /// Histograms of the values of each channel (sample of the product), and their range.
    pub hist: Vec<(Vec<u32>, f32, f32)>,
    /// Product tree, and the filter text of the side panel.
    pub contents: Group,
    pub filter: String,
}

impl MapLayer {
    /// A new layer with the first layer of a dataset. The composite is a default preset, or the first band.
    pub fn new(uid: u64, path: String, first: Arc<Layer>) -> MapLayer {
        let name = std::path::Path::new(&path).file_name().map_or(path.clone(), |n| n.to_string_lossy().into());
        let mut m = MapLayer {
            uid,
            order: usize::MAX,
            path,
            name,
            chans: channels(&first.ds.product),
            cache: HashMap::new(),
            steps: time_steps(&first.ds.product),
            step: 0,
            shown: 0,
            stride: 1,
            kind: Kind::Band,
            band: 0,
            rgb: Default::default(),
            expr: String::new(),
            err: None,
            trees: vec![],
            used: vec![],
            st: [Stretch::default(); 3],
            clip: 2.0,
            cmap: 0,
            stops: CMAPS[0].1.iter().map(|&c| hex(c)).collect(),
            invert: false,
            opacity: 1.0,
            visible: true,
            inputs: vec![],
            auto_pending: true,
            hist: vec![],
            contents: Group::default(),
            filter: String::new(),
        };
        m.contents = contents(&first.ds.product, &m.chans);
        if first.var().levels[0].dtype.is_complex() {
            m.st[0].db = first.part == eo_cache::Part::Amp;
        }
        m.cache.insert((0, first.var, first.choice), first);
        if let Some(p) = PRESETS.iter().find(|p| DEFAULT_PRESETS.contains(&p.0) && m.preset_ok(p)) {
            m.set_preset(p);
        } else if let Some(ids) = m.color_bands() {
            m.kind = Kind::Rgb;
            m.rgb = ids;
        }
        m
    }

    /// Bands of a color image: the product has one variable, and its bands are colors.
    // ponytail: an alpha band is not used. Add it to the composite if images with transparency need it.
    fn color_bands(&self) -> Option<[String; 3]> {
        let p = &self.any()?.ds.product;
        let [v] = &p.vars[..] else { return None };
        (is_color_var(v) && self.chans.len() >= 3).then(|| [0, 1, 2].map(|k| self.chans[k].id.clone()))
    }

    /// True if the pixels are colors: an RGB composite, each channel one band of 8-bit data. Then the
    /// default is no stretch.
    pub fn is_color(&self) -> bool {
        self.kind == Kind::Rgb
            && !self.inputs.is_empty()
            && self.trees.iter().all(|t| matches!(t, Node::Var(_)))
            && self.inputs.iter().all(|l| l.var().levels[0].dtype == eo_core::DType::U8)
    }

    /// No stretch: the stored values 0 to 255 are the display values.
    pub fn as_is(&mut self) {
        for k in 0..self.trees.len().min(3) {
            if let Node::Var(j) = self.trees[k]
                && let Some(l) = self.inputs.get(j)
            {
                let v = l.var();
                self.st[k] = Stretch { lo: v.offset as f32, hi: (255.0 * v.scale + v.offset) as f32, gamma: 1.0, db: false };
            }
        }
        self.histograms();
    }

    pub fn names(&self) -> Vec<String> {
        self.chans.iter().map(|c| c.id.clone()).collect()
    }

    /// A layer of the product (for a list of products: of the product of the selected step, if it is open).
    pub fn any(&self) -> Option<&Arc<Layer>> {
        self.steps.get(self.step).and_then(|s| s.ds.as_ref()).or_else(|| self.cache.values().next())
    }

    /// A layer of the product of step `s`: the engine makes the other channels of the step from it.
    /// None: the product of the step is not open.
    pub fn base(&self, s: usize) -> Option<&Arc<Layer>> {
        match self.steps.get(s) {
            Some(st) if !st.path.is_empty() => st.ds.as_ref(),
            _ => self.cache.values().next(),
        }
    }

    /// Index on the time dimension of variable `var` for step `s`.
    pub fn time_of(&self, s: usize, var: usize) -> u64 {
        match (self.steps.get(s), self.cache.values().next()) {
            (Some(st), Some(l)) if st.path.is_empty() => (s as u64).min(l.ds.product.vars[var].steps() - 1),
            _ => 0,
        }
    }

    /// (variable, choice) of the used channels that are not ready for step `s`.
    pub fn missing(&self, s: usize) -> Vec<(usize, usize)> {
        let m: HashSet<(usize, usize)> = self.used.iter().map(|&c| (self.chans[c].var, self.chans[c].choice)).filter(|k| !self.cache.contains_key(&(s, k.0, k.1))).collect();
        m.into_iter().collect()
    }

    /// Layers of the used channels for step `s`, if all are ready.
    pub fn inputs_at(&self, s: usize) -> Option<Vec<Arc<Layer>>> {
        self.used.iter().map(|&c| self.cache.get(&(s, self.chans[c].var, self.chans[c].choice)).cloned()).collect()
    }

    /// Go to time step `s`. Until the layers of the step are ready, the layer shows the step before.
    /// A layer of each visited step stays in the cache (with a sample of 64 K values). A long data cube
    /// has thousands of steps: with more than 64 layers, only the layers of the selected step, of the
    /// step that shows and of the next steps stay.
    pub fn set_step(&mut self, s: usize) {
        self.step = s.min(self.steps.len().saturating_sub(1));
        if self.cache.len() > 64 {
            let (n, step, shown, st) = (self.steps.len().max(1), self.step, self.shown, self.stride.max(1));
            self.cache.retain(|k, _| k.0 == step || k.0 == shown || (1..=crate::app::AHEAD).any(|a| (step + a * st) % n == k.0));
        }
        self.ready();
    }

    /// Step nearest to time `t`. Without times: step `s`.
    pub fn nearest(&self, t: f64, s: usize) -> usize {
        let best = self.steps.iter().enumerate().filter(|x| x.1.t.is_finite()).min_by(|a, b| (a.1.t - t).abs().total_cmp(&(b.1.t - t).abs()));
        match best {
            Some((i, _)) if t.is_finite() => i,
            _ => s.min(self.steps.len().saturating_sub(1)),
        }
    }

    /// Text of step `s` for the timeline: its time, else the name of its product, else its number.
    pub fn step_label(&self, s: usize) -> String {
        match self.steps.get(s) {
            Some(st) if st.t.is_finite() => eo_core::time::text(st.t),
            Some(st) if !st.path.is_empty() => st.path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string(),
            _ => format!("step {}", s + 1),
        }
    }

    /// Make the layer a series of products: one time step for each (path, time). The first product is the
    /// product of the layer.
    pub fn set_series(&mut self, list: Vec<(String, f64)>) {
        let first = self.cache.values().next().cloned();
        self.steps = list.into_iter().map(|(path, t)| Step { path, t, ds: None }).collect();
        if let Some(s) = self.steps.first_mut() {
            s.ds = first;
        }
    }

    pub fn preset_ok(&self, p: &(&str, Kind, [&str; 3], &str, bool)) -> bool {
        let e: Vec<&str> = p.2.iter().copied().filter(|e| !e.is_empty()).collect();
        bandmath::parse(&e, &self.names()).is_ok()
    }

    pub fn set_preset(&mut self, p: &(&str, Kind, [&str; 3], &str, bool)) {
        self.kind = p.1;
        match p.1 {
            Kind::Rgb => self.rgb = p.2.map(String::from),
            _ => self.expr = p.2[0].into(),
        }
        if let Some(i) = CMAPS.iter().position(|c| c.0 == p.3) {
            self.set_cmap(i);
        }
        self.st.iter_mut().for_each(|s| s.db = p.4);
        self.auto_pending = true;
    }

    pub fn set_cmap(&mut self, i: usize) {
        self.cmap = i % CMAPS.len();
        self.stops = CMAPS[self.cmap].1.iter().map(|&c| hex(c)).collect();
    }

    /// Show the next or the previous band (one band mode).
    pub fn cycle_band(&mut self, d: i32) {
        let n = self.chans.len().max(1) as i32;
        self.kind = Kind::Band;
        self.band = (self.band as i32 + d).rem_euclid(n) as usize;
        self.auto_pending = true;
    }

    /// Parse the composite. Return the (variable, choice) of the used channels that are not ready for the
    /// selected step: the application asks the engine for them.
    pub fn compile(&mut self) -> Vec<(usize, usize)> {
        let names = self.names();
        let exprs: Vec<String> = match self.kind {
            Kind::Band => vec![names.get(self.band).cloned().unwrap_or_default()],
            Kind::Rgb => self.rgb.to_vec(),
            Kind::Expr => vec![self.expr.clone()],
        };
        let e: Vec<&str> = exprs.iter().map(String::as_str).collect();
        match bandmath::parse(&e, &names) {
            Ok((trees, used)) if used.len() <= eo_render::MAX_INPUTS => {
                self.trees = trees;
                self.used = used;
                self.err = None;
            }
            Ok(_) => {
                self.err = Some(format!("more than {} bands", eo_render::MAX_INPUTS));
                return vec![];
            }
            Err(e) => {
                self.err = Some(e);
                return vec![];
            }
        }
        self.inputs.clear();
        let missing = self.missing(self.step);
        if missing.is_empty() {
            self.ready();
        }
        missing
    }

    /// A channel layer of step `s` is ready. Return true if the view must make its inputs again: the
    /// selected step is complete and shows now, or a next step is complete (prefetch).
    pub fn loaded(&mut self, s: usize, l: Arc<Layer>) -> bool {
        self.cache.insert((s, l.var, l.choice), l);
        if s == self.step { (self.inputs.is_empty() || self.shown != s) && self.ready() } else { self.inputs_at(s).is_some() }
    }

    fn ready(&mut self) -> bool {
        let Some(v) = self.inputs_at(self.step) else { return false };
        self.inputs = v;
        self.shown = self.step;
        if self.auto_pending {
            self.auto_pending = false;
            if self.is_color() { self.as_is() } else { self.auto() }
        }
        self.histograms();
        true
    }

    fn gray(&self) -> bool {
        self.kind != Kind::Rgb
    }

    /// Values of channel k of the composite for the sample: the sample of the band, or the expression on
    /// the value pairs of layers on the same grid.
    fn values(&self, k: usize) -> Vec<f32> {
        let Some(t) = self.trees.get(k) else { return vec![] };
        let mut vals: Vec<f32> = match t {
            Node::Var(j) => self.inputs.get(*j).map_or(vec![], |l| l.sample.clone()),
            _ => {
                let len = self.inputs.first().map_or(0, |l| l.sample_at.len());
                if self.inputs.iter().all(|l| l.sample_at.len() == len) {
                    (0..len).map(|i| t.eval(&self.inputs.iter().map(|l| l.sample_at[i] as f64).collect::<Vec<_>>()) as f32).filter(|v| v.is_finite()).collect()
                } else {
                    vec![-1.0, 1.0]
                }
            }
        };
        if self.st[k].db {
            vals.iter_mut().for_each(|v| *v = db(*v));
        }
        vals.sort_unstable_by(f32::total_cmp);
        vals
    }

    /// Stretch limits of each channel from the sample percentiles.
    pub fn auto(&mut self) {
        let n = if self.gray() { 1 } else { 3 };
        for k in 0..n.min(self.trees.len()) {
            let vals = self.values(k);
            if vals.is_empty() {
                (self.st[k].lo, self.st[k].hi) = (0.0, 1.0);
                continue;
            }
            let q = |p: f32| vals[((vals.len() - 1) as f32 * p) as usize];
            let c = self.clip / 100.0;
            (self.st[k].lo, self.st[k].hi) = (q(c), q(1.0 - c));
            if self.st[k].hi <= self.st[k].lo {
                self.st[k].hi = self.st[k].lo + 1.0;
            }
        }
        self.histograms();
    }

    /// Histograms of the channels, between the 0.1 and 99.9 percentiles of the sample.
    pub fn histograms(&mut self) {
        let n = if self.gray() { 1 } else { 3 };
        self.hist = (0..n.min(self.trees.len()))
            .map(|k| {
                let v = self.values(k);
                if v.is_empty() {
                    return (vec![0; BINS], 0.0, 1.0);
                }
                let (lo, hi) = (v[v.len() / 1000], v[(v.len() - 1) * 999 / 1000]);
                let (lo, hi) = (lo.min(self.st[k].lo), hi.max(self.st[k].hi));
                let hi = if hi > lo { hi } else { lo + 1.0 };
                let mut b = vec![0u32; BINS];
                for x in v {
                    let i = ((x - lo) / (hi - lo) * BINS as f32) as isize;
                    if (0..BINS as isize).contains(&i) {
                        b[i as usize] += 1;
                    }
                }
                (b, lo, hi)
            })
            .collect();
    }

    /// Layer spec for the composite. `inputs` are the view input indices of the inputs of this layer.
    pub fn spec(&self, inputs: &[usize]) -> Option<LayerSpec> {
        let map = |j: usize| inputs[j];
        let w: Vec<String> = self.trees.iter().map(|t| t.wgsl_map(&map)).collect();
        let mode = if self.gray() { Mode::Gray(w.first()?.clone()) } else { Mode::Rgb([w.first()?.clone(), w.get(1)?.clone(), w.get(2)?.clone()]) };
        Some(LayerSpec { mode, inputs: inputs.to_vec() })
    }

    pub fn params(&self) -> LayerParams {
        let mut p = LayerParams { opacity: self.opacity, flags: (self.invert as u32) << 3, ..Default::default() };
        for (k, s) in self.st.iter().enumerate() {
            p.lo[k] = s.lo;
            p.hi[k] = if s.hi == s.lo { s.lo + 1e-6 } else { s.hi };
            p.gamma[k] = s.gamma;
            p.flags |= (s.db as u32) << k;
        }
        p
    }

    /// Default display CRS: the CRS of the layer; for a geolocation grid, the UTM zone of its center
    /// (conformal: no stretch), or polar stereographic above 84 degrees; for geolocation arrays (not read
    /// yet) WGS 84; else pixels.
    pub fn default_space(&self) -> Option<u32> {
        let l = self.inputs.first().or(self.any())?;
        match &l.var().georef {
            eo_core::Georef::Affine { crs, .. } => crs.epsg,
            eo_core::Georef::None => None,
            eo_core::Georef::Grid { lon, lat, .. } => {
                let k = lon.len() / 2;
                let (lo, la) = (lon[k], lat[k]);
                Some(match la {
                    _ if la > 84.0 => 3413,
                    _ if la < -84.0 => 3031,
                    _ => {
                        let zone = (((lo + 180.0) / 6.0).floor() as u32 + 1).clamp(1, 60);
                        if la >= 0.0 { 32600 + zone } else { 32700 + zone }
                    }
                })
            }
            _ => Some(4326),
        }
    }

    /// What the layer shows: the preset name, the band, the RGB bands or the expression.
    pub fn comp_name(&self) -> String {
        let exprs: [&str; 3] = [&self.rgb[0], &self.rgb[1], &self.rgb[2]];
        let preset = PRESETS.iter().find(|p| {
            p.1 == self.kind
                && match self.kind {
                    Kind::Rgb => p.2 == exprs,
                    Kind::Expr => p.2[0] == self.expr,
                    Kind::Band => false,
                }
        });
        match (preset, self.kind) {
            (Some(p), _) => p.0.into(),
            (None, Kind::Band) => self.chans.get(self.band).map_or(String::new(), |c| c.id.clone()),
            (None, Kind::Rgb) => self.rgb.join(" "),
            (None, Kind::Expr) => self.expr.clone(),
        }
    }

    /// Short name for lists and titles: what the layer shows, then the product name, at most `n` characters.
    pub fn label(&self, n: usize) -> String {
        let s = format!("{} - {}", self.comp_name(), self.name);
        if s.chars().count() <= n { s } else { format!("{}...", s.chars().take(n.saturating_sub(3)).collect::<String>()) }
    }

    /// Name of channel `c` for the user: the variable, and the band if the variable has more than one.
    /// Name of channel `c` in the product tree: the band of a variable with more than one band, else the
    /// last part of the variable name.
    pub fn chan_leaf(&self, c: usize) -> String {
        let (Some(ch), Some(l)) = (self.chans.get(c), self.any()) else { return String::new() };
        let v = &l.ds.product.vars[ch.var];
        let names = Layer::choices(v);
        if names.len() > 1 { names[ch.choice].clone() } else { v.name.rsplit('/').next().unwrap_or(&v.name).to_string() }
    }

    pub fn chan_label(&self, c: usize) -> String {
        let (Some(ch), Some(l)) = (self.chans.get(c), self.any()) else { return String::new() };
        let v = &l.ds.product.vars[ch.var];
        let names = Layer::choices(v);
        if names.len() > 1 { format!("{} - {}", v.name, names[ch.choice]) } else { v.name.clone() }
    }
}

/// Layer settings in a workspace file. No data and no credentials.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayerSave {
    pub path: String,
    pub kind: Kind,
    /// Band of the one band mode, by its name in expressions.
    pub band: String,
    pub rgb: [String; 3],
    pub expr: String,
    pub st: [Stretch; 3],
    pub clip: f32,
    pub cmap: String,
    pub stops: Vec<[u8; 3]>,
    pub invert: bool,
    pub opacity: f32,
    pub visible: bool,
    /// A series of products: the path and the time of each step. Empty: one product.
    #[serde(default)]
    pub series: Vec<(String, Option<f64>)>,
    /// Selected time step.
    #[serde(default)]
    pub step: usize,
}

/// Path without the query, the fragment and the user information of a URL: they can contain credentials
/// or signed tokens.
pub fn clean_path(p: &str) -> String {
    let Some((scheme, rest)) = p.split_once("://") else { return p.to_string() };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (host, path) = rest.split_once('/').map_or((rest, None), |(h, q)| (h, Some(q)));
    let host = host.rsplit('@').next().unwrap_or(host);
    match path {
        Some(q) => format!("{scheme}://{host}/{q}"),
        None => format!("{scheme}://{host}"),
    }
}

impl MapLayer {
    pub fn save(&self) -> LayerSave {
        LayerSave {
            path: clean_path(&self.path),
            kind: self.kind,
            band: self.chans.get(self.band).map_or(String::new(), |c| c.id.clone()),
            rgb: self.rgb.clone(),
            expr: self.expr.clone(),
            st: self.st,
            clip: self.clip,
            cmap: CMAPS[self.cmap].0.into(),
            stops: self.stops.clone(),
            invert: self.invert,
            opacity: self.opacity,
            visible: self.visible,
            series: self.steps.iter().filter(|s| !s.path.is_empty()).map(|s| (clean_path(&s.path), s.t.is_finite().then_some(s.t))).collect(),
            step: self.step,
        }
    }

    /// Settings of a workspace file. The stretch stays as saved.
    pub fn apply(&mut self, s: &LayerSave) {
        self.kind = s.kind;
        self.band = self.chans.iter().position(|c| c.id == s.band).unwrap_or(0);
        self.rgb = s.rgb.clone();
        self.expr = s.expr.clone();
        self.st = s.st;
        self.clip = s.clip;
        self.set_cmap(CMAPS.iter().position(|c| c.0 == s.cmap).unwrap_or(0));
        if s.stops.len() >= 2 {
            self.stops = s.stops.clone();
        }
        self.invert = s.invert;
        self.opacity = s.opacity;
        self.visible = s.visible;
        if !s.series.is_empty() {
            self.set_series(s.series.iter().map(|(p, t)| (p.clone(), t.unwrap_or(f64::NAN))).collect());
        }
        self.step = s.step.min(self.steps.len().saturating_sub(1));
        self.auto_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_path_removes_credentials() {
        assert_eq!(clean_path("https://user:pw@host.eu/a/b.tif?X-Amz-Signature=abc#f"), "https://host.eu/a/b.tif");
        assert_eq!(clean_path("s3://bucket/key.zarr"), "s3://bucket/key.zarr");
        assert_eq!(clean_path("https://host?token=1"), "https://host");
        assert_eq!(clean_path("/data/a?b.tif"), "/data/a?b.tif");
    }
}
