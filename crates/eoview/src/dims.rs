//! The dimensions of a variable (DESIGN section 16): the two image axes and the index of the other
//! dimensions. The same controls are in the open dialog and in the layer panel. eoview does not guess what
//! the user wants to see: a product with more than one variable, or with more than two dimensions, opens
//! the dialog. The readers give the dimension names and a proposal of axes (`Variable::dims`).
use crate::lang::{t, tf};
use eo_cache::{Layer, Pick};
use eo_core::{Product, Variable};
use std::sync::Arc;

/// True if the user selects the variable and its dimensions before the layer opens.
pub fn ask(p: &Product) -> bool {
    p.vars.iter().filter(|v| !v.dims.is_empty()).count() > 1 || p.vars.iter().any(|v| v.dims.len() > 2)
}

/// "dim_camera 4 × dim_ALT 11 × ...", or "width × height" for a format without dimension names.
pub fn shape_text(v: &Variable) -> String {
    let a = &v.levels[0];
    if v.dims.len() != a.shape.len() {
        let (w, h) = v.size();
        return format!("{w} × {h}");
    }
    v.dims.iter().zip(&a.shape).map(|(d, n)| format!("{d} {n}")).collect::<Vec<_>>().join(" × ")
}

/// Positions of the image axes of `v` in `pick`, else the axes of the reader.
fn axes(v: &Variable, pick: &Pick) -> Option<[usize; 2]> {
    let pos = |n: &String| v.dims.iter().position(|d| d == n);
    pick.axes.as_ref().and_then(|[r, c]| Some([pos(r)?, pos(c)?])).filter(|a| a[0] != a[1]).or_else(|| v.axes())
}

/// Controls for variable `v` (as the reader gives it): the dimension of the rows, of the columns, and a
/// slider for the index of each other dimension, with the value of its coordinate (`coord`: dimension name
/// and index to text). Time has the timeline. Return true if `pick` changed.
pub fn dims_ui(ui: &mut egui::Ui, v: &Variable, pick: &mut Pick, coord: &dyn Fn(&str, u64) -> Option<String>) -> bool {
    let (n, kind) = (v.dims.len(), &v.levels[0].dims);
    let Some([r0, c0]) = axes(v, pick).filter(|_| n >= 2 && kind.len() == n) else { return false };
    let (mut r, mut c) = (r0, c0);
    let label = |k: usize| format!("{} ({})", v.dims[k], v.levels[0].shape[k]);
    egui::Grid::new(("dims", &v.name)).num_columns(2).show(ui, |ui| {
        for (name, a) in [(t("Rows"), &mut r), (t("Columns"), &mut c)] {
            ui.label(name);
            egui::ComboBox::from_id_salt((name, &v.name)).selected_text(label(*a)).show_ui(ui, |ui| {
                for k in 0..n {
                    ui.selectable_value(a, k, label(k));
                }
            });
            ui.end_row();
        }
    });
    // The same dimension for the rows and the columns: the other axis takes the old dimension.
    if r == c {
        (r, c) = if r != r0 { (r, r0) } else { (c0, c) };
    }
    let mut changed = (r, c) != (r0, c0);
    let mut at: Vec<(String, u64)> = pick.at.iter().filter(|p| !v.dims.contains(&p.0)).cloned().collect();
    egui::Grid::new(("index", &v.name)).num_columns(2).show(ui, |ui| {
        for k in (0..n).filter(|&k| k != r && k != c && kind[k] != "time" && kind[k] != "band") {
            let (name, size) = (&v.dims[k], v.levels[0].shape[k]);
            let mut i = pick.at.iter().find(|p| p.0 == *name).map_or(0, |p| p.1).min(size - 1);
            ui.label(name);
            ui.horizontal(|ui| {
                changed |= ui.add_enabled(size > 1, egui::Slider::new(&mut i, 0..=size - 1).suffix(format!(" / {}", size - 1))).changed();
                if let Some(c) = coord(name, i) {
                    ui.strong(c);
                }
            });
            ui.end_row();
            at.push((name.clone(), i));
        }
    });
    if changed {
        *pick = Pick { axes: Some([v.dims[r].clone(), v.dims[c].clone()]), at };
    }
    changed
}

