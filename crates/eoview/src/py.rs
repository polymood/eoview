//! Python: scripts and notebooks that use the module `eoview` (DESIGN.md, section 6).
//!
//! eoview listens on a local TCP port (127.0.0.1). A client sends a token first: the port and the token are
//! in `server.json` in the configuration directory (for a notebook), or in the environment of a script that
//! the Python panel starts. Then each request is a line of JSON and `bytes` bytes of data. Each answer is
//! the same. The module `eoview` is in `python/eoview/__init__.py`: eoview writes a copy of it in the
//! configuration directory and puts it on the path of its scripts.
use crate::app::App;
use crate::lang::{t, tf};
use eo_core::{Crs, Georef};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex, mpsc};

/// The module `eoview` for Python.
const MODULE: &str = include_str!("../../../python/eoview/__init__.py");

/// The channel of the answer to a client.
type Reply = mpsc::Sender<(Value, Vec<u8>)>;

/// A request of a client, and the channel of its answer.
pub struct Req {
    pub head: Value,
    pub data: Vec<u8>,
    pub reply: mpsc::Sender<(Value, Vec<u8>)>,
}

/// A chart of a script: its name and its image.
pub struct Chart {
    pub name: String,
    pub tex: egui::TextureHandle,
}

/// The Python part of the application.
pub struct Py {
    pub port: u16,
    token: String,
    rx: mpsc::Receiver<Req>,
    /// Reads of the inputs of the clients: engine request to the answer channel and the text of the layer.
    pub reads: std::collections::HashMap<u64, (Reply, Value)>,
    /// The directory of the module `eoview`.
    pub path: Option<std::path::PathBuf>,
    /// The panel: open or not, the code, the script that runs, its output.
    pub open: bool,
    pub code: String,
    pub child: Option<std::process::Child>,
    pub log: Arc<Mutex<String>>,
    pub charts: Vec<Chart>,
    pub charts_open: bool,
    /// A script made layers in this session: the workspace file keeps its code.
    pub used: bool,
    /// The script of a workspace file: the panel asks the user to run it.
    pub ask: bool,
}

/// An answer with an error.
fn err(e: impl std::fmt::Display) -> (Value, Vec<u8>) {
    (json!({ "ok": false, "error": e.to_string() }), vec![])
}

/// Read one message: a line of JSON, then `bytes` bytes.
fn read_msg(r: &mut impl BufRead) -> std::io::Result<Option<(Value, Vec<u8>)>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let head: Value = serde_json::from_str(&line).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let n = head.get("bytes").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut data = vec![0; n];
    r.read_exact(&mut data)?;
    Ok(Some((head, data)))
}

fn write_msg(w: &mut impl Write, mut head: Value, data: &[u8]) -> std::io::Result<()> {
    head["bytes"] = data.len().into();
    w.write_all(format!("{head}\n").as_bytes())?;
    w.write_all(data)?;
    w.flush()
}

/// A random token: the keys of the hash of the standard library are random for each process.
fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    (0..2).map(|k| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(k ^ std::process::id() as u64);
        format!("{:016x}", h.finish())
    })
    .collect()
}

/// The georeferencing as JSON for Python: a transform (GDAL order) and a CRS, or a grid of longitudes and latitudes.
pub fn georef_json(g: &Georef) -> Value {
    match g {
        Georef::Affine { gt, crs } => json!({ "transform": gt, "epsg": crs.epsg, "crs": crs.name }),
        Georef::Grid { cols, rows, lon, lat } => json!({ "grid": { "cols": cols, "rows": rows, "lon": lon, "lat": lat } }),
        _ => json!({}),
    }
}

