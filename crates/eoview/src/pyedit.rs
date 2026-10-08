//! The Python editor of eoview: tabs of scripts and notebooks, syntax highlighting, and the interpreter.
//!
//! A script runs in a new Python process (`App::py_run`). The cells of a notebook run in one Python process
//! that keeps its variables (`KERNEL`): eoview writes the code of a cell to its input, and the process writes
//! a mark before and after the output of each cell. A file that changes on the disk (for example in VS Code)
//! opens again in its tab.
use crate::app::App;
use crate::lang::{t, tf};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

/// The program of the process of a notebook. The last expression of a cell shows its value, and the
/// matplotlib figures of a cell go to the charts window of eoview.
const KERNEL: &str = r#"
import ast, sys, traceback
sys.stderr = sys.stdout
g = {"__name__": "__main__"}
while True:
    head = sys.stdin.buffer.readline()
    if not head:
        break
    n, k = map(int, head.split())
    code = sys.stdin.buffer.read(n).decode("utf-8")
    print("\x1eCELL %d" % k, flush=True)
    try:
        tree = ast.parse(code, "<cell>")
        last = tree.body.pop() if tree.body and isinstance(tree.body[-1], ast.Expr) else None
        exec(compile(tree, "<cell>", "exec"), g)
        if last is not None:
            v = eval(compile(ast.Expression(last.value), "<cell>", "eval"), g)
            if v is not None:
                print(repr(v))
        plt = sys.modules.get("matplotlib.pyplot")
        if plt is not None and plt.get_fignums():
            import eoview
            for i in plt.get_fignums():
                eoview.plot(plt.figure(i), name="figure %d" % i)
            plt.close("all")
    except BaseException:
        # Without the frame of this program.
        t, e, tb = sys.exc_info()
        traceback.print_exception(t, e, tb.tb_next)
    sys.stdout.flush()
    print("\x1eDONE %d" % k, flush=True)
"#;

/// The information of the interpreter: the version, the path, and the versions of the main packages.
const INFO: &str = r#"
import importlib, json, sys
p = {}
for m in ("numpy", "xarray", "matplotlib", "scipy"):
    try:
        p[m] = getattr(importlib.import_module(m), "__version__", "?")
    except Exception:
        p[m] = None
print(json.dumps({"version": sys.version.split()[0], "exe": sys.executable, "venv": sys.prefix != sys.base_prefix, "packages": p}))
"#;

/// A cell of a notebook (a script has one cell).
pub struct Cell {
    pub id: u64,
    pub code: String,
    pub md: bool,
}

/// The Python process of a notebook.
pub struct Kernel {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    /// Cells sent and not done.
    pending: Arc<AtomicUsize>,
    /// The cell that runs now.
    running: Arc<Mutex<Option<u64>>>,
}

