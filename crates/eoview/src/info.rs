//! The information window: the metadata of the product of the selected layer (global attributes,
//! dimensions, all the variables with their dimensions and attributes). The values window: the values of the
//! pixels around the cursor in a table, as they are in the file (level 0, with the scale and the offset).
use crate::app::App;
use crate::lang::{t, tf};
use eo_cache::Layer;
use eo_core::{Attrs, Meta, Product, Variable};
use std::sync::Arc;

/// The size of a cell of the table of values, and of its headers, in points.
const CW: f32 = 96.0;
const RH: f32 = 18.0;
const HW: f32 = 120.0;
const HH: f32 = 20.0;

/// The state of the values window.
pub struct Values {
    pub open: bool,
    /// The table scrolls to the cell under the cursor of the view.
    pub follow: bool,
    /// The input of the selected layer (RGB: 0 red, 1 green, 2 blue).
    pub input: usize,
    /// The input (layer id) and its level-0 pixel under the cursor (the highlighted cell).
    pub center: Option<(u64, u64, u64)>,
    /// The request that comes, and its input and window.
    pub req: Option<(u64, u64, (u64, u64, u64, u64))>,
    /// The values that are here: input, window (x0, y0, w, h), values (NaN: no data).
    pub data: Option<(u64, (u64, u64, u64, u64), Arc<Vec<f32>>)>,
    /// A scalar or a 1D variable of the product (index in `Product::small`) in place of the plane.
    pub small: Option<usize>,
    /// The headers show the indexes with the coordinates.
    pub indexes: bool,
    /// The highlighted cell of the last frame: the table scrolls when it changes.
    shown: Option<(u64, u64)>,
}

impl Default for Values {
    fn default() -> Values {
        Values { open: false, follow: true, input: 0, center: None, req: None, data: None, small: None, indexes: true, shown: None }
    }
}

/// A value for the table. Integers stored with a scale (0.01): the decimals of the scale. Else the shortest
/// text that reads back as the same 32-bit float.
fn num(v: f32, dec: Option<usize>) -> String {
    if v.is_nan() {
        return "-".into();
    }
    let s = match dec {
        Some(d) => format!("{v:.d$}"),
        None => v.to_string(),
    };
    if s.len() <= 12 { s } else { format!("{v:.4e}") }
}

/// Dimensions as "name = size, name = size".
fn dims_text(d: &[(String, u64)]) -> String {
    d.iter().map(|(n, s)| format!("{n} = {s}")).collect::<Vec<_>>().join(", ")
}

/// The description of a variable of the viewer without metadata from the reader: from its array.
fn meta_of(v: &Variable) -> Meta {
    let a = &v.levels[0];
    Meta { name: v.name.clone(), dims: a.dims.iter().cloned().zip(a.shape.iter().copied()).collect(), dtype: format!("{:?}", a.dtype).to_lowercase(), attrs: vec![] }
}

/// What the viewer knows of a variable that it shows: storage and the values that it applies.
fn storage(v: &Variable) -> Attrs {
    let a = &v.levels[0];
    let mut s: Attrs = vec![
        (t("Chunks").into(), a.chunk.iter().map(u64::to_string).collect::<Vec<_>>().join(" x ")),
        (t("Compression").into(), if a.codecs.is_empty() { t("none").into() } else { a.codecs.iter().map(|c| format!("{c:?}")).collect::<Vec<_>>().join(", ") }),
        (t("Resolution levels").into(), v.levels.iter().map(|l| format!("{} x {}", l.len_of("x"), l.len_of("y"))).collect::<Vec<_>>().join(", ")),
    ];
    if v.scale != 1.0 || v.offset != 0.0 {
        s.push((t("Scale and offset").into(), format!("{} and {}", v.scale, v.offset)));
    }
    if let Some(f) = v.fill {
        s.push((t("No data value").into(), f.to_string()));
    }
    if !v.units.is_empty() {
        s.push((t("Units").into(), v.units.clone()));
    }
    s
}

fn attrs_grid(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, a: &Attrs, filter: &str) {
    egui::Grid::new(id).num_columns(2).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
        for (k, v) in a.iter().filter(|(k, v)| filter.is_empty() || k.to_lowercase().contains(filter) || v.to_lowercase().contains(filter)) {
            ui.add(egui::Label::new(egui::RichText::new(k).strong()).selectable(true));
            ui.add(egui::Label::new(v).selectable(true).wrap());
            ui.end_row();
        }
    });
}