/// The pick with the index of the first dimension that is not an image axis or time moved by `d` (Page Up
/// and Page Down). None: no such dimension, or the index is at its end.
pub fn step(v: &Variable, pick: &Pick, d: i32) -> Option<Pick> {
    let [r, c] = axes(v, pick)?;
    let kind = &v.levels[0].dims;
    let k = (0..v.dims.len()).find(|&k| k != r && k != c && kind.get(k).is_some_and(|x| x != "time" && x != "band") && v.levels[0].shape[k] > 1)?;
    let (name, size) = (&v.dims[k], v.levels[0].shape[k] as i64);
    let i = pick.at.iter().find(|p| p.0 == *name).map_or(0, |p| p.1 as i64);
    let j = (i + d as i64).clamp(0, size - 1);
    if j == i {
        return None;
    }
    let mut at: Vec<(String, u64)> = pick.at.iter().filter(|p| p.0 != *name).cloned().collect();
    at.push((name.clone(), j as u64));
    Some(Pick { axes: Some([v.dims[r].clone(), v.dims[c].clone()]), at })
}

/// A product that waits for the choice of the user: its first layer, the selected variable and its pick.
pub struct Asking {
    pub l: Arc<Layer>,
    pub var: usize,
    pub pick: Pick,
    pub filter: String,
}

impl Asking {
    /// `last`: the last choice of the user. It stays if the variable has its dimension names.
    pub fn new(l: Arc<Layer>, last: &Pick) -> Asking {
        // The first variable with the most dimensions: in most products, the data and not a coordinate.
        let p = &l.ds.product;
        let var = (0..p.vars.len()).rev().max_by_key(|&k| p.vars[k].dims.len()).unwrap_or(0);
        let v = &p.vars[var];
        let fits = last.axes.as_ref().is_some_and(|a| a.iter().all(|n| v.dims.contains(n)));
        let pick = if fits { last.clone() } else { propose(v) };
        Asking { l, var, pick, filter: String::new() }
    }
}

/// Image axes from known dimension names (latitude and longitude, y and x, rows and columns, along and
/// across track). None: the axes of the reader (the last two dimensions).
pub fn propose(v: &Variable) -> Pick {
    const ROWS: &[&str] = &["lat", "latitude", "y", "row", "rows", "line", "lines", "scanline", "along_track", "alt", "nrows"];
    const COLS: &[&str] = &["lon", "longitude", "x", "col", "cols", "column", "columns", "pixel", "pixels", "ground_pixel", "across_track", "act", "ncols"];
    let find = |names: &[&str]| v.dims.iter().position(|d| {
        let d = d.to_lowercase();
        names.contains(&d.as_str()) || d.strip_prefix("dim_").is_some_and(|s| names.contains(&s))
    });
    let axes = match (find(ROWS), find(COLS)) {
        (Some(r), Some(c)) if r != c => Some([v.dims[r].clone(), v.dims[c].clone()]),
        _ => None,
    };
    Pick { axes, at: vec![] }
}

/// The open dialog. Some(true): open the variable, Some(false): cancel, None: the dialog stays.
pub fn ask_ui(ctx: &egui::Context, a: &mut Asking, smalls: &Smalls) -> Option<bool> {
    let l = a.l.clone();
    let p = &l.ds.product;
    let mut out = None;
    let r = egui::Modal::new(egui::Id::new("open-dims")).show(ctx, |ui| {
        ui.set_width(760.0);
        ui.strong(tf("Open {}", &[&p.name]));
        ui.label(t("Select the variable, the dimensions of the image, and the index of the other dimensions."));
        ui.add(egui::TextEdit::singleline(&mut a.filter).hint_text(t("Filter")).desired_width(f32::INFINITY));
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical().id_salt("ask-vars").max_height(380.0).auto_shrink([false, true]).show(&mut cols[0], |ui| {
                for (k, v) in p.vars.iter().enumerate() {
                    if !a.filter.is_empty() && crate::ui::fuzzy(&a.filter, &v.name).is_none() {
                        continue;
                    }
                    let r = ui.selectable_label(a.var == k, &v.name).on_hover_text(shape_text(v));
                    if r.clicked() {
                        a.var = k;
                    }
                    if r.double_clicked() {
                        out = Some(true);
                    }
                }
            });
            let ui = &mut cols[1];
            let v = &p.vars[a.var];
            ui.strong(&v.name);
            ui.small(shape_text(v));
            ui.add_space(4.0);
            let l = a.l.clone();
            dims_ui(ui, v, &mut a.pick, &|d, i| smalls.coord(&l, &v.name, d, i));
        });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(t("Open")).clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                out = Some(true);
            }
            if ui.button(t("Cancel")).clicked() {
                out = Some(false);
            }
        });
    });
    if out.is_none() && r.should_close() {
        out = Some(false);
    }
    out
}