impl Drop for Kernel {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// A tab of the editor: the script of the workspace (tab 0), a script or a notebook.
pub struct Doc {
    /// The file. None: not saved (the script of the workspace, or a new script).
    pub path: Option<PathBuf>,
    pub name: String,
    pub nb: bool,
    pub cells: Vec<Cell>,
    pub dirty: bool,
    /// The time of the file when it opened or when eoview wrote it.
    mtime: Option<std::time::SystemTime>,
    /// The file changed on the disk and the tab has changes: the user selects which one stays.
    pub conflict: bool,
    /// Run the script when its file changes (an editor outside eoview saves it).
    pub auto_run: bool,
    /// The JSON of a notebook: its metadata stays when eoview writes it.
    raw: Option<Value>,
    kernel: Option<Kernel>,
    /// The output of each cell of a notebook (u64::MAX: the messages of the process).
    outs: Arc<Mutex<HashMap<u64, String>>>,
}

/// Call `f` with each line of `r`, until its end. A text that is not UTF-8 does not stop the reading: a
/// reader that stops closes the pipe, and the next `print` of the script fails (Windows: OSError 22).
pub fn each_line(r: impl Read, mut f: impl FnMut(&str)) {
    let mut r = BufReader::new(r);
    let mut buf = vec![];
    while r.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
        f(String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r']));
        buf.clear();
    }
}

fn cell_id() -> u64 {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    NEXT.fetch_add(1, Relaxed) as u64
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// The text of a `source` of a notebook: a string, or a list of lines.
fn source(v: &Value) -> String {
    match v {
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        v => v.as_str().unwrap_or("").to_string(),
    }
}

/// A text as the lines of a notebook (each line with its end).
fn lines(s: &str) -> Vec<String> {
    s.split_inclusive('\n').map(String::from).collect()
}

impl Doc {
    pub fn script(name: &str, code: String) -> Doc {
        Doc { path: None, name: name.into(), nb: false, cells: vec![Cell { id: cell_id(), code, md: false }], dirty: false, mtime: None, conflict: false, auto_run: false, raw: None, kernel: None, outs: Default::default() }
    }

    pub fn notebook(name: &str) -> Doc {
        let mut d = Doc::script(name, "import eoview as ev\n\nev.layers()\n".into());
        d.nb = true;
        d
    }

    /// Open a script (`.py`) or a notebook (`.ipynb`).
    pub fn open(path: &Path) -> Result<Doc, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let mut d = Doc::script(&name, String::new());
        (d.path, d.mtime) = (Some(path.to_path_buf()), mtime(path));
        d.load(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(d)
    }

    /// The cells of the text of the file.
    fn load(&mut self, text: &str) -> Result<(), String> {
        self.nb = self.path.as_ref().is_some_and(|p| p.extension().is_some_and(|e| e == "ipynb"));
        if !self.nb {
            self.cells = vec![Cell { id: cell_id(), code: text.into(), md: false }];
            return Ok(());
        }
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let mut outs = self.outs.lock().unwrap();
        outs.clear();
        self.cells = v["cells"]
            .as_array()
            .ok_or("no cells: not a notebook")?
            .iter()
            .filter(|c| c["cell_type"] != "raw")
            .map(|c| {
                let id = cell_id();
                // The text outputs of the file.
                let out: String = c["outputs"].as_array().into_iter().flatten().map(|o| source(if o["text"].is_null() { &o["data"]["text/plain"] } else { &o["text"] })).collect();
                if !out.is_empty() {
                    outs.insert(id, out);
                }
                Cell { id, code: source(&c["source"]), md: c["cell_type"] == "markdown" }
            })
            .collect();
        drop(outs);
        self.raw = Some(v);
        Ok(())
    }

    /// The text of the file: the code, or the JSON of the notebook with the text outputs of the cells.
    fn text(&self) -> String {
        if !self.nb {
            return self.cells.first().map_or(String::new(), |c| c.code.clone());
        }
        let outs = self.outs.lock().unwrap();
        let cells: Vec<Value> = self
            .cells
            .iter()
            .map(|c| match c.md {
                true => json!({ "cell_type": "markdown", "metadata": {}, "source": lines(&c.code) }),
                false => {
                    let out: Vec<Value> = outs.get(&c.id).filter(|o| !o.is_empty()).map(|o| json!({ "output_type": "stream", "name": "stdout", "text": lines(o) })).into_iter().collect();
                    json!({ "cell_type": "code", "execution_count": null, "metadata": {}, "outputs": out, "source": lines(&c.code) })
                }
            })
            .collect();
        let mut v = self.raw.clone().unwrap_or_else(|| {
            json!({ "metadata": { "kernelspec": { "display_name": "Python 3", "language": "python", "name": "python3" }, "language_info": { "name": "python" } }, "nbformat": 4, "nbformat_minor": 5 })
        });
        v["cells"] = Value::Array(cells);
        serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
    }

    /// Write the file (`path`: a new file).
    pub fn save(&mut self, path: Option<PathBuf>) -> Result<(), String> {
        if let Some(p) = path {
            self.name = p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            self.nb = p.extension().is_some_and(|e| e == "ipynb");
            self.path = Some(p);
        }
        let p = self.path.clone().ok_or("no file")?;
        std::fs::write(&p, self.text()).map_err(|e| format!("{}: {e}", p.display()))?;
        (self.mtime, self.dirty, self.conflict) = (mtime(&p), false, false);
        Ok(())
    }

    /// True if the file changed on the disk: the tab opens it again if it has no changes. Return true if it did.
    pub fn poll(&mut self) -> bool {
        let Some(p) = &self.path else { return false };
        let m = mtime(p);
        if m.is_none() || m == self.mtime {
            return false;
        }
        if self.dirty {
            self.conflict = true;
            return false;
        }
        self.reload().is_ok()
    }

    pub fn reload(&mut self) -> Result<(), String> {
        let p = self.path.clone().ok_or("no file")?;
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        self.load(&text)?;
        (self.mtime, self.dirty, self.conflict) = (mtime(&p), false, false);
        Ok(())
    }

    /// True if cells of the notebook wait or run.
    pub fn busy(&self) -> bool {
        self.kernel.as_ref().is_some_and(|k| k.pending.load(Relaxed) > 0)
    }

    pub fn stop(&mut self) {
        self.kernel = None;
    }

    /// Run the code cells `ids` in the process of the notebook (started by `start` if it does not run).
    pub fn run_cells(&mut self, ids: &[u64], start: impl FnOnce() -> Option<std::process::Command>) -> Result<(), String> {
        if self.kernel.as_mut().is_some_and(|k| k.child.try_wait().ok().flatten().is_some()) {
            self.kernel = None;
        }
        if self.kernel.is_none() {
            let mut cmd = start().ok_or("no Python")?;
            cmd.arg("-u").arg("-c").arg(KERNEL);
            if let Some(d) = self.path.as_ref().and_then(|p| p.parent()).filter(|d| !d.as_os_str().is_empty()) {
                cmd.current_dir(d);
            }
            cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let stdin = child.stdin.take().ok_or("no input")?;
            let (pending, running) = (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)));
            if let Some(out) = child.stdout.take() {
                let (outs, pending, running) = (self.outs.clone(), pending.clone(), running.clone());
                std::thread::spawn(move || {
                    each_line(out, |line| {
                        if let Some(id) = line.strip_prefix("\x1eCELL ").and_then(|v| v.parse().ok()) {
                            *running.lock().unwrap() = Some(id);
                            outs.lock().unwrap().insert(id, String::new());
                        } else if line.starts_with("\x1eDONE ") {
                            *running.lock().unwrap() = None;
                            let _ = pending.fetch_update(Relaxed, Relaxed, |n| n.checked_sub(1));
                        } else {
                            let id = running.lock().unwrap().unwrap_or(u64::MAX);
                            outs.lock().unwrap().entry(id).or_default().push_str(&format!("{line}\n"));
                        }
                    });
                    // The process ended: no cell waits.
                    pending.store(0, Relaxed);
                });
            }
            if let Some(err) = child.stderr.take() {
                let outs = self.outs.clone();
                std::thread::spawn(move || {
                    let mut b = vec![];
                    let _ = BufReader::new(err).read_to_end(&mut b);
                    let s = String::from_utf8_lossy(&b);
                    if !s.is_empty() {
                        outs.lock().unwrap().entry(u64::MAX).or_default().push_str(&s);
                    }
                });
            }
            self.outs.lock().unwrap().remove(&u64::MAX);
            self.kernel = Some(Kernel { child, stdin, pending, running });
        }
        let k = self.kernel.as_mut().unwrap();
        for id in ids {
            let Some(c) = self.cells.iter().find(|c| c.id == *id && !c.md) else { continue };
            self.outs.lock().unwrap().insert(*id, String::new());
            k.pending.fetch_add(1, Relaxed);
            let b = c.code.as_bytes();
            k.stdin.write_all(format!("{} {id}\n", b.len()).as_bytes()).and_then(|_| k.stdin.write_all(b)).and_then(|_| k.stdin.flush()).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// The colors of the syntax: keyword, built-in, string, number, comment, function name, decorator.
fn colors(dark: bool) -> [Color32; 7] {
    let c = Color32::from_rgb;
    if dark {
        [c(198, 120, 221), c(86, 182, 194), c(152, 195, 121), c(209, 154, 102), c(127, 132, 142), c(97, 175, 239), c(229, 192, 123)]
    } else {
        [c(166, 38, 164), c(1, 132, 188), c(80, 161, 79), c(152, 104, 1), c(140, 140, 140), c(64, 120, 242), c(193, 132, 1)]
    }
}

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "match", "case",
];

const BUILTINS: &[&str] = &[
    "print", "len", "range", "enumerate", "zip", "map", "filter", "sorted", "sum", "min", "max", "abs", "round", "int", "float", "str", "bool", "list", "dict", "set", "tuple", "type", "isinstance", "open", "next", "iter", "any", "all", "self", "super", "object", "Exception",
];

/// The kinds of the tokens of Python code: an index into `colors`, or None for the default color.
pub fn tokens(s: &str) -> Vec<(std::ops::Range<usize>, Option<usize>)> {
    let b = s.as_bytes();
    let (mut i, mut out) = (0, vec![]);
    let mut after_def = false;
    let is_id = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80;
    while i < b.len() {
        let c = b[i];
        let st = i;
        let kind = if c == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            Some(4)
        } else if c == b'"' || c == b'\'' || ((c | 0x20) == b'r' || (c | 0x20) == b'b' || (c | 0x20) == b'f' || (c | 0x20) == b'u') && {
            // A string prefix: one or two letters, then a quote.
            let mut j = i;
            while j < b.len() && j - i < 2 && b"rRbBfFuU".contains(&b[j]) {
                j += 1;
            }
            j < b.len() && (b[j] == b'"' || b[j] == b'\'') && (i == 0 || !is_id(b[i - 1]))
        } {
            while b[i] != b'"' && b[i] != b'\'' {
                i += 1;
            }
            let q = b[i];
            let triple = b.get(i..i + 3) == Some(&[q, q, q][..]);
            i += if triple { 3 } else { 1 };
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                } else if triple && b.get(i..i + 3) == Some(&[q, q, q][..]) {
                    i += 3;
                    break;
                } else if !triple && (b[i] == q || b[i] == b'\n') {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            i = i.min(b.len());
            Some(2)
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_' || ((b[i] == b'-' || b[i] == b'+') && matches!(b[i - 1], b'e' | b'E'))) {
                i += 1;
            }
            Some(3)
        } else if c == b'@' && (i == 0 || b[..i].iter().rev().take_while(|&&x| x != b'\n').all(|x| x.is_ascii_whitespace())) {
            i += 1;
            while i < b.len() && (is_id(b[i]) || b[i] == b'.') {
                i += 1;
            }
            Some(6)
        } else if is_id(c) && !c.is_ascii_digit() {
            while i < b.len() && is_id(b[i]) {
                i += 1;
            }
            let w = &s[st..i];
            let k = if after_def {
                Some(5)
            } else if KEYWORDS.contains(&w) {
                Some(0)
            } else if BUILTINS.contains(&w) {
                Some(1)
            } else {
                None
            };
            after_def = w == "def" || w == "class";
            out.push((st..i, k));
            continue;
        } else {
            // Other characters: up to the next character that starts a token.
            i += s[i..].chars().next().map_or(1, char::len_utf8);
            while i < b.len() && !b"#\"'@.0123456789".contains(&b[i]) && !is_id(b[i]) {
                i += 1;
            }
            None
        };
        out.push((st..i, kind));
    }
    out
}