/// The metadata of a product as text, as `ncdump -h` shows it (for the clipboard).
fn dump(p: &Product, metas: &[Meta]) -> String {
    let mut s = format!("{}\n{}\n\ndimensions:\n", p.name, p.desc);
    for (n, z) in &p.info.dims {
        s += &format!("    {n} = {z}\n");
    }
    s += "\nvariables:\n";
    for m in metas {
        s += &format!("    {} {}({})\n", m.dtype, m.name, m.dims.iter().map(|d| d.0.as_str()).collect::<Vec<_>>().join(", "));
        for (k, v) in &m.attrs {
            s += &format!("        {}:{k} = {v}\n", m.name);
        }
    }
    s += "\nglobal attributes:\n";
    for (k, v) in &p.info.attrs {
        s += &format!("    :{k} = {v}\n");
    }
    s
}

impl App {
    /// The input of the selected layer of the active view, for the windows (`input`: RGB channel).
    fn info_input(&self, input: usize) -> Option<Arc<Layer>> {
        let p = self.pane(self.active)?;
        let l = p.layers.get(p.sel)?;
        l.inputs.get(input.min(l.inputs.len().saturating_sub(1))).cloned()
    }

    pub fn info_ui(&mut self, ctx: &egui::Context) {
        if !self.info_open {
            return;
        }
        let mut open = true;
        let inp = self.info_input(0);
        let filter = self.info_filter.trim().to_lowercase();
        let mut text = std::mem::take(&mut self.info_filter);
        egui::Window::new(t("Information")).open(&mut open).default_size([560.0, 620.0]).show(ctx, |ui| {
            let Some(inp) = inp else {
                ui.label(t("Open a product, then select one of its layers."));
                return;
            };
            let p = &inp.ds.product;
            let shown = &inp.var().name;
            // All the variables of the file: from the reader, else the variables that the viewer shows.
            let metas: Vec<Meta> = if p.info.vars.is_empty() { p.vars.iter().map(meta_of).collect() } else { p.info.vars.clone() };
            ui.heading(&p.name);
            ui.label(&p.desc);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut text).hint_text(t("Search the names and the values")).desired_width(280.0));
                if ui.button(t("Copy as text")).clicked() {
                    ui.ctx().copy_text(dump(p, &metas));
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if !p.info.dims.is_empty() {
                    egui::CollapsingHeader::new(tf("Dimensions ({})", &[&p.info.dims.len().to_string()])).default_open(true).show(ui, |ui| {
                        egui::Grid::new("dims").num_columns(2).striped(true).show(ui, |ui| {
                            for (n, z) in &p.info.dims {
                                ui.add(egui::Label::new(egui::RichText::new(n).strong()).selectable(true));
                                ui.label(z.to_string());
                                ui.end_row();
                            }
                        });
                    });
                }
                if !p.info.attrs.is_empty() {
                    egui::CollapsingHeader::new(tf("Global attributes ({})", &[&p.info.attrs.len().to_string()])).default_open(p.info.attrs.len() <= 40).show(ui, |ui| attrs_grid(ui, "global", &p.info.attrs, &filter));
                }
                egui::CollapsingHeader::new(tf("Variables ({})", &[&metas.len().to_string()])).default_open(true).show(ui, |ui| {
                    for m in &metas {
                        let viewer = p.vars.iter().find(|v| v.name == m.name);
                        let hit = filter.is_empty() || m.name.to_lowercase().contains(&filter) || m.attrs.iter().any(|(k, v)| k.to_lowercase().contains(&filter) || v.to_lowercase().contains(&filter));
                        if !hit {
                            continue;
                        }
                        let head = format!("{}  ({})  {}", m.name, dims_text(&m.dims), m.dtype);
                        egui::CollapsingHeader::new(head).id_salt(("var", &m.name)).default_open(&m.name == shown || !filter.is_empty()).show(ui, |ui| {
                            if let Some(v) = viewer {
                                attrs_grid(ui, ("store", &m.name), &storage(v), "");
                            }
                            if m.attrs.is_empty() {
                                ui.weak(t("No attributes."));
                            } else {
                                attrs_grid(ui, ("attrs", &m.name), &m.attrs, if m.name.to_lowercase().contains(&filter) { "" } else { &filter });
                            }
                        });
                    }
                });
            });
        });
        self.info_filter = text;
        self.info_open = open;
    }

    /// An answer of the engine for the values window.
    pub fn values_read(&mut self, res: eo_core::Result<(Arc<Vec<f32>>, eo_core::Georef)>) {
        let Some((_, id, win)) = self.values.req.take() else { return };
        match res {
            Ok((v, _)) => self.values.data = Some((id, win, v)),
            Err(e) => self.error = Some(e.0),
        }
    }

    pub fn values_ui(&mut self, ctx: &egui::Context) {
        if !self.values.open {
            return;
        }
        let inp = self.info_input(self.values.input);
        // The highlighted cell follows the cursor of the active view.
        if let Some(inp) = &inp
            && self.values.follow
            && self.hovered == Some(self.active)
            && let Some(p) = self.pane(self.active)
            && let Some(c) = p.v.cursor
            && let Some((w, _)) = self.warps.get(&(inp.id, p.v.space))
            && let Some((x, y)) = w.inverse(c[0], c[1])
            && x >= 0.0
            && y >= 0.0
            && (x as u64) < inp.size().0
            && (y as u64) < inp.size().1
        {
            self.values.center = Some((inp.id, x as u64, y as u64));
        }
        let mut open = true;
        let mut values = std::mem::take(&mut self.values);
        let smalls = self.smalls.clone();
        let n_inputs = self.pane(self.active).and_then(|p| p.layers.get(p.sel)).map_or(0, |l| l.inputs.len());
        let names: Vec<String> = self.pane(self.active).and_then(|p| p.layers.get(p.sel)).map_or(vec![], |l| (0..n_inputs).map(|j| l.used.get(j).map_or(String::new(), |&c| l.chans[c].id.clone())).collect());
        let mut want = None;
        egui::Window::new(t("Values")).open(&mut open).default_size([780.0, 470.0]).show(ctx, |ui| {
            let Some(inp) = &inp else {
                ui.label(t("Open a product, then select one of its layers."));
                return;
            };
            if let Some(k) = values.small.filter(|&k| k < inp.ds.product.small.len()) {
                small_table(ui, &smalls, inp, k, &mut values.small);
                return;
            }
            let v = inp.var();
            let (lw, lh) = inp.size();
            let axes = v.axes().filter(|_| v.dims.len() == v.levels[0].dims.len());
            let dim = |k: usize| axes.map(|a| v.dims[a[k]].clone());
            ui.horizontal(|ui| {
                if n_inputs > 1 {
                    for (j, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut values.input, j, n);
                    }
                    ui.separator();
                }
                ui.checkbox(&mut values.follow, t("Follow the cursor"));
                ui.checkbox(&mut values.indexes, t("Indexes"));
                ui.weak(if v.units.is_empty() { v.name.clone() } else { format!("{} ({})", v.name, v.units) });
            });
            ui.horizontal(|ui| {
                ui.label(tf("Rows: {} ({}), columns: {} ({})", &[&dim(0).unwrap_or_else(|| "y".into()), &lh.to_string(), &dim(1).unwrap_or_else(|| "x".into()), &lw.to_string()]));
                if let Some((_, (x0, y0, w, h), vals)) = values.data.as_ref().filter(|d| d.0 == inp.id)
                    && ui.button(t("Copy as CSV")).on_hover_text(t("The values that the table has read: the visible cells and a margin")).clicked()
                {
                    let mut s = String::from("row/column");
                    for x in *x0..x0 + w {
                        s += &format!(",{}", head(&smalls, inp, &dim(1), 1, x, false));
                    }
                    for r in 0..*h {
                        s += &format!("\n{}", head(&smalls, inp, &dim(0), 0, y0 + r, false));
                        for c in 0..*w {
                            let v = vals[(r * w + c) as usize];
                            s += &if v.is_nan() { ",".to_string() } else { format!(",{v}") };
                        }
                    }
                    ui.ctx().copy_text(s);
                }
            });
            ui.separator();
            let center = values.center.filter(|c| c.0 == inp.id).map(|c| (c.1, c.2));
            let to = center.filter(|_| values.follow && center != values.shown);
            values.shown = center;
            let var = inp.var();
            let dec = (var.scale != 1.0 && var.scale != 0.0 && !matches!(var.levels[0].dtype, eo_core::DType::F32 | eo_core::DType::F64 | eo_core::DType::CF32 | eo_core::DType::CF64)).then(|| (-var.scale.abs().log10() - 1e-4).ceil().max(0.0) as usize);
            let mut sa = egui::ScrollArea::both().auto_shrink([false, false]);
            if let Some((x, y)) = to {
                let a = ui.available_size();
                sa = sa.scroll_offset(egui::vec2((HW + x as f32 * CW - a.x / 2.0).max(0.0), (HH + y as f32 * RH - a.y / 2.0).max(0.0)));
            }
            sa.show_viewport(ui, |ui, vp| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(HW + lw as f32 * CW, HH + lh as f32 * RH), egui::Sense::hover());
                let o = rect.min;
                let span = |a: f32, b: f32, h: f32, s: f32, n: u64| (((a - h) / s).floor().max(0.0) as u64).min(n)..(((b - h) / s).ceil().max(0.0) as u64).min(n);
                let (cols, rows) = (span(vp.min.x, vp.max.x, HW, CW, lw), span(vp.min.y, vp.max.y, HH, RH, lh));
                // Read the visible cells and a margin.
                let (x0, y0) = (cols.start.saturating_sub(4), rows.start.saturating_sub(16));
                let (x1, y1) = ((cols.end + 4).min(lw), (rows.end + 16).min(lh));
                want = Some((x0, y0, x1 - x0, y1 - y0));
                let screen = vp.translate(o.to_vec2());
                let pt = ui.painter_at(screen);
                let vis = ui.visuals();
                let font = egui::FontId::monospace(12.0);
                let data = values.data.as_ref().filter(|d| d.0 == inp.id);
                for r in rows.clone() {
                    let y = o.y + HH + r as f32 * RH;
                    if r % 2 == 1 {
                        pt.rect_filled(egui::Rect::from_min_size(egui::pos2(screen.min.x, y), egui::vec2(screen.width(), RH)), 0.0, vis.faint_bg_color);
                    }
                    for c in cols.clone() {
                        let val = data.and_then(|(_, (dx, dy, dw, dh), v)| (c >= *dx && c < dx + dw && r >= *dy && r < dy + dh).then(|| v[((r - dy) * dw + (c - dx)) as usize]));
                        let hit = center == Some((c, r));
                        let color = if hit { vis.selection.stroke.color } else { vis.text_color() };
                        let text = val.map_or(String::new(), |x| num(x, dec));
                        pt.text(egui::pos2(o.x + HW + (c + 1) as f32 * CW - 6.0, y + RH / 2.0), egui::Align2::RIGHT_CENTER, text, font.clone(), color);
                    }
                }
                // Headers that stay at the top and at the left.
                let bg = vis.window_fill;
                pt.rect_filled(egui::Rect::from_min_size(screen.min, egui::vec2(screen.width(), HH)), 0.0, bg);
                pt.rect_filled(egui::Rect::from_min_size(screen.min, egui::vec2(HW, screen.height())), 0.0, bg);
                let weak = vis.weak_text_color();
                for c in cols {
                    let x = o.x + HW + c as f32 * CW + CW / 2.0;
                    pt.text(egui::pos2(x, screen.min.y + HH / 2.0), egui::Align2::CENTER_CENTER, head(&smalls, inp, &dim(1), 1, c, values.indexes), font.clone(), weak);
                }
                for r in rows {
                    let y = o.y + HH + r as f32 * RH + RH / 2.0;
                    pt.text(egui::pos2(screen.min.x + HW - 8.0, y), egui::Align2::RIGHT_CENTER, head(&smalls, inp, &dim(0), 0, r, values.indexes), font.clone(), weak);
                }
                pt.rect_filled(egui::Rect::from_min_size(screen.min, egui::vec2(HW, HH)), 0.0, bg);
            });
        });
        // Ask for the window of the table if the values are not here.
        if let (Some(inp), Some(win)) = (&inp, want) {
            let here = values.data.as_ref().is_some_and(|d| d.0 == inp.id && covers(d.1, win));
            let asked = values.req.as_ref().is_some_and(|r| r.1 == inp.id && covers(r.2, win));
            if !here && !asked {
                let req = self.engine.read(inp.clone(), 0, win);
                values.req = Some((req, inp.id, win));
            }
        }
        values.open = open;
        self.values = values;
    }
}

