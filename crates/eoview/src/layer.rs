//! One layer of a view: a product and its composite (one band, RGB or band math), with its own stretch,
//! color map and opacity. A layer can have time steps: the steps of the time dimension of its product, or
//! a list of products (one for each step).
use eo_cache::Layer;
use eo_render::bandmath::{self, Node};
use eo_render::{LayerParams, LayerSpec, Mode};
use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// The color maps are in `colormaps.rs` (a library of more than 100 maps, and the maps of the user).

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Kind {
    Band,
    Rgb,
    Expr,
    /// A vector field (wind, current): `rgb[0]` and `rgb[1]` are the expressions of the components to
    /// the east and to the north. The view shows the speed with the color map, and arrows.
    Wind,
}

/// Color map of a new wind layer (Viridis): the white arrows show on all its colors.
pub const WIND_CMAP: usize = 1;

/// The components to the east and to the north of a vector field in the names of a product: the wind at
/// 10 m of a weather model, else the first pair that it knows. None: the product has no such pair.
pub fn wind_pair(names: &[String]) -> Option<[String; 2]> {
    const PAIRS: &[(&str, &str)] = &[
        ("u10", "v10"),
        // GRIB short names of ECMWF (10u, 10v): a name in expressions does not start with a digit.
        ("b10u", "b10v"),
        ("b10m_u_component_of_wind", "b10m_v_component_of_wind"),
        ("u100", "v100"),
        ("b100u", "b100v"),
        ("b100m_u_component_of_wind", "b100m_v_component_of_wind"),
        ("u_component_of_wind", "v_component_of_wind"),
        ("eastward_wind", "northward_wind"),
        ("uwnd", "vwnd"),
        ("ugrd", "vgrd"),
        ("uo", "vo"),
        ("u", "v"),
    ];
    let find = |n: &str| names.iter().find(|x| x.eq_ignore_ascii_case(n)).cloned();
    PAIRS.iter().find_map(|(u, v)| Some([find(u)?, find(v)?]))
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

/// The automatic range of a new layer (a preference).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StretchInit {
    /// From the minimum to the maximum of the data (of a sample of the data).
    #[default]
    MinMax,
    /// Without a part of the values at each end (`clip` percent): more contrast.
    Clip,
    /// The range of the data type (0 to 255 for 8-bit data, 0 to 65535 for 16-bit data), with the scale
    /// and the offset of the file. Floats and band math: from the minimum to the maximum.
    Type,
}

/// The automatic range of the preferences, and the percent of `StretchInit::Clip` (bits of an f32).
static INIT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static CLIP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x4000_0000);

/// Set the automatic range of the preferences (`App::apply_prefs`).
pub fn set_stretch_init(init: StretchInit, clip: f32) {
    use std::sync::atomic::Ordering::Relaxed;
    INIT.store(init as u8, Relaxed);
    CLIP.store(clip.to_bits(), Relaxed);
}

fn stretch_init() -> (StretchInit, f32) {
    use std::sync::atomic::Ordering::Relaxed;
    let i = match INIT.load(Relaxed) {
        1 => StretchInit::Clip,
        2 => StretchInit::Type,
        _ => StretchInit::MinMax,
    };
    (i, f32::from_bits(CLIP.load(Relaxed)))
}