/// The layout of Python code with the colors of the syntax.
pub fn highlight(s: &str, font: FontId, text: Color32, dark: bool) -> LayoutJob {
    let col = colors(dark);
    let mut job = LayoutJob::default();
    for (r, k) in tokens(s) {
        job.append(&s[r], 0.0, TextFormat::simple(font.clone(), k.map_or(text, |k| col[k])));
    }
    job
}

/// A code editor with Python colors.
fn code_edit(ui: &mut egui::Ui, code: &mut String, rows: usize, id: impl std::hash::Hash + std::fmt::Debug) -> egui::Response {
    let (font, text, dark) = (egui::TextStyle::Monospace.resolve(ui.style()), ui.visuals().text_color(), ui.visuals().dark_mode);
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = highlight(buf.as_str(), font.clone(), text, dark);
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    ui.add(egui::TextEdit::multiline(code).id_salt(id).code_editor().desired_width(f32::INFINITY).desired_rows(rows).layouter(&mut layouter))
}

/// What the user does in the panel (after the borrows of the panel).
enum Act {
    Run,
    RunCells(Vec<u64>),
    Stop,
    Open,
    SaveAs,
    Save,
    New(bool),
    Close(usize),
}

impl App {
    /// A command for the Python of the preferences, with the module `eoview` on its path and the address of
    /// eoview in its environment.
    pub fn py_command(&self) -> Option<std::process::Command> {
        let p = self.py.as_ref()?;
        let exe = if self.prefs.python.is_empty() { if cfg!(windows) { "python".to_string() } else { "python3".to_string() } } else { self.prefs.python.clone() };
        let dir = p.path.clone()?;
        let path = std::env::var_os("PYTHONPATH").map_or(dir.clone().into_os_string(), |old| {
            let mut v = vec![dir.clone()];
            v.extend(std::env::split_paths(&old));
            std::env::join_paths(v).unwrap_or_default()
        });
        let mut cmd = std::process::Command::new(&exe);
        cmd.env("PYTHONPATH", path).env("EOVIEW_PORT", p.port.to_string()).env("EOVIEW_TOKEN", p.token()).env("MPLBACKEND", "Agg");
        // UTF-8 on the pipes: on Windows, Python writes the ANSI code page to a pipe by default.
        cmd.env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1");
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);
        Some(cmd)
    }

    /// Open a script or a notebook in a tab of the editor (or select its tab).
    pub fn py_open(&mut self, path: &Path) {
        let Some(p) = &mut self.py else { return };
        if let Some(k) = p.docs.iter().position(|d| d.path.as_deref() == Some(path)) {
            p.cur = k;
        } else {
            match Doc::open(path) {
                Ok(d) => {
                    p.docs.push(d);
                    p.cur = p.docs.len() - 1;
                }
                Err(e) => return self.error = Some(e),
            }
        }
        p.open = true;
    }

    /// Save tab `k` to `path` (None: its file).
    pub fn py_save(&mut self, k: usize, path: Option<PathBuf>) {
        let Some(d) = self.py.as_mut().and_then(|p| p.docs.get_mut(k)) else { return };
        if let Err(e) = d.save(path) {
            self.error = Some(e);
        }
    }

    /// The information of the interpreter, made one time for each path of the preferences.
    fn py_info(&mut self) -> String {
        let exe = self.prefs.python.clone();
        let cmd = self.py_command();
        let Some(p) = &mut self.py else { return String::new() };
        let mut info = p.interp.lock().unwrap();
        if info.0 != exe || info.1.is_empty() {
            *info = (exe.clone(), t("Python: checking...").into());
            if let Some(mut cmd) = cmd {
                let (cell, ctx) = (p.interp.clone(), self.ctx.clone());
                std::thread::spawn(move || {
                    let out = cmd.arg("-c").arg(INFO).output();
                    let text = match out {
                        Ok(o) if o.status.success() => serde_json::from_slice::<Value>(&o.stdout).map_or_else(
                            |e| e.to_string(),
                            |v| {
                                let pk: Vec<String> = v["packages"].as_object().into_iter().flatten().map(|(k, x)| format!("{k} {}", x.as_str().unwrap_or("-"))).collect();
                                format!("Python {}{}  {}\n{}", v["version"].as_str().unwrap_or("?"), if v["venv"] == true { " (venv)" } else { "" }, v["exe"].as_str().unwrap_or(""), pk.join("  "))
                            },
                        ),
                        Ok(o) => String::from_utf8_lossy(&o.stderr).into_owned(),
                        Err(e) => format!("{e}. {}", t("Set the path of Python in the preferences.")),
                    };
                    cell.lock().unwrap().1 = text;
                    ctx.request_repaint();
                });
            }
        }
        info.1.clone()
    }

    /// The Python panel: the tabs, the code, Run and Stop, the outputs. The charts window.
    pub fn py_ui(&mut self, ctx: &egui::Context) {
        if !self.py.as_ref().is_some_and(|p| p.open) {
            self.py_charts(ctx);
            return;
        }
        let info = self.py_info();
        let cmd_ok = self.py_command().is_some();
        let Some(p) = &mut self.py else { return };
        // The files that changed on the disk, one time each second.
        let mut auto = false;
        if p.poll.elapsed().as_secs_f32() > 1.0 {
            p.poll = std::time::Instant::now();
            for (k, d) in p.docs.iter_mut().enumerate() {
                if d.poll() && d.auto_run && k == p.cur && !d.nb {
                    auto = true;
                }
            }
        }
        let mut acts = vec![];
        let mut open = p.open;
        egui::Window::new(t("Python")).open(&mut open).default_size([720.0, 600.0]).show(ctx, |ui| {
            // The tabs.
            ui.horizontal_wrapped(|ui| {
                for k in 0..p.docs.len() {
                    let d = &p.docs[k];
                    let name = format!("{}{}", if k == 0 { t("Workspace script").to_string() } else { d.name.clone() }, if d.dirty { " *" } else { "" });
                    if ui.selectable_label(p.cur == k, name).on_hover_text(d.path.as_ref().map_or(String::new(), |x| x.display().to_string())).clicked() {
                        p.cur = k;
                    }
                    if k > 0 && ui.small_button("x").on_hover_text(t("Close the tab")).clicked() {
                        acts.push(Act::Close(k));
                    }
                }
                ui.separator();
                ui.menu_button(t("File"), |ui| {
                    if ui.button(t("New script")).clicked() {
                        acts.push(Act::New(false));
                    }
                    if ui.button(t("New notebook")).clicked() {
                        acts.push(Act::New(true));
                    }
                    if ui.button(t("Open...")).clicked() {
                        acts.push(Act::Open);
                    }
                    if ui.button(t("Save")).clicked() {
                        acts.push(Act::Save);
                    }
                    if ui.button(t("Save as...")).clicked() {
                        acts.push(Act::SaveAs);
                    }
                });
            });
            ui.label(egui::RichText::new(&info).small().weak()).on_hover_text(t("The Python of the preferences (Preferences, Python). eoview does not include Python."));
            let cur = p.cur.min(p.docs.len() - 1);
            let busy = p.child.is_some() || p.docs[cur].busy();
            ui.horizontal(|ui| {
                let nb = p.docs[cur].nb;
                let run = if nb { t("Run all") } else { t("Run") };
                if ui.add_enabled(!busy && cmd_ok, egui::Button::new(run).shortcut_text("Ctrl+Enter")).on_hover_text(t("Run the script in the Python of the preferences")).clicked() {
                    acts.push(Act::Run);
                }
                if ui.add_enabled(busy, egui::Button::new(t("Stop"))).on_hover_text(t("Stop the process. The variables of a notebook go.")).clicked() {
                    acts.push(Act::Stop);
                }
                if busy {
                    ui.spinner();
                }
                let d = &mut p.docs[cur];
                if d.path.is_some() && !d.nb {
                    ui.checkbox(&mut d.auto_run, t("Run when the file changes")).on_hover_text(t("For a script that you edit in an other editor (VS Code, ...): eoview runs it when the editor saves it."));
                }
                if d.nb && d.kernel.is_some() && !busy && ui.button(t("Restart")).on_hover_text(t("A new Python process: the variables go.")).clicked() {
                    acts.push(Act::Stop);
                }
            });
            if nb_run_all(ui, &p.docs[cur]) {
                let ids = p.docs[cur].cells.iter().filter(|c| !c.md).map(|c| c.id).collect();
                acts.push(Act::RunCells(ids));
            }
            if cur == 0 && p.ask {
                ui.colored_label(ui.visuals().warn_fg_color, t("The workspace has this script: it makes some of its layers. Read the code, then Run."));
            }
            let d = &mut p.docs[cur];
            if d.conflict {
                ui.horizontal(|ui| {
                    ui.colored_label(ui.visuals().warn_fg_color, t("The file changed on the disk."));
                    if ui.button(t("Open the file again")).clicked() {
                        let _ = d.reload();
                    }
                    if ui.button(t("Keep this text")).clicked() {
                        d.conflict = false;
                        d.mtime = d.path.as_deref().and_then(mtime);
                    }
                });
            }
            ui.label(egui::RichText::new(tf("Notebooks and VS Code: import eoview, then eoview.connect(). Module: {}", &[&p.path.as_ref().map_or(String::new(), |d| d.display().to_string())])).small().weak());
            ui.separator();
            if d.nb {
                notebook_ui(ui, d, &mut acts);
            } else {
                let h = ui.available_height();
                egui::ScrollArea::vertical().id_salt(("code", cur)).max_height(h * 0.62).show(ui, |ui| {
                    let r = code_edit(ui, &mut d.cells[0].code, 18, ("py code", cur));
                    d.dirty |= r.changed();
                    if r.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)) {
                        acts.push(Act::Run);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().id_salt("log").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(p.log.lock().unwrap().as_str()).monospace()).wrap());
                });
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
                acts.push(Act::Save);
            }
        });
        p.open = open;
        if auto {
            acts.push(Act::Run);
        }
        for a in acts {
            self.py_act(a);
        }
        self.py_charts(ctx);
    }

    fn py_act(&mut self, a: Act) {
        let cmd = self.py_command();
        let Some(p) = &mut self.py else { return };
        let cur = p.cur.min(p.docs.len() - 1);
        match a {
            Act::Run => {
                if cur == 0 {
                    p.ask = false;
                }
                self.py_run();
            }
            Act::RunCells(ids) => {
                if let Err(e) = p.docs[cur].run_cells(&ids, || cmd) {
                    self.error = Some(e);
                }
            }
            Act::Stop => {
                if let Some(mut c) = p.child.take() {
                    let _ = c.kill();
                    p.log.lock().unwrap().push_str(&format!("--- {}\n", t("Stopped.")));
                }
                p.docs[cur].stop();
            }
            Act::New(nb) => {
                let n = (1..).find(|n| !p.docs.iter().any(|d| d.name.starts_with(&format!("untitled-{n}.")))).unwrap_or(1);
                let d = if nb { Doc::notebook(&format!("untitled-{n}.ipynb")) } else { Doc::script(&format!("untitled-{n}.py"), "import eoview as ev\n\n".into()) };
                p.docs.push(d);
                p.cur = p.docs.len() - 1;
            }
            Act::Open => self.dialog = Some(crate::app::Dialog::PyOpen),
            Act::SaveAs => self.dialog = Some(crate::app::Dialog::PySave(cur)),
            Act::Save if p.docs[cur].path.is_none() && cur > 0 => self.dialog = Some(crate::app::Dialog::PySave(cur)),
            Act::Save if cur == 0 => {
                // The workspace script stays in the workspace file, and in the configuration directory.
                if let Some(dir) = &p.path {
                    let _ = std::fs::write(dir.join("script.py"), &p.docs[0].cells[0].code);
                }
                p.docs[0].dirty = false;
            }
            Act::Save => self.py_save(cur, None),
            Act::Close(k) => {
                if p.docs[k].dirty && !p.close_ask.replace(k).is_some_and(|x| x == k) {
                    self.error = Some(tf("{} has changes that are not saved. Close again to close it.", &[&p.docs[k].name]));
                    return;
                }
                p.close_ask = None;
                p.docs.remove(k);
                p.cur = p.cur.min(p.docs.len() - 1);
            }
        }
    }

    /// The charts of the scripts.
    fn py_charts(&mut self, ctx: &egui::Context) {
        let Some(p) = &mut self.py else { return };
        if !p.charts_open || p.charts.is_empty() {
            return;
        }
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
}