/// The georeferencing of JSON from Python (see `georef_json`).
pub fn georef_of(v: &Value) -> Result<Georef, String> {
    let nums = |v: &Value| -> Result<Vec<f64>, String> { v.as_array().ok_or("not a list")?.iter().map(|x| x.as_f64().ok_or_else(|| "not a number".to_string())).collect() };
    if let Some(gt) = v.get("transform").filter(|x| !x.is_null()) {
        let gt: [f64; 6] = nums(gt)?.try_into().map_err(|_| "a transform has 6 numbers (GDAL order)".to_string())?;
        let epsg = v.get("epsg").and_then(Value::as_u64).map(|e| e as u32);
        let name = v.get("crs").and_then(Value::as_str).map_or_else(|| epsg.map_or(String::new(), |e| format!("EPSG:{e}")), String::from);
        return Ok(Georef::Affine { gt, crs: Crs { epsg, name } });
    }
    if let Some(g) = v.get("grid").filter(|x| !x.is_null()) {
        let f = |k: &str| nums(g.get(k).unwrap_or(&Value::Null));
        return Ok(Georef::Grid { cols: f("cols")?, rows: f("rows")?, lon: f("lon")?, lat: f("lat")? });
    }
    Ok(Georef::None)
}

impl Py {
    /// Listen on a local port. `wake` wakes the event loop of the application after each request.
    pub fn start(wake: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Py> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let tok = token();
        let (tx, rx) = mpsc::channel::<Req>();
        let tok2 = tok.clone();
        std::thread::Builder::new().name("eoview-py".into()).spawn(move || {
            for s in listener.incoming().flatten() {
                let (tx, tok, wake) = (tx.clone(), tok2.clone(), wake.clone());
                let _ = std::thread::Builder::new().name("eoview-py-client".into()).spawn(move || serve(s, &tok, &tx, &*wake));
            }
        })?;
        // The module and the address for the notebooks.
        let dir = crate::prefs::config_dir();
        let path = dir.as_ref().map(|d| d.join("python"));
        if let Some(p) = &path {
            let _ = std::fs::create_dir_all(p.join("eoview")).and_then(|_| std::fs::write(p.join("eoview").join("__init__.py"), MODULE));
        }
        if let Some(d) = &dir {
            let _ = std::fs::write(d.join("server.json"), json!({ "port": port, "token": tok, "pid": std::process::id() }).to_string());
        }
        let code = path.as_ref().and_then(|p| std::fs::read_to_string(p.join("script.py")).ok()).unwrap_or_else(|| {
            "import eoview as ev\n\nimg = ev.input()          # the selected layer of the active view\nev.output(img * 2, name=\"twice\")\n".into()
        });
        Ok(Py { port, token: tok, rx, reads: Default::default(), path, open: false, code, child: None, log: Default::default(), charts: vec![], charts_open: false, used: false, ask: false })
    }
}

/// One client: the token, then the requests. Each request goes to the application, then its answer goes back.
fn serve(s: std::net::TcpStream, tok: &str, tx: &mpsc::Sender<Req>, wake: &(dyn Fn() + Send + Sync)) {
    let Ok(mut w) = s.try_clone() else { return };
    let mut r = BufReader::new(s);
    match read_msg(&mut r) {
        Ok(Some((h, _))) if h.get("token").and_then(Value::as_str) == Some(tok) => {
            let _ = write_msg(&mut w, json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }), &[]);
        }
        _ => {
            let _ = write_msg(&mut w, json!({ "ok": false, "error": "bad token" }), &[]);
            return;
        }
    }
    while let Ok(Some((head, data))) = read_msg(&mut r) {
        let (reply, answer) = mpsc::channel();
        if tx.send(Req { head, data, reply }).is_err() {
            return;
        }
        wake();
        let Ok((h, d)) = answer.recv() else { return };
        if write_msg(&mut w, h, &d).is_err() {
            return;
        }
    }
}

