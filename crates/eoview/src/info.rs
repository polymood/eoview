//! The information window: the metadata of the product of the selected layer (global attributes,
//! dimensions, all the variables with their dimensions and attributes). The values window: the values of the
//! pixels around the cursor in a table, as they are in the file (level 0, with the scale and the offset).
use crate::app::App;
use crate::lang::{t, tf};
use eo_cache::Layer;
use eo_core::{Attrs, Meta, Product, Variable};
use std::sync::Arc;

/// Columns and rows of the table of values.
const W: u64 = 9;
const H: u64 = 15;

/// The state of the values window.
pub struct Values {
    pub open: bool,
    /// The center of the table follows the cursor in the view.
    pub follow: bool,
    /// The input of the selected layer (RGB: 0 red, 1 green, 2 blue).
    pub input: usize,
    /// The input (layer id) and its level-0 pixel at the center of the table.
    pub center: Option<(u64, u64, u64)>,
    /// The request that comes, and its input and window.
    pub req: Option<(u64, u64, (u64, u64, u64, u64))>,
    /// The values that are here: input, window (x0, y0, w, h), values (NaN: no data).
    pub data: Option<(u64, (u64, u64, u64, u64), Arc<Vec<f32>>)>,
}

impl Default for Values {
    fn default() -> Values {
        Values { open: false, follow: true, input: 0, center: None, req: None, data: None }
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
        // The center follows the cursor of the active view.
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
        // The window of the table around the center, in the layer.
        let win = inp.as_ref().map(|inp| {
            let (lw, lh) = inp.size();
            let (cx, cy) = match self.values.center {
                Some((id, x, y)) if id == inp.id => (x, y),
                _ => (lw / 2, lh / 2),
            };
            let (w, h) = (W.min(lw), H.min(lh));
            ((cx.saturating_sub(W / 2)).min(lw - w), (cy.saturating_sub(H / 2)).min(lh - h), w, h)
        });
        if let (Some(inp), Some(win)) = (&inp, win) {
            let here = self.values.data.as_ref().is_some_and(|d| d.0 == inp.id && d.1 == win);
            let asked = self.values.req.as_ref().is_some_and(|r| r.1 == inp.id && r.2 == win);
            if !here && !asked {
                let req = self.engine.read(inp.clone(), 0, win);
                self.values.req = Some((req, inp.id, win));
            }
        }
        let mut open = true;
        let mut values = std::mem::take(&mut self.values);
        let n_inputs = self.pane(self.active).and_then(|p| p.layers.get(p.sel)).map_or(0, |l| l.inputs.len());
        let names: Vec<String> = self.pane(self.active).and_then(|p| p.layers.get(p.sel)).map_or(vec![], |l| (0..n_inputs).map(|j| l.used.get(j).map_or(String::new(), |&c| l.chans[c].id.clone())).collect());
        let center_ll = inp.as_ref().and_then(|_| self.hovered.and_then(|id| self.pane(id)?.v.cursor.and_then(|c| self.lonlat(id, c))));
        egui::Window::new(t("Values")).open(&mut open).default_size([780.0, 470.0]).show(ctx, |ui| {
            let Some(inp) = &inp else {
                ui.label(t("Open a product, then select one of its layers."));
                return;
            };
            ui.horizontal(|ui| {
                if n_inputs > 1 {
                    for (j, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut values.input, j, n);
                    }
                    ui.separator();
                }
                ui.checkbox(&mut values.follow, t("Follow the cursor"));
                let v = inp.var();
                ui.weak(if v.units.is_empty() { inp.var().name.clone() } else { format!("{} ({})", v.name, v.units) });
            });
            let Some((_, (x0, y0, w, h), vals)) = values.data.as_ref().filter(|d| d.0 == inp.id) else {
                ui.label("...");
                return;
            };
            let (x0, y0, w, h) = (*x0, *y0, *w, *h);
            let var = inp.var();
            let dec = (var.scale != 1.0 && var.scale != 0.0 && !matches!(var.levels[0].dtype, eo_core::DType::F32 | eo_core::DType::F64 | eo_core::DType::CF32 | eo_core::DType::CF64)).then(|| (-var.scale.abs().log10() - 1e-4).ceil().max(0.0) as usize);
            let center = values.center.filter(|c| c.0 == inp.id).map(|c| (c.1, c.2));
            ui.horizontal(|ui| {
                let (lw, lh) = inp.size();
                let mut mv = |dx: i64, dy: i64| {
                    let (cx, cy) = center.unwrap_or((x0 + w / 2, y0 + h / 2));
                    let nx = (cx as i64 + dx).clamp(0, lw as i64 - 1) as u64;
                    let ny = (cy as i64 + dy).clamp(0, lh as i64 - 1) as u64;
                    values.center = Some((inp.id, nx, ny));
                    values.follow = false;
                };
                if ui.button("<").on_hover_text(t("Move left")).clicked() {
                    mv(-(W as i64), 0);
                }
                if ui.button(">").on_hover_text(t("Move right")).clicked() {
                    mv(W as i64, 0);
                }
                if ui.button("^").on_hover_text(t("Move up")).clicked() {
                    mv(0, -(H as i64));
                }
                if ui.button("v").on_hover_text(t("Move down")).clicked() {
                    mv(0, H as i64);
                }
                ui.separator();
                ui.label(tf("Columns {} to {}, rows {} to {} of {} x {}", &[&x0.to_string(), &(x0 + w - 1).to_string(), &y0.to_string(), &(y0 + h - 1).to_string(), &lw.to_string(), &lh.to_string()]));
                if ui.button(t("Copy as CSV")).clicked() {
                    let mut s = String::from("row/column");
                    for x in x0..x0 + w {
                        s += &format!(",{x}");
                    }
                    for r in 0..h {
                        s += &format!("\n{}", y0 + r);
                        for c in 0..w {
                            let v = vals[(r * w + c) as usize];
                            s += &if v.is_nan() { ",".to_string() } else { format!(",{v}") };
                        }
                    }
                    ui.ctx().copy_text(s);
                }
            });
            if let Some((lon, lat)) = center_ll.filter(|_| values.follow) {
                ui.weak(format!("lat {lat:.5}  lon {lon:.5}"));
            }
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("values").striped(true).spacing([14.0, 3.0]).min_col_width(64.0).show(ui, |ui| {
                    ui.weak("");
                    for x in x0..x0 + w {
                        ui.weak(x.to_string());
                    }
                    ui.end_row();
                    for r in 0..h {
                        ui.weak((y0 + r).to_string());
                        for c in 0..w {
                            let text = egui::RichText::new(num(vals[(r * w + c) as usize], dec)).monospace();
                            let hit = center == Some((x0 + c, y0 + r));
                            ui.label(if hit { text.strong().color(ui.visuals().selection.stroke.color) } else { text });
                        }
                        ui.end_row();
                    }
                });
            });
        });
        values.open = open;
        self.values = values;
    }
}