/// True if window `a` (x0, y0, w, h) contains window `b`.
fn covers(a: (u64, u64, u64, u64), b: (u64, u64, u64, u64)) -> bool {
    b.0 >= a.0 && b.1 >= a.1 && b.0 + b.2 <= a.0 + a.2 && b.1 + b.3 <= a.1 + a.3
}

/// The header of row or column `i` (`axis` 0: rows, 1: columns) of dimension `dim`: the value of its
/// coordinate variable, else the coordinate of a regular grid (georeferencing), else the index.
fn head(smalls: &crate::dims::Smalls, inp: &Arc<Layer>, dim: &Option<String>, axis: usize, i: u64, index: bool) -> String {
    let v = inp.var();
    let grid = match &v.georef {
        eo_core::Georef::Affine { gt, .. } if gt[2] == 0.0 && gt[4] == 0.0 => Some(if axis == 0 { gt[3] + (i as f64 + 0.5) * gt[5] } else { gt[0] + (i as f64 + 0.5) * gt[1] }),
        _ => None,
    };
    let c = dim.as_ref().and_then(|d| smalls.coord(inp, &v.name, d, i)).or_else(|| grid.map(crate::dims::num));
    match (c, index) {
        (Some(c), true) => format!("{i}: {c}"),
        (Some(c), false) => c,
        (None, _) => i.to_string(),
    }
}