/// The physical range of the values of a data type, with the scale and the offset of the variable. None:
/// a float or a complex type.
fn type_range(v: &eo_core::Variable, dtype: eo_core::DType) -> Option<(f32, f32)> {
    use eo_core::DType::*;
    let (a, b) = match dtype {
        U8 => (0.0, 255.0),
        I8 => (-128.0, 127.0),
        U16 => (0.0, 65535.0),
        I16 => (-32768.0, 32767.0),
        U32 => (0.0, u32::MAX as f64),
        I32 => (i32::MIN as f64, i32::MAX as f64),
        _ => return None,
    };
    let (x, y) = (a * v.scale + v.offset, b * v.scale + v.offset);
    Some((x.min(y) as f32, x.max(y) as f32))
}

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
    /// The scalars and the 1D variables of the group (indexes in `Product::small`).
    pub small: Vec<usize>,
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
        self.chans.len() + self.small.len() + self.groups.iter().map(Group::count).sum::<usize>()
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
    for (k, v) in p.small.iter().enumerate() {
        let dir = v.name.rsplit_once('/').map_or("", |d| d.0);
        let mut g = &mut root;
        for part in (if v.group.is_empty() { dir } else { &v.group }).split('/').filter(|s| !s.is_empty()) {
            g = g.child(part);
        }
        g.small.push(k);
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
    /// Layers that are ready, by (time step, variable, choice), with the generation of the pick that made them.
    pub cache: HashMap<(usize, usize, usize), (u64, Arc<Layer>)>,
    /// The image axes and the index of the other dimensions (DESIGN section 16), and its generation: a
    /// change of the pick makes the layers of the older generations not current.
    pub pick: eo_cache::Pick,
    pub pgen: u64,
    /// The pick generation of the inputs that show.
    pub shown_pgen: u64,
    /// Time steps. Empty or one step: the layer has no timeline.
    pub steps: Vec<Step>,
    /// The step that the user selected, and the step that the layer shows (the selected step shows when
    /// its layers are ready).
    pub step: usize,
    pub shown: usize,
    /// The first step and the last step with data, if the product gives the times of its data.
    pub valid: Option<(usize, usize)>,
    /// Interval in steps between the selected step and the steps that the view loads ahead: 1, or the
    /// interval between the frames of a render.
    pub stride: usize,
    /// The view shows a blend of the selected step and of the next step (`Pane::tmix`): the frames
    /// between two steps of a render.
    pub blend: bool,
    /// Wind mode: show the speed with the color map, the arrows, the particles.
    pub fill: bool,
    pub arrows: bool,
    pub particles: bool,
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
    /// The globe view takes its terrain from this layer (the first band, in meters), with this vertical
    /// exaggeration (DESIGN section 14).
    pub terrain: bool,
    pub exag: f32,
    /// Layers of the used channels, when all are ready.
    pub inputs: Vec<Arc<Layer>>,
    /// Stretch again when the inputs are ready.
    pub auto_pending: bool,
    /// The stretch is automatic (not a change of the user), and it is new (`App::same_stretch`).
    pub auto_st: bool,
    pub fresh: bool,
    /// Histograms of the values of each channel (sample of the product), and their range.
    pub hist: Vec<(Vec<u32>, f32, f32)>,
    /// Product tree, and the filter text of the side panel.
    pub contents: Group,
    pub filter: String,
    /// A computed layer: its operation (for the workspace file).
    pub op: Option<OpSave>,
}

impl MapLayer {
    /// A new layer with the first layer of a dataset. The composite is a default preset, or the first band.
    pub fn new(uid: u64, path: String, first: Arc<Layer>) -> MapLayer {
        let name = std::path::Path::new(&path).file_name().map_or(path.clone(), |n| n.to_string_lossy().into());
        let steps = time_steps(&first.ds.product);
        let valid = first.ds.product.valid.filter(|_| steps.len() > 1).map(|(a, b)| {
            let at = |t: f64| steps.partition_point(|s| s.t < t);
            (at(a).min(steps.len() - 1), at(b).saturating_sub(1).min(steps.len() - 1))
        });
        let mut m = MapLayer {
            uid,
            order: usize::MAX,
            path,
            name,
            chans: channels(&first.ds.product),
            cache: HashMap::new(),
            pick: first.pick(),
            pgen: 0,
            shown_pgen: 0,
            steps,
            // A cube opens at its first step with data.
            step: valid.map_or(0, |v| v.0),
            shown: valid.map_or(0, |v| v.0),
            valid,
            stride: 1,
            blend: false,
            fill: true,
            arrows: false,
            particles: true,
            kind: Kind::Band,
            band: 0,
            rgb: Default::default(),
            expr: String::new(),
            err: None,
            trees: vec![],
            used: vec![],
            st: [Stretch::default(); 3],
            clip: if stretch_init().1 > 0.0 { stretch_init().1 } else { 2.0 },
            cmap: 0,
            stops: crate::colormaps::stops(0),
            invert: false,
            opacity: 1.0,
            terrain: false,
            exag: 1.0,
            visible: true,
            inputs: vec![],
            auto_pending: true,
            auto_st: true,
            fresh: false,
            hist: vec![],
            contents: Group::default(),
            filter: String::new(),
            op: None,
        };
        m.contents = contents(&first.ds.product, &m.chans);
        if first.var().levels[0].dtype.is_complex() {
            m.st[0].db = first.part == eo_cache::Part::Amp;
        }
        m.cache.insert((0, first.var, first.choice), (0, first));
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
        self.steps.get(self.step).and_then(|s| s.ds.as_ref()).or_else(|| self.cache.values().next().map(|x| &x.1))
    }