impl App {
    /// The Python requests that wait: answer them. Return true if a request came.
    pub fn py_poll(&mut self) -> bool {
        let reqs: Vec<Req> = self.py.as_ref().map_or(vec![], |p| p.rx.try_iter().collect());
        let any = !reqs.is_empty();
        for r in reqs {
            let cmd = r.head.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
            let res = match cmd.as_str() {
                "layers" => Ok(self.py_layers()),
                "input" => match self.py_input(&r.head) {
                    // The answer comes with the data (`py_read`).
                    Ok((req, meta)) => {
                        if let Some(p) = &mut self.py {
                            p.reads.insert(req, (r.reply.clone(), meta));
                        }
                        continue;
                    }
                    Err(e) => Err(e),
                },
                "output" => self.py_output(&r.head, &r.data),
                "plot" => self.py_plot(&r.head, &r.data),
                c => Err(format!("unknown command {c}")),
            };
            let _ = r.reply.send(match res {
                Ok(v) => (v, vec![]),
                Err(e) => err(e),
            });
        }
        // A script of the panel that ended.
        if let Some(p) = &mut self.py
            && let Some(c) = &mut p.child
            && let Ok(Some(st)) = c.try_wait()
        {
            let line = if st.success() { t("The script ended.").to_string() } else { tf("The script stopped: {}", &[&st.to_string()]) };
            p.log.lock().unwrap().push_str(&format!("--- {line}\n"));
            p.child = None;
        }
        any
    }

    /// The views, the active view first.
    fn py_views(&self) -> Vec<&crate::app::Pane> {
        let mut v: Vec<&crate::app::Pane> = self.panes.iter().filter(|p| p.id != self.active && !p.layers.is_empty()).collect();
        v.extend(self.pane(self.active));
        v.rotate_right(1);
        v
    }

    /// The layers of all views, the active view first, top layer first.
    fn py_layers(&self) -> Value {
        let ls: Vec<Value> = self
            .py_views()
            .iter()
            .flat_map(|p| p.layers.iter().rev().map(|l| json!({ "name": l.label(200), "band": l.comp_name(), "steps": l.steps.len().max(1), "view": p.id, "active": p.id == self.active })))
            .collect();
        json!({ "ok": true, "layers": ls })
    }

    /// Start the read of an input: the layer (`layer`: a name, else the selected layer), the extent (`view`
    /// or `all`) and the level. Return the engine request and the text of the layer.
    fn py_input(&mut self, h: &Value) -> Result<(u64, Value), String> {
        // A name: the layer in the active view, else in an other view.
        let (p, li) = match h.get("layer").and_then(Value::as_str) {
            Some(n) => self
                .py_views()
                .into_iter()
                .find_map(|p| Some((p, p.layers.iter().rposition(|l| l.label(200) == n || l.comp_name() == n || l.name == n)?)))
                .ok_or_else(|| format!("no layer {n} in the views"))?,
            None => {
                let p = self.pane(self.active).ok_or("no view")?;
                (p, p.sel)
            }
        };
        let l = p.layers.get(li).ok_or("the view has no layer")?;
        if l.kind != crate::layer::Kind::Band || l.inputs.len() != 1 {
            return Err("an input is one band: show one band of the layer".into());
        }
        let x = l.inputs[0].clone();
        let all = h.get("extent").and_then(Value::as_str) == Some("all");
        let k = p.v.inputs.iter().position(|i| i.layer.id == x.id);
        let level = match h.get("level").and_then(Value::as_u64) {
            Some(v) => v as usize,
            None if all => 0,
            None => k.and_then(|k| p.v.levels.get(k).copied()).unwrap_or(0),
        }
        .min(x.levels.len() - 1);
        let lv = x.levels[level];
        let mut win = (0, 0, lv.w, lv.h);
        if !all {
            // The pixel box of the view, at the level.
            let warp = k.and_then(|k| p.v.inputs[k].warp.clone()).ok_or("the layer does not show in the view")?.0;
            let (w0, h0) = x.size();
            let b = crate::tools::pixel_box(&warp, p.v.rect(), w0 as f64, h0 as f64).ok_or("the layer is not in the view")?;
            let cx = |v: f64| (((v - lv.ox) / lv.kx).max(0.0) as u64).min(lv.w);
            let cy = |v: f64| (((v - lv.oy) / lv.ky).max(0.0) as u64).min(lv.h);
            let (x0, y0, x1, y1) = (cx(b[0]), cy(b[1]), cx(b[2]).max(cx(b[2] + lv.kx)), cy(b[3]).max(cy(b[3] + lv.ky)));
            if x1 <= x0 || y1 <= y0 {
                return Err("the layer is not in the view".into());
            }
            win = (x0, y0, x1 - x0, y1 - y0);
        }
        let meta = json!({ "name": l.comp_name(), "units": x.var().units, "level": level, "x0": win.0, "y0": win.1, "w": win.2, "h": win.3 });
        Ok((self.engine.read(x, level, win), meta))
    }