/// Ctrl+Enter in a notebook with no cell in focus: run all cells.
fn nb_run_all(ui: &egui::Ui, d: &Doc) -> bool {
    d.nb && !d.busy() && ui.memory(|m| m.focused().is_none()) && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
}

/// The cells of a notebook: code (with its output) or markdown. Ctrl+Enter runs the cell in focus.
fn notebook_ui(ui: &mut egui::Ui, d: &mut Doc, acts: &mut Vec<Act>) {
    let outs = d.outs.lock().unwrap().clone();
    let running = d.kernel.as_ref().and_then(|k| *k.running.lock().unwrap());
    let busy = d.busy();
    let mut edit: Option<(usize, i32)> = None;
    if let Some(m) = outs.get(&u64::MAX).filter(|m| !m.is_empty()) {
        ui.colored_label(ui.visuals().warn_fg_color, egui::RichText::new(m).monospace().small());
    }
    egui::ScrollArea::vertical().id_salt("cells").auto_shrink([false, false]).show(ui, |ui| {
        for k in 0..d.cells.len() {
            let c = &mut d.cells[k];
            ui.horizontal(|ui| {
                if !c.md && ui.add_enabled(!busy || running != Some(c.id), egui::Button::new("▶").small()).on_hover_text(t("Run the cell (Ctrl+Enter)")).clicked() {
                    acts.push(Act::RunCells(vec![c.id]));
                }
                ui.weak(if c.md { "md".to_string() } else { format!("[{}]", k + 1) });
                if running == Some(c.id) {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("x").on_hover_text(t("Remove the cell")).clicked() {
                        edit = Some((k, -1));
                    }
                    if ui.small_button("+").on_hover_text(t("A new cell below")).clicked() {
                        edit = Some((k, 1));
                    }
                    if ui.small_button(if c.md { "code" } else { "md" }).on_hover_text(t("Code or text")).clicked() {
                        edit = Some((k, 0));
                    }
                });
            });
            let r = if c.md {
                ui.add(egui::TextEdit::multiline(&mut c.code).id_salt(("md", c.id)).desired_width(f32::INFINITY).desired_rows(1))
            } else {
                let rows = c.code.lines().count().max(1);
                code_edit(ui, &mut c.code, rows, ("cell", c.id))
            };
            d.dirty |= r.changed();
            if r.has_focus() && !c.md && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)) {
                acts.push(Act::RunCells(vec![c.id]));
            }
            if let Some(o) = outs.get(&c.id).filter(|o| !o.is_empty()) {
                egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(4.0).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(o.trim_end()).monospace().small()).wrap());
                });
            }
            ui.add_space(6.0);
        }
        if ui.button(t("+ Cell")).clicked() {
            edit = Some((d.cells.len(), 1));
        }
    });
    match edit {
        Some((k, 1)) => {
            d.cells.insert((k + 1).min(d.cells.len()), Cell { id: cell_id(), code: String::new(), md: false });
            d.dirty = true;
        }
        Some((k, -1)) => {
            d.cells.remove(k);
            if d.cells.is_empty() {
                d.cells.push(Cell { id: cell_id(), code: String::new(), md: false });
            }
            d.dirty = true;
        }
        Some((k, _)) => {
            d.cells[k].md ^= true;
            d.dirty = true;
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_tokens() {
        let s = "def f(x):  # c\n    return 'a' + f\"b{x}\" + 1.5e-3 + len(x)\n@ev.tool\n\"\"\"doc\nmore\"\"\"";
        let kinds: Vec<(&str, usize)> = tokens(s).into_iter().filter_map(|(r, k)| Some((&s[r], k?))).collect();
        assert_eq!(kinds, [("def", 0), ("f", 5), ("# c", 4), ("return", 0), ("'a'", 2), ("f\"b{x}\"", 2), ("1.5e-3", 3), ("len", 1), ("@ev.tool", 6), ("\"\"\"doc\nmore\"\"\"", 2)]);
        // All the text is in the tokens, in order.
        assert_eq!(tokens(s).iter().map(|(r, _)| &s[r.clone()]).collect::<String>(), s);
    }

    /// A file that an other editor changes opens again. With changes in the tab: a conflict.
    #[test]
    fn file_changes_on_the_disk() {
        let f = std::env::temp_dir().join(format!("eoview-edit-{}.py", std::process::id()));
        let touch = |text: &str, secs: u64| {
            std::fs::write(&f, text).unwrap();
            std::fs::File::options().write(true).open(&f).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
        };
        touch("a = 1\n", 1000);
        let mut d = Doc::open(&f).unwrap();
        assert!(!d.poll());
        touch("a = 2\n", 2000);
        assert!(d.poll() && d.cells[0].code == "a = 2\n");
        d.cells[0].code.push_str("b = 3\n");
        d.dirty = true;
        touch("a = 4\n", 3000);
        assert!(!d.poll() && d.conflict && d.cells[0].code == "a = 2\nb = 3\n");
        d.save(None).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "a = 2\nb = 3\n");
        assert!(!d.conflict && !d.dirty && !d.poll());
        std::fs::remove_file(&f).ok();
    }

    /// A line in cp1252 (Windows) does not stop the reading of the next lines.
    #[test]
    fn lines_that_are_not_utf8() {
        let mut v = vec![];
        each_line(&b"France: 37.3 \xb0C\r\nnext line\nend"[..], |l| v.push(l.to_string()));
        assert_eq!(v, ["France: 37.3 \u{fffd}C", "next line", "end"]);
    }

    #[test]
    fn notebook_round_trip() {
        let nb = r##"{"cells": [{"cell_type": "markdown", "metadata": {}, "source": ["# Title\n", "text"]},
            {"cell_type": "code", "execution_count": 3, "metadata": {}, "outputs": [{"output_type": "stream", "name": "stdout", "text": ["4\n"]}], "source": "print(2 + 2)"}],
            "metadata": {"kernelspec": {"name": "python3"}}, "nbformat": 4, "nbformat_minor": 5}"##;
        let mut d = Doc::script("a.ipynb", String::new());
        d.path = Some("a.ipynb".into());
        d.load(nb).unwrap();
        assert!(d.nb && d.cells.len() == 2 && d.cells[0].md && d.cells[0].code == "# Title\ntext" && d.cells[1].code == "print(2 + 2)");
        assert_eq!(d.outs.lock().unwrap().get(&d.cells[1].id).map(String::as_str), Some("4\n"));
        let v: Value = serde_json::from_str(&d.text()).unwrap();
        assert_eq!(v["metadata"]["kernelspec"]["name"], "python3");
        assert_eq!(v["cells"][1]["outputs"][0]["text"][0], "4\n");
        assert_eq!(source(&v["cells"][0]["source"]), "# Title\ntext");
    }
}