/// The values of the small variables (scalars and 1D, `Product::small`), read in a thread when the interface
/// needs them first: by (dataset id, index in `Product::small`). None: the read is in progress.
pub type SmallValues = Option<Result<Arc<Vec<f64>>, String>>;

#[derive(Clone)]
pub struct Smalls {
    map: Arc<std::sync::Mutex<std::collections::HashMap<(u64, usize), SmallValues>>>,
    ctx: egui::Context,
}

impl Smalls {
    pub fn new(ctx: egui::Context) -> Smalls {
        Smalls { map: Default::default(), ctx }
    }

    /// The values of small variable `k` of the product of `l`. The first call starts the read.
    pub fn get(&self, l: &Arc<Layer>, k: usize) -> SmallValues {
        let key = (l.ds_id, k);
        let mut m = self.map.lock().unwrap();
        if let Some(v) = m.get(&key) {
            return v.clone();
        }
        m.insert(key, None);
        let (map, ctx, ds) = (self.map.clone(), self.ctx.clone(), l.ds.clone());
        std::thread::spawn(move || {
            let r = ds.product.small.get(k).ok_or_else(|| "no such variable".to_string()).and_then(|v| eo_io::read_all(&ds, v).map_err(|e| e.to_string()));
            map.lock().unwrap().insert(key, Some(r.map(Arc::new)));
            ctx.request_repaint();
        });
        None
    }

    /// The value of index `i` of the coordinate variable of dimension `dim` (for variable `var` of the
    /// product of `l`), with its unit. None: no coordinate variable, or its values are not here yet.
    pub fn coord(&self, l: &Arc<Layer>, var: &str, dim: &str, i: u64) -> Option<String> {
        let k = coord_var(&l.ds.product, var, dim)?;
        let v = self.get(l, k)?.ok()?;
        let x = *v.get(i as usize)?;
        let u = &l.ds.product.small[k].units;
        Some(if u.is_empty() { num(x) } else { format!("{} {u}", num(x)) })
    }
}

/// A short text of a value: 6 significant digits.
pub fn num(x: f64) -> String {
    if x.is_nan() {
        return "-".into();
    }
    let d = (5 - x.abs().max(1e-300).log10().floor() as i32).clamp(0, 12) as usize;
    let s = format!("{x:.d$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s.len() > 12 { format!("{x:.5e}") } else { s }
}

#[cfg(test)]
mod tests {
    #[test]
    fn short_numbers() {
        let n = super::num;
        assert_eq!([n(676.063843), n(1000.0), n(0.000123456789), n(400.0), n(-2.5), n(f64::NAN)], ["676.064", "1000", "0.000123457", "400", "-2.5", "-"]);
    }
}

/// The coordinate variable of dimension `dim`: a 1D small variable with this dimension and this name, in
/// the group of `var` or the nearest parent group.
pub fn coord_var(p: &Product, var: &str, dim: &str) -> Option<usize> {
    let dir = |n: &str| n.rsplit_once('/').map_or(String::new(), |d| d.0.to_string());
    let leaf = |n: &str| n.rsplit('/').next().unwrap_or(n).to_string();
    let vd = dir(var);
    (0..p.small.len())
        .filter(|&k| p.small[k].dims.len() == 1 && p.small[k].dims[0] == dim && leaf(&p.small[k].name) == dim && vd.starts_with(&dir(&p.small[k].name)))
        .max_by_key(|&k| dir(&p.small[k].name).len())
}

/// The profile along a dimension at the cursor (the spectrum of a pixel): the request and its values.
#[derive(Default)]
pub struct Profile {
    /// The name of the dimension that the user selected. None: no profile.
    pub dim: Option<String>,
    /// (layer id, x, y, dimension position) of the values, or of the request in progress.
    pub key: Option<(u64, u64, u64, usize)>,
    pub req: Option<u64>,
    pub values: Option<Result<Vec<f64>, String>>,
}