    /// The values of an input are there: answer the client.
    pub fn py_read(&mut self, req: u64, res: eo_core::Result<(Arc<Vec<f32>>, Georef)>) {
        let Some((reply, mut meta)) = self.py.as_mut().and_then(|p| p.reads.remove(&req)) else { return };
        let ans = match res {
            Ok((v, g)) => {
                meta["ok"] = true.into();
                meta["georef"] = georef_json(&g);
                let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
                (meta, bytes)
            }
            Err(e) => err(e.0),
        };
        let _ = reply.send(ans);
    }

    /// A new layer in the active view from the values of a script.
    fn py_output(&mut self, h: &Value, data: &[u8]) -> Result<Value, String> {
        let w = h.get("w").and_then(Value::as_u64).ok_or("no width")?;
        let rows = h.get("h").and_then(Value::as_u64).ok_or("no height")?;
        if data.len() as u64 != w * rows * 4 {
            return Err(format!("{} bytes for {w} x {rows} values", data.len()));
        }
        let v: Vec<f32> = data.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        let g = georef_of(h.get("georef").unwrap_or(&Value::Null))?;
        let name = h.get("name").and_then(Value::as_str).unwrap_or("output").to_string();
        let units = h.get("units").and_then(Value::as_str).unwrap_or("").to_string();
        let req = self.engine.memory(name.clone(), units, w, v, g);
        if let Some(p) = &mut self.py {
            p.used = true;
        }
        self.open_req(req, self.active, name);
        Ok(json!({ "ok": true }))
    }

    /// A chart of a script (a PNG image) in the charts window.
    fn py_plot(&mut self, h: &Value, data: &[u8]) -> Result<Value, String> {
        let mut dec = png::Decoder::new(std::io::Cursor::new(data));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut r = dec.read_info().map_err(|e| e.to_string())?;
        let mut buf = vec![0; r.output_buffer_size().ok_or("bad image")?];
        let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
        let px = &buf[..info.buffer_size()];
        let rgba: Vec<u8> = match info.color_type {
            png::ColorType::Rgba => px.to_vec(),
            png::ColorType::Rgb => px.as_chunks::<3>().0.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
            png::ColorType::GrayscaleAlpha => px.as_chunks::<2>().0.iter().flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
            _ => px.iter().flat_map(|&c| [c, c, c, 255]).collect(),
        };
        let img = egui::ColorImage::from_rgba_unmultiplied([info.width as usize, info.height as usize], &rgba);
        let name = h.get("name").and_then(Value::as_str).unwrap_or("chart").to_string();
        let tex = self.ctx.load_texture(format!("chart {name}"), img, Default::default());
        if let Some(p) = &mut self.py {
            p.charts.retain(|c| c.name != name);
            p.charts.push(Chart { name, tex });
            p.charts_open = true;
        }
        Ok(json!({ "ok": true }))
    }

