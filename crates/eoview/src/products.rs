//! The Products panel (the Product Explorer of SNAP, the sources of Panoply): the open products of all
//! views, the variables of each one in the tree of its groups, and the metadata of the selected variable
//! (dimensions, attributes, storage). A double click adds a variable to the active view, a drag to any view.
use crate::app::App;
use crate::lang::{t, tf};
use crate::ui::Cmd;
use eo_cache::Layer;
use std::sync::Arc;

/// A variable that the user drags to a view: layer `li` of view `pane` has its product, `c` is its channel.
#[derive(Clone, Copy, Debug)]
pub struct DragVar {
    pub pane: u32,
    pub li: usize,
    pub c: usize,
}

/// The state of the panel: the selected variable (dataset id and name), the search text.
#[derive(Default)]
pub struct Products {
    pub sel: Option<(u64, String)>,
    pub filter: String,
}

impl App {
    /// The open products: for each dataset, a layer of it, its view and the index of its map layer.
    fn open_products(&self) -> Vec<(Arc<Layer>, u32, usize)> {
        let mut out: Vec<(Arc<Layer>, u32, usize)> = vec![];
        for p in self.panes.iter().filter(|p| !self.floating.contains(&p.id) || self.wins.iter().any(|w| w.pane == p.id)) {
            for (li, l) in p.layers.iter().enumerate() {
                if let Some(a) = l.any().filter(|a| a.op.is_none())
                    && !out.iter().any(|o| o.0.ds_id == a.ds_id)
                {
                    out.push((a.clone(), p.id, li));
                }
            }
        }
        out
    }