    /// A layer of the product of step `s`: the engine makes the other channels of the step from it.
    /// None: the product of the step is not open.
    pub fn base(&self, s: usize) -> Option<&Arc<Layer>> {
        match self.steps.get(s) {
            Some(st) if !st.path.is_empty() => st.ds.as_ref(),
            _ => self.cache.values().next().map(|x| &x.1),
        }
    }

    /// Index on the time dimension of variable `var` for step `s`.
    pub fn time_of(&self, s: usize, var: usize) -> u64 {
        match (self.steps.get(s), self.cache.values().next().map(|x| &x.1)) {
            (Some(st), Some(l)) if st.path.is_empty() => (s as u64).min(l.ds.product.vars[var].steps() - 1),
            _ => 0,
        }
    }

    /// (variable, choice) of the used channels that are not ready for step `s`.
    pub fn missing(&self, s: usize) -> Vec<(usize, usize)> {
        let m: HashSet<(usize, usize)> = self.used.iter().map(|&c| (self.chans[c].var, self.chans[c].choice)).filter(|k| !self.cache.get(&(s, k.0, k.1)).is_some_and(|x| x.0 == self.pgen)).collect();
        m.into_iter().collect()
    }

    /// Layers of the used channels for step `s`, if all are ready.
    pub fn inputs_at(&self, s: usize) -> Option<Vec<Arc<Layer>>> {
        self.used.iter().map(|&c| self.cache.get(&(s, self.chans[c].var, self.chans[c].choice)).filter(|x| x.0 == self.pgen).map(|x| x.1.clone())).collect()
    }

    /// Layers of the used channels for the step after the step that shows (at the interval `stride`), if
    /// the layer blends two steps and all are ready.
    pub fn next_inputs(&self) -> Option<Vec<Arc<Layer>>> {
        if !self.blend || self.steps.len() < 2 {
            return None;
        }
        self.inputs_at((self.shown + self.stride.max(1)) % self.steps.len())
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
        let first = self.cache.values().next().map(|x| x.1.clone());
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
        if let Some(i) = crate::colormaps::find(p.3) {
            self.set_cmap(i);
        }
        self.st.iter_mut().for_each(|s| s.db = p.4);
        self.auto_pending = true;
    }