    /// Run the code of the panel in the Python of the preferences.
    pub fn py_run(&mut self) {
        let exe = if self.prefs.python.is_empty() { if cfg!(windows) { "python".to_string() } else { "python3".to_string() } } else { self.prefs.python.clone() };
        let Some(p) = &mut self.py else { return };
        if let Some(mut c) = p.child.take() {
            let _ = c.kill();
        }
        let Some(dir) = p.path.clone() else { return };
        let file = dir.join("script.py");
        if let Err(e) = std::fs::write(&file, &p.code) {
            p.log.lock().unwrap().push_str(&format!("{}: {e}\n", file.display()));
            return;
        }
        let path = std::env::var_os("PYTHONPATH").map_or(dir.clone().into_os_string(), |old| {
            let mut v = vec![dir.clone()];
            v.extend(std::env::split_paths(&old));
            std::env::join_paths(v).unwrap_or_default()
        });
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("-u").arg(&file).env("PYTHONPATH", path).env("EOVIEW_PORT", p.port.to_string()).env("EOVIEW_TOKEN", &p.token).env("MPLBACKEND", "Agg");
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        p.log.lock().unwrap().clear();
        match cmd.spawn() {
            Ok(mut c) => {
                // The output of the script goes to the log of the panel.
                for out in [c.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), c.stderr.take().map(|o| Box::new(o) as Box<dyn Read + Send>)].into_iter().flatten() {
                    let (log, ctx) = (p.log.clone(), self.ctx.clone());
                    std::thread::spawn(move || {
                        for line in BufReader::new(out).lines().map_while(Result::ok) {
                            log.lock().unwrap().push_str(&format!("{line}\n"));
                            ctx.request_repaint();
                        }
                    });
                }
                p.child = Some(c);
            }
            Err(e) => p.log.lock().unwrap().push_str(&format!("{exe}: {e}\n{}\n", t("Set the path of Python in the preferences."))),
        }
    }

    /// The Python panel: the code, Run and Stop, the output of the script.
    pub fn py_ui(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.py else { return };
        let (mut run, mut open) = (false, p.open);
        if p.open {
            egui::Window::new(t("Python")).open(&mut open).default_size([640.0, 520.0]).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let busy = p.child.is_some();
                    run = ui.add_enabled(!busy, egui::Button::new(t("Run")).shortcut_text("Ctrl+Enter")).on_hover_text(t("Run the script in the Python of the preferences")).clicked();
                    if ui.add_enabled(busy, egui::Button::new(t("Stop"))).clicked()
                        && let Some(mut c) = p.child.take()
                    {
                        let _ = c.kill();
                        p.log.lock().unwrap().push_str(&format!("--- {}\n", t("Stopped.")));
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                if p.ask {
                    ui.colored_label(ui.visuals().warn_fg_color, t("The workspace has this script: it makes some of its layers. Read the code, then Run."));
                }
                ui.label(egui::RichText::new(tf("Notebooks: import eoview, then eoview.connect(). Module: {}", &[&p.path.as_ref().map_or(String::new(), |d| d.display().to_string())])).small().weak());
                let h = ui.available_height();
                egui::ScrollArea::vertical().id_salt("code").max_height(h * 0.62).show(ui, |ui| {
                    let r = ui.add(egui::TextEdit::multiline(&mut p.code).code_editor().desired_width(f32::INFINITY).desired_rows(18));
                    run |= r.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
                });
                ui.separator();
                egui::ScrollArea::vertical().id_salt("log").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(p.log.lock().unwrap().as_str()).monospace()).wrap());
                });
            });
        }
        p.open = open;
        if p.charts_open && !p.charts.is_empty() {
            let mut open = true;
            let mut remove = None;
            egui::Window::new(t("Charts")).open(&mut open).default_size([520.0, 420.0]).show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (k, c) in p.charts.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.strong(&c.name);
                            if ui.small_button(t("Remove")).clicked() {
                                remove = Some(k);
                            }
                        });
                        let s = c.tex.size_vec2();
                        let w = ui.available_width().min(s.x);
                        ui.image((c.tex.id(), egui::vec2(w, s.y * w / s.x)));
                        ui.separator();
                    }
                });
            });
            if let Some(k) = remove {
                p.charts.remove(k);
            }
            p.charts_open = open;
        }
        if run {
            p.ask = false;
            self.py_run();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn georef_round_trip() {
        let a = Georef::Affine { gt: [10.0, 0.5, 0.0, 50.0, 0.0, -0.5], crs: Crs { epsg: Some(4326), name: "WGS 84".into() } };
        let g = Georef::Grid { cols: vec![0.0, 10.0], rows: vec![0.0, 5.0], lon: vec![1.0, 2.0, 3.0, 4.0], lat: vec![5.0, 6.0, 7.0, 8.0] };
        for x in [a, g, Georef::None] {
            assert_eq!(georef_of(&georef_json(&x)).unwrap(), x);
        }
    }
}