    /// The Products panel.
    pub fn products_ui(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<(Cmd, u32)>) {
        let prods = self.open_products();
        if prods.is_empty() {
            ui.label(t("No product. Open a product (Ctrl+O) or drop files on a view."));
            return;
        }
        ui.add(egui::TextEdit::singleline(&mut self.products.filter).hint_text(t("Search the variables and the attributes")).desired_width(f32::INFINITY));
        let q = self.products.filter.trim().to_lowercase();
        let active = self.active;
        let mut sel = self.products.sel.clone();
        egui::ScrollArea::vertical().id_salt("products").max_height(ui.available_height() * 0.55).auto_shrink([false, true]).show(ui, |ui| {
            for (a, pane, li) in &prods {
                let p = &a.ds.product;
                let metas = if p.info.vars.is_empty() { p.vars.iter().map(crate::info::meta_of).collect() } else { p.info.vars.clone() };
                let hit = |m: &eo_core::Meta| q.is_empty() || m.name.to_lowercase().contains(&q) || m.attrs.iter().any(|(k, v)| k.to_lowercase().contains(&q) || v.to_lowercase().contains(&q));
                let head = egui::CollapsingHeader::new(egui::RichText::new(&p.name).strong()).id_salt(("product", a.ds_id)).default_open(true);
                head.show(ui, |ui| {
                    ui.small(&p.desc);
                    // The variables by group (the directory part of the name), the root first.
                    let mut groups: Vec<String> = vec![];
                    for m in &metas {
                        let g = m.name.rsplit_once('/').map_or(String::new(), |x| x.0.trim_start_matches('/').to_string());
                        if !groups.contains(&g) {
                            groups.push(g);
                        }
                    }
                    groups.sort_by_key(|g| !g.is_empty());
                    for g in groups {
                        let in_g: Vec<&eo_core::Meta> = metas.iter().filter(|m| m.name.rsplit_once('/').map_or(String::new(), |x| x.0.trim_start_matches('/').to_string()) == g && hit(m)).collect();
                        if in_g.is_empty() {
                            continue;
                        }
                        let rows = |ui: &mut egui::Ui| {
                            for m in in_g {
                                let leaf = m.name.rsplit('/').next().unwrap_or(&m.name);
                                let shape = m.dims.iter().map(|d| d.1.to_string()).collect::<Vec<_>>().join(" x ");
                                let var = p.vars.iter().position(|v| v.name == m.name);
                                let on = sel.as_ref().is_some_and(|s| s.0 == a.ds_id && s.1 == m.name);
                                let text = if shape.is_empty() { leaf.to_string() } else { format!("{leaf}   {shape}") };
                                let label = if var.is_some() { egui::RichText::new(text) } else { egui::RichText::new(text).weak() };
                                let r = ui.add(egui::Button::selectable(on, label).sense(egui::Sense::click_and_drag())).on_hover_text(format!("{}  ({})  {}", m.name, crate::info::dims_text(&m.dims), m.dtype));
                                if r.clicked() {
                                    sel = Some((a.ds_id, m.name.clone()));
                                }
                                // A variable that the views can show: double click to the active view, drag to a view.
                                let c = var.and_then(|v| self.pane(*pane).and_then(|p| p.layers.get(*li)).and_then(|l| l.chans.iter().position(|c| c.var == v && c.choice == 0)));
                                if let Some(c) = c {
                                    if r.double_clicked() {
                                        cmds.push((Cmd::AddVar(*pane, *li, c), active));
                                    }
                                    r.dnd_set_drag_payload(DragVar { pane: *pane, li: *li, c });
                                }
                            }
                        };
                        if g.is_empty() {
                            rows(ui);
                        } else {
                            egui::CollapsingHeader::new(&g).id_salt(("group", a.ds_id, &g)).default_open(!q.is_empty() || metas.len() <= 40).show(ui, rows);
                        }
                    }
                });
            }
        });
        self.products.sel = sel.clone();
        // The metadata of the selected variable, as Panoply shows it.
        let Some((ds, name)) = sel else {
            ui.separator();
            ui.weak(t("Select a variable: its dimensions and attributes show here. Double click: a layer of the active view. Drag it to a view."));
            return;
        };
        let Some((a, pane, li)) = prods.iter().find(|p| p.0.ds_id == ds).cloned() else { return };
        let p = &a.ds.product;
        let metas = if p.info.vars.is_empty() { p.vars.iter().map(crate::info::meta_of).collect() } else { p.info.vars.clone() };
        let Some(m) = metas.iter().find(|m| m.name == name) else { return };
        ui.separator();
        ui.strong(&m.name);
        ui.small(format!("{}  ({})", m.dtype, crate::info::dims_text(&m.dims)));
        let var = p.vars.iter().position(|v| v.name == m.name);
        let c = var.and_then(|v| self.pane(pane).and_then(|p| p.layers.get(li)).and_then(|l| l.chans.iter().position(|c| c.var == v && c.choice == 0)));
        if let Some(c) = c {
            ui.horizontal(|ui| {
                if ui.button(t("Add to the view")).on_hover_text(t("A new layer of the active view")).clicked() {
                    cmds.push((Cmd::AddVar(pane, li, c), active));
                }
                if ui.button(t("Open in a new view")).clicked() {
                    cmds.push((Cmd::ShowVar(li, c, true), pane));
                }
            });
        }
        // A scalar shows its value, a 1D variable a chart of its values.
        if let Some(k) = p.small.iter().position(|v| v.name == m.name) {
            match self.smalls.get(&a, k) {
                Some(Ok(v)) if p.small[k].levels[0].shape.is_empty() => drop(ui.label(tf("Value: {}", &[&format!("{} {}", crate::dims::num(v.first().copied().unwrap_or(f64::NAN)), p.small[k].units)]))),
                Some(Ok(v)) => {
                    let step = v.len().div_ceil(2000).max(1);
                    let pts: Vec<(f64, Vec<f64>)> = v.iter().enumerate().step_by(step).map(|(i, x)| (i as f64, vec![*x])).collect();
                    crate::tools::line_chart(ui, &pts, &[p.small[k].units.clone()], 0.0, &|d| format!("{} {}", t("index"), d.round()));
                }
                Some(Err(e)) => drop(ui.colored_label(ui.visuals().error_fg_color, e)),
                None => drop(ui.spinner()),
            }
        }
        egui::ScrollArea::vertical().id_salt("product meta").auto_shrink([false, false]).show(ui, |ui| {
            if let Some(v) = var.map(|v| &p.vars[v]) {
                egui::CollapsingHeader::new(t("Storage")).default_open(false).show(ui, |ui| crate::info::attrs_grid(ui, ("pstore", &m.name), &crate::info::storage(v), ""));
            }
            egui::CollapsingHeader::new(tf("Attributes ({})", &[&m.attrs.len().to_string()])).id_salt(("pattrs", &m.name)).default_open(true).show(ui, |ui| {
                if m.attrs.is_empty() {
                    ui.weak(t("No attributes."));
                }
                crate::info::attrs_grid(ui, ("pattrs grid", &m.name), &m.attrs, "");
            });
            if !p.info.attrs.is_empty() {
                egui::CollapsingHeader::new(tf("Global attributes ({})", &[&p.info.attrs.len().to_string()])).id_salt(("pglobal", ds)).default_open(false).show(ui, |ui| crate::info::attrs_grid(ui, ("pglobal grid", ds), &p.info.attrs, ""));
            }
            if !p.info.dims.is_empty() {
                egui::CollapsingHeader::new(tf("Dimensions ({})", &[&p.info.dims.len().to_string()])).id_salt(("pdims", ds)).default_open(false).show(ui, |ui| {
                    for (n, z) in &p.info.dims {
                        ui.label(format!("{n} = {z}"));
                    }
                });
            }
        });
    }
}