/// The values of a scalar or of a 1D variable, with its coordinate if it has one.
fn small_table(ui: &mut egui::Ui, smalls: &crate::dims::Smalls, inp: &Arc<Layer>, k: usize, small: &mut Option<usize>) {
    let v = &inp.ds.product.small[k];
    ui.horizontal(|ui| {
        if ui.button(t("Back to the layer")).clicked() {
            *small = None;
        }
        ui.strong(&v.name);
        if !v.units.is_empty() {
            ui.weak(&v.units);
        }
        ui.weak(crate::dims::shape_text(v));
    });
    ui.separator();
    let vals = match smalls.get(inp, k) {
        None => return drop(ui.label("...")),
        Some(Err(e)) => return drop(ui.colored_label(ui.visuals().error_fg_color, e)),
        Some(Ok(x)) => x,
    };
    if v.levels[0].shape.is_empty() {
        ui.heading(crate::dims::num(vals.first().copied().unwrap_or(f64::NAN)));
        return;
    }
    let dim = v.dims.first().cloned();
    let coord = |i: u64| dim.as_ref().filter(|d| !v.name.ends_with(d.as_str())).and_then(|d| smalls.coord(inp, &v.name, d, i));
    // ponytail: one point in `step` for a long array; a min-max envelope if the peaks matter.
    let step = vals.len().div_ceil(4000).max(1);
    let pts: Vec<(f64, Vec<f64>)> = vals.iter().enumerate().step_by(step).map(|(i, x)| (i as f64, vec![*x])).collect();
    crate::tools::line_chart(ui, &pts, &[v.units.clone()], 0.0, &|d| {
        let i = d.round().max(0.0) as u64;
        coord(i).map_or(i.to_string(), |c| format!("{c} ({i})"))
    });
    if ui.button(t("Copy as CSV")).clicked() {
        let mut s = String::from("index,value");
        for (i, x) in vals.iter().enumerate() {
            s += &format!("\n{}{},{}", i, coord(i as u64).map_or(String::new(), |c| format!(" ({c})")), if x.is_nan() { String::new() } else { x.to_string() });
        }
        ui.ctx().copy_text(s);
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, RH, vals.len(), |ui, range| {
        egui::Grid::new(("small", k)).striped(true).min_col_width(80.0).start_row(range.start).show(ui, |ui| {
            for i in range {
                ui.weak(coord(i as u64).map_or(i.to_string(), |c| format!("{i}: {c}")));
                ui.monospace(crate::dims::num(vals[i]));
                ui.end_row();
            }
        });
    });
}