    pub fn set_cmap(&mut self, i: usize) {
        self.cmap = i % crate::colormaps::all().len();
        self.stops = crate::colormaps::stops(self.cmap);
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
            // The speed is the value of the layer (stretch, histogram, inspector). Then the two components.
            Kind::Wind => vec![format!("sqrt(({})^2 + ({})^2)", self.rgb[0], self.rgb[1]), self.rgb[0].clone(), self.rgb[1].clone()],
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

    /// Change the pick: the layers of all steps come again with it.
    pub fn set_pick(&mut self, p: eo_cache::Pick) {
        if p != self.pick {
            self.pick = p;
            self.pgen += 1;
        }
    }

    /// A channel layer of step `s` and of pick generation `pgen` is ready. Return true if the view must make
    /// its inputs again: the selected step is complete and shows now, or a next step is complete (prefetch).
    pub fn loaded(&mut self, s: usize, pgen: u64, l: Arc<Layer>) -> bool {
        if pgen != self.pgen {
            return false;
        }
        self.cache.insert((s, l.var, l.choice), (pgen, l));
        if s == self.step { (self.inputs.is_empty() || self.shown != s || self.shown_pgen != pgen) && self.ready() } else { self.inputs_at(s).is_some() }
    }

    fn ready(&mut self) -> bool {
        let Some(v) = self.inputs_at(self.step) else { return false };
        self.inputs = v;
        self.shown = self.step;
        self.shown_pgen = self.pgen;
        if self.auto_pending {
            self.auto_pending = false;
            if self.is_color() { self.as_is() } else { self.auto() }
            self.fresh = true;
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

    /// Stretch limits of each channel: the automatic range of the preferences (`StretchInit`).
    pub fn auto(&mut self) {
        let (init, _) = stretch_init();
        let n = if self.gray() { 1 } else { 3 };
        for k in 0..n.min(self.trees.len()) {
            let vals = self.values(k);
            if vals.is_empty() {
                (self.st[k].lo, self.st[k].hi) = (0.0, 1.0);
                continue;
            }
            let q = |p: f32| vals[((vals.len() - 1) as f32 * p) as usize];
            let c = self.clip / 100.0;
            // The range of the type of a band (not of band math, not in dB).
            let typed = match self.trees[k] {
                Node::Var(j) if !self.st[k].db => self.inputs.get(j).and_then(|l| type_range(l.var(), l.var().levels[0].dtype).filter(|_| l.part == eo_cache::Part::Real)),
                _ => None,
            };
            (self.st[k].lo, self.st[k].hi) = match (init, typed) {
                (StretchInit::Clip, _) => (q(c), q(1.0 - c)),
                (StretchInit::Type, Some(r)) => r,
                _ => (vals[0], vals[vals.len() - 1]),
            };
            if self.st[k].hi <= self.st[k].lo {
                self.st[k].hi = self.st[k].lo + 1.0;
            }
        }
        self.auto_st = true;
        self.histograms();
    }

    /// What the layer shows: two layers with the same key show the same bands (of other products).
    pub fn display_key(&self) -> String {
        match self.kind {
            Kind::Band => format!("band {}", self.chans.get(self.band).map_or("", |c| c.id.as_str())),
            Kind::Expr => format!("expr {}", self.expr),
            k => format!("{k:?} {:?}", self.rgb),
        }
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
    /// `inputs`: the view input of each used channel. `next`: the view inputs of the same channels at the
    /// next time step; the value of a channel is then the blend of the two steps.
    pub fn spec(&self, inputs: &[usize], next: Option<&[usize]>) -> Option<LayerSpec> {
        let var = |j: usize| match next {
            Some(n) => format!("mix(v{}, v{}, u.tmix)", inputs[j], n[j]),
            None => format!("v{}", inputs[j]),
        };
        let w: Vec<String> = self.trees.iter().map(|t| t.wgsl_with(&var)).collect();
        let mode = match self.kind {
            Kind::Wind => Mode::Wind { speed: w.first()?.clone(), u: w.get(1)?.clone(), v: w.get(2)?.clone() },
            Kind::Rgb => Mode::Rgb([w.first()?.clone(), w.get(1)?.clone(), w.get(2)?.clone()]),
            _ => Mode::Gray(w.first()?.clone()),
        };
        Some(LayerSpec { mode, inputs: inputs.iter().chain(next.into_iter().flatten()).copied().collect() })
    }

    pub fn params(&self) -> LayerParams {
        let mut p = LayerParams { opacity: self.opacity, flags: (self.invert as u32) << 3 | (!self.fill as u32) << 4 | (!self.arrows as u32) << 5, ..Default::default() };
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
                    Kind::Band | Kind::Wind => false,
                }
        });
        match (preset, self.kind) {
            (Some(p), _) => p.0.into(),
            (None, Kind::Band) => self.chans.get(self.band).map_or(String::new(), |c| c.id.clone()),
            (None, Kind::Rgb) => self.rgb.join(" "),
            (None, Kind::Expr) => self.expr.clone(),
            (None, Kind::Wind) => format!("Wind {} {}", self.rgb[0], self.rgb[1]),
        }
    }

    /// Short name for lists and titles: what the layer shows, then the product name, at most `n` characters.
    pub fn label(&self, n: usize) -> String {
        // A file with one band: the band has the name of the file, the name of the file is sufficient.
        let stem = std::path::Path::new(&self.name).file_stem().map_or(String::new(), |x| ident(&x.to_string_lossy()));
        let comp = self.comp_name();
        let s = if comp == ident(&self.name) || comp == stem { self.name.clone() } else { format!("{comp} - {}", self.name) };
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

fn one() -> f32 {
    1.0
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
    /// The terrain of the globe view, and its vertical exaggeration.
    #[serde(default)]
    pub terrain: bool,
    #[serde(default = "one")]
    pub exag: f32,
    /// A series of products: the path and the time of each step. Empty: one product.
    #[serde(default)]
    pub series: Vec<(String, Option<f64>)>,
    /// Selected time step.
    #[serde(default)]
    pub step: usize,
    /// Wind mode: the colors of the speed, the arrows, the particles.
    #[serde(default)]
    pub wind: Option<[bool; 3]>,
    /// A computed layer: its operation. The layer opens again from it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<OpSave>,
    /// The image axes and the index of the other dimensions, by the names of the file (DESIGN section 16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axes: Option<[String; 2]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub at: Vec<(String, u64)>,
}

/// The operation of a computed layer in a workspace file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpSave {
    /// An aggregate over time (`how`: the English name of `eo_cache::Agg`) of the steps `first` to `last`
    /// of the layer `source`.
    Agg { how: String, source: Box<LayerSave>, first: usize, last: usize },
    /// An aggregate along dimension `dim` (all its indexes) of the layer `source`.
    AggDim { how: String, source: Box<LayerSave>, dim: String },
    /// Layer math: the expression `expr` of the layers `inputs`, with the names `names` in the expression.
    Math { expr: String, names: Vec<String>, inputs: Vec<LayerSave> },
    /// A filter of the layers `inputs`, for example `{"op": "gaussian", "sigma": 2.0}`.
    Filter { filter: eo_cache::Filter, inputs: Vec<LayerSave> },
}

/// The bands of an input of an index: its name, then the names of the band in the products (Sentinel-2,
/// then the common names of STAC).
// ponytail: Sentinel-2 names only. Add the Landsat names (SR_B5 ...) when a user opens Landsat products.
type Band = (&'static str, &'static [&'static str]);
const NIR: Band = ("NIR", &["B08", "B8", "nir"]);
const RED: Band = ("red", &["B04", "B4", "red"]);
const GREEN: Band = ("green", &["B03", "B3", "green"]);
const BLUE: Band = ("blue", &["B02", "B2", "blue"]);
const SWIR1: Band = ("SWIR 1.6", &["B11", "swir16"]);
const SWIR2: Band = ("SWIR 2.2", &["B12", "swir22"]);

/// The filters of the Layer menu: group, name, filter with its first parameters, and for an index the
/// bands of its inputs (empty: the band of the layer).
pub const FILTERS: &[(&str, &str, eo_cache::Filter, &[Band])] = {
    use eo_cache::Filter as F;
    &[
        ("Radiometry", "Scale and offset", F::Scale { k: 1.0, o: 0.0 }, &[]),
        ("Radiometry", "Decibels", F::Db, &[]),
        ("Indices", "NDVI", F::NormDiff, &[NIR, RED]),
        ("Indices", "NDWI", F::NormDiff, &[GREEN, NIR]),
        ("Indices", "NDMI", F::NormDiff, &[NIR, SWIR1]),
        ("Indices", "NBR", F::NormDiff, &[NIR, SWIR2]),
        ("Indices", "NDSI", F::NormDiff, &[GREEN, SWIR1]),
        ("Indices", "EVI", F::Evi, &[NIR, RED, BLUE]),
        ("Indices", "SAVI", F::Savi { l: 0.5 }, &[NIR, RED]),
        ("Space", "Gaussian", F::Gaussian { sigma: 2.0 }, &[]),
        ("Space", "Boxcar mean", F::Boxcar { w: 3.0, h: 3.0 }, &[]),
        ("Space", "Median", F::Median { r: 1.0 }, &[]),
        ("Space", "Sobel edges", F::Sobel, &[]),
        ("Space", "Laplacian", F::Laplace, &[]),
        ("Space", "Erosion", F::Erode { r: 1.0 }, &[]),
        ("Space", "Dilation", F::Dilate { r: 1.0 }, &[]),
        ("Space", "Opening", F::Open { r: 1.0 }, &[]),
        ("Space", "Closing", F::Close { r: 1.0 }, &[]),
        ("SAR", "Lee speckle filter", F::Lee { r: 2.0, looks: 1.0 }, &[]),
        ("SAR", "Multilook (boxcar mean)", F::Boxcar { w: 5.0, h: 1.0 }, &[]),
        ("Terrain", "Hillshade", F::Hillshade { azimuth: 315.0, altitude: 45.0, z: 1.0 }, &[]),
        ("Terrain", "Slope", F::Slope { z: 1.0 }, &[]),
        ("Terrain", "Aspect", F::Aspect, &[]),
        ("Mask", "Threshold", F::Threshold { lo: 0.0, hi: 1.0 }, &[]),
    ]
};

impl MapLayer {
    /// The channel of each band `bands` of an index (`FILTERS`), by the name of the band. Err: the name of
    /// the first band that the product does not have.
    pub fn index_bands(&self, bands: &[Band]) -> Result<Vec<usize>, String> {
        bands
            .iter()
            .map(|(name, ids)| {
                let found = ids.iter().find_map(|n| (0..self.chans.len()).find(|&c| self.chans[c].id.eq_ignore_ascii_case(n) || self.chan_leaf(c).eq_ignore_ascii_case(n)));
                found.ok_or_else(|| format!("{name} ({})", ids.join(", ")))
            })
            .collect()
    }
}


impl MapLayer {
    /// True if a workspace file can keep the layer: a layer of a file, or a computed layer with its
    /// operation. The outputs of a Python script are not in the file: the script makes them again.
    pub fn saveable(&self) -> bool {
        self.op.is_some() || self.any().is_none_or(|x| x.op.is_none())
    }

    pub fn save(&self) -> LayerSave {
        LayerSave {
            path: eo_core::redact_url(&self.path),
            kind: self.kind,
            band: self.chans.get(self.band).map_or(String::new(), |c| c.id.clone()),
            rgb: self.rgb.clone(),
            expr: self.expr.clone(),
            st: self.st,
            clip: self.clip,
            cmap: crate::colormaps::name(self.cmap),
            stops: self.stops.clone(),
            invert: self.invert,
            opacity: self.opacity,
            visible: self.visible,
            terrain: self.terrain,
            exag: self.exag,
            series: self.steps.iter().filter(|s| !s.path.is_empty()).map(|s| (eo_core::redact_url(&s.path), s.t.is_finite().then_some(s.t))).collect(),
            step: self.step,
            wind: Some([self.fill, self.arrows, self.particles]),
            op: self.op.clone(),
            axes: self.pick.axes.clone(),
            at: self.pick.at.clone(),
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
        self.set_cmap(crate::colormaps::find(&s.cmap).unwrap_or(0));
        if s.stops.len() >= 2 {
            self.stops = s.stops.clone();
        }
        self.invert = s.invert;
        self.opacity = s.opacity;
        self.visible = s.visible;
        (self.terrain, self.exag) = (s.terrain, s.exag);
        if !s.series.is_empty() {
            self.set_series(s.series.iter().map(|(p, t)| (p.clone(), t.unwrap_or(f64::NAN))).collect());
        }
        self.step = s.step.min(self.steps.len().saturating_sub(1));
        if let Some([f, a, p]) = s.wind {
            (self.fill, self.arrows, self.particles) = (f, a, p);
        }
        if s.axes.is_some() || !s.at.is_empty() {
            self.set_pick(eo_cache::Pick { axes: s.axes.clone(), at: s.at.clone() });
        }
        self.auto_pending = false;
    }
}
