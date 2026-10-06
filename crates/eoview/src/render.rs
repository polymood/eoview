//! Render of a view to a video file or to PNG files: one frame for each time step, at a set size.
//!
//! The render is for data that is too large for the real time: a frame waits for all its tiles, then the
//! render goes to the next step. Only the data of one step (and of the next steps) is in the memory.
//! `ffmpeg` makes the video file: the frames go to its standard input as raw pixels.
//!
//! The render draws a copy of the view (a temporary view that is not in the dock) into an offscreen
//! target, with its own egui context.

use crate::app::{App, Dialog};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// File extensions of the outputs that `ffmpeg` writes. An other output is a directory for PNG files.
pub const VIDEO_EXT: &[&str] = &["mp4", "mov", "mkv", "webm", "avi", "gif"];

/// Settings of a render. A project file keeps them.
#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    /// Size of the frames in pixels.
    pub width: u32,
    pub height: u32,
    /// Frames for each second of the video.
    pub fps: f32,
    /// Time steps of the layer with a timeline: the first step, the last step (None: the last step of
    /// the layer) and the interval between two frames, in steps.
    pub first: usize,
    pub last: Option<usize>,
    pub stride: usize,
    /// Frames for each time step. More than 1: the frames between two steps are a blend of the steps,
    /// for a smooth change.
    pub sub: usize,
    /// A video file (see `VIDEO_EXT`), or a directory for PNG files.
    pub out: String,
    /// Write the time of the step on each frame.
    pub stamp: bool,
    /// The preview of the animate workspace has 1 / `proxy` of the size of the frames (2, 4, 8 or 16).
    pub proxy: u32,
    /// Text of the legend of the frames. Empty: the name of the layer and its unit.
    pub legend: String,
    /// Keep the frames as PNG files next to a video output, and make the video at the end. A render that
    /// stopped then continues after its last frame.
    pub keep: bool,
    /// The frames show all the data. Else they show the view as it is at the start of the render.
    pub fit: bool,
    /// Camera: the center of the view and the width of the view, in display units. It does not depend on
    /// the size of the frames. None: the frame shows all the data.
    pub view: Option<([f64; 2], f64)>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { width: 1920, height: 1080, fps: 24.0, first: 0, last: None, stride: 1, sub: 1, out: "eoview.mp4".into(), stamp: true, proxy: 4, legend: String::new(), keep: false, fit: false, view: None }
    }
}

impl Settings {
    /// Steps of the frames for a layer with `n` time steps (1: a layer without a timeline).
    pub fn steps(&self, n: usize) -> Vec<usize> {
        let last = self.last.unwrap_or(usize::MAX).min(n.saturating_sub(1));
        (self.first..=last).step_by(self.stride.max(1)).collect()
    }

    fn video(&self) -> bool {
        Path::new(&self.out).extension().and_then(|e| e.to_str()).is_some_and(|e| VIDEO_EXT.contains(&e.to_lowercase().as_str()))
    }
}

/// A command without a console window (Windows) and without input.
fn quiet(c: &mut Command) -> &mut Command {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(c, 0x0800_0000);
    c.stdin(Stdio::null())
}

/// The `ffmpeg` program: the path of the preferences, then the directory of the executable, then the
/// search path of the system. None: no `ffmpeg` that runs.
pub fn ffmpeg(pref: &str) -> Option<PathBuf> {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let beside = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join(name)));
    let list = [Some(PathBuf::from(pref)).filter(|_| !pref.is_empty()), beside, Some(PathBuf::from(name))];
    list.into_iter().flatten().find(|p| quiet(&mut Command::new(p)).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success()))
}

/// Where the frames go.
enum Sink {
    /// `ffmpeg`, with the frames on its standard input.
    Video(Child),
    /// PNG files in this directory. At the end, `ffmpeg` (its path) makes the video from them.
    Images(PathBuf, Option<PathBuf>),
}

/// Offscreen target of the frames, and the buffer that brings the pixels back from the GPU.
struct Target {
    view: wgpu::TextureView,
    texture: wgpu::Texture,
    buffer: wgpu::Buffer,
    /// Bytes of one row in the buffer (a multiple of 256).
    row: u32,
    /// The pixels are B, G, R, A (else R, G, B, A).
    bgra: bool,
}

/// An offscreen target of `w` x `h` pixels, with its buffer for the pixels.
fn target(device: &wgpu::Device, format: wgpu::TextureFormat, w: u32, h: u32) -> Target {
    let row = (w * 4).div_ceil(256) * 256;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        // TEXTURE_BINDING: the interface shows the target (the last frame, the preview).
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("render frame"),
        size: row as u64 * h as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let bgra = matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
    Target { view: texture.create_view(&Default::default()), texture, buffer, row, bgra }
}

/// The preview of the animate workspace: the scene at a part of the size of the frames, drawn with the
/// code of the frames of a render. The viewport shows its target as a texture.
pub struct Preview {
    target: Target,
    ctx: egui::Context,
    egui: egui_wgpu::Renderer,
    pub tex: egui::TextureId,
    pub w: u32,
    pub h: u32,
}

pub struct Job {
    /// The temporary view of the render.
    pub pane: u32,
    pub set: Settings,
    steps: Vec<usize>,
    /// Number of frames that are done.
    pub done: usize,
    /// The view is at the step of the next frame.
    stepped: bool,
    target: Target,
    ctx: egui::Context,
    egui: egui_wgpu::Renderer,
    sink: Sink,
    pub t0: Instant,
    /// Time of the last progress line of `eoview --render`.
    pub said: Instant,
    /// The offscreen target as a texture of the main window, for the render window.
    preview: Option<egui::TextureId>,
    /// The output is not the one of the settings (no `ffmpeg`): where the frames go, and why.
    pub note: Option<String>,
}

impl Job {
    /// Number of frames: `sub` for each step, and one for the last step.
    pub fn frames(&self) -> usize {
        frames(self.steps.len(), self.set.sub)
    }
}

/// Number of frames of `steps` time steps with `sub` frames for each step.
pub fn frames(steps: usize, sub: usize) -> usize {
    if steps == 0 { 0 } else { (steps - 1) * sub.max(1) + 1 }
}

fn err<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// Open the output. Without `ffmpeg`, a video output becomes PNG files in a directory next to it.
fn sink(set: &Settings, w: u32, h: u32, bgra: bool, pref: &str) -> Result<(Sink, Option<String>), String> {
    let images = |dir: PathBuf, exe, note| std::fs::create_dir_all(&dir).map(|_| (Sink::Images(dir, exe), note)).map_err(err(&set.out));
    if !set.video() {
        return images(PathBuf::from(&set.out), None, None);
    }
    let frames = Path::new(&set.out).with_extension("frames");
    let Some(exe) = ffmpeg(pref) else {
        let note = format!("ffmpeg was not found: the frames are PNG files in {}. Set the path of ffmpeg in the preferences.", frames.display());
        return images(frames, None, Some(note));
    };
    if set.keep {
        return images(frames, Some(exe), None);
    }
    if let Some(d) = Path::new(&set.out).parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d).map_err(err(&set.out))?;
    }
    let mut c = Command::new(exe);
    c.args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", if bgra { "bgra" } else { "rgba" }]);
    c.args(["-s", &format!("{w}x{h}"), "-r", &format!("{}", set.fps), "-i", "-", "-an"]);
    c.args(encoder(&set.out)).arg(&set.out);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000);
    let child = c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(err("ffmpeg"))?;
    Ok((Sink::Video(child), None))
}

/// Arguments of `ffmpeg` for the video encoder of an output file.
fn encoder(out: &str) -> Vec<&'static str> {
    let ext = Path::new(out).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    // H.264 with 4:2:0 colors plays in all players. The other containers use the encoder that ffmpeg selects.
    if ["mp4", "mov", "mkv"].contains(&ext.as_str()) { vec!["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18", "-preset", "medium", "-movflags", "+faststart"] } else { vec![] }
}

/// Number of frames that are in `dir` from the first frame, without a gap.
fn frames_done(dir: &Path) -> usize {
    (1..).take_while(|i| dir.join(format!("frame_{i:05}.png")).is_file()).count()
}

/// Write a PNG file. The file has its name only when it is complete: a render that stops does not
/// leave a part of a frame.
fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let part = path.with_extension("part");
    write_png_to(&part, w, h, rgba)?;
    std::fs::rename(&part, path).map_err(err(&path.to_string_lossy()))
}

fn write_png_to(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(err(&path.to_string_lossy()))?;
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().and_then(|mut wr| wr.write_image_data(rgba)).map_err(err(&path.to_string_lossy()))
}

impl App {
    /// Start a render of view `src`. The render draws a copy of the view: the view stays as it is.
    pub fn start_render(&mut self, src: u32, set: Settings) -> Result<(), String> {
        self.cancel_render();
        let Some(win) = &self.win else { return Err("the GPU did not start".into()) };
        let p = self.pane(src).ok_or("no view")?;
        if p.layers.is_empty() {
            return Err("the view has no layer".into());
        }
        let steps = set.steps(p.timed().map_or(1, |l| l.steps.len()));
        if steps.is_empty() {
            return Err("no time step in the range".into());
        }
        // The video encoders need even sizes.
        let (w, h) = ((set.width & !1).max(16), (set.height & !1).max(16));
        let (device, format) = (win.gpu.device.clone(), win.gpu_format());
        if w.max(h) > device.limits().max_texture_dimension_2d {
            return Err(format!("the GPU cannot draw frames of {w} x {h} pixels"));
        }
        let target = target(&device, format, w, h);
        let bgra = target.bgra;
        let egui = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        let (sink, note) = sink(&set, w, h, bgra, &self.prefs.ffmpeg)?;

        let pane = self.copy_pane(src);
        let stride = set.stride.max(1);
        if let Some(p) = self.pane_mut(pane) {
            p.link = 0;
            p.play = false;
            let blend = set.sub > 1 && steps.len() > 1;
            p.layers.iter_mut().for_each(|l| (l.stride, l.blend) = (stride, blend));
            match set.view {
                Some((center, width)) if width > 0.0 => (p.v.center, p.v.scale, p.v.fit) = (center, w as f64 / width, false),
                _ => p.v.fit = true,
            }
        }
        // The main window does not draw this view, and no window opens for it (`reconcile`).
        self.floating.push(pane);
        let set = Settings { width: w, height: h, ..set };
        let done = match &sink {
            Sink::Images(dir, Some(_)) => frames_done(dir).min(frames(steps.len(), set.sub)),
            _ => 0,
        };
        self.job = Some(Job { pane, set, steps, done, stepped: false, target, ctx: egui::Context::default(), egui, sink, t0: Instant::now(), said: Instant::now(), preview: None, note });
        self.render_msg = None;
        Ok(())
    }

    /// Stop the render now. The frames that are done stay in the output.
    pub fn cancel_render(&mut self) {
        if self.job.is_some() {
            let n = self.job.as_ref().map_or(0, |j| j.done);
            self.end_render(Some(format!("Stopped after {n} frame(s)")));
        }
    }

    /// Close the output and remove the temporary view. `msg`: the result for the user, if it is not "done".
    fn end_render(&mut self, msg: Option<String>) {
        let Some(job) = self.job.take() else { return };
        let (n, secs) = (job.done, job.t0.elapsed().as_secs_f64());
        let mut text = msg.unwrap_or_else(|| format!("{n} frame(s) in {secs:.1} s: {}", job.set.out));
        match job.sink {
            Sink::Video(mut child) => {
                // The end of the input tells ffmpeg to complete the file.
                drop(child.stdin.take());
                match child.wait_with_output() {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => text = format!("ffmpeg: {}", String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => text = format!("ffmpeg: {e}"),
                }
            }
            // The video from the kept frames, if the render made all of them.
            Sink::Images(dir, Some(exe)) if n >= job.frames() => {
                let mut c = Command::new(exe);
                c.args(["-y", "-loglevel", "error", "-framerate", &format!("{}", job.set.fps), "-i"]).arg(dir.join("frame_%05d.png")).arg("-an");
                c.args(encoder(&job.set.out)).arg(&job.set.out);
                match quiet(&mut c).stdout(Stdio::null()).stderr(Stdio::piped()).output() {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => text = format!("ffmpeg: {}", String::from_utf8_lossy(&o.stderr).trim()),
                    Err(e) => text = format!("ffmpeg: {e}"),
                }
            }
            Sink::Images(dir, Some(_)) => text = format!("{text}\nThe frames are in {}: the next render continues after them.", dir.display()),
            Sink::Images(_, None) => {}
        }
        if let Some(note) = job.note {
            text = format!("{text}\n{note}");
        }
        if let (Some(id), Some(w)) = (job.preview, &mut self.win) {
            w.egui.free_texture(&id);
        }
        self.floating.retain(|&f| f != job.pane);
        self.engine.want(job.pane, vec![]);
        self.panes.retain(|p| p.id != job.pane);
        self.render_msg = Some(text);
    }

    /// Do the work of the render for a short time: set the step of the next frame, draw it, and write it
    /// if all its data is there. Else the frame waits: the engine wakes the application when data comes.
    /// Return true if a render ended in this call.
    pub fn render_tick(&mut self) -> bool {
        if self.job.is_none() {
            return false;
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(40) {
            self.events();
            let Some(job) = &mut self.job else { return true };
            let pane = job.pane;
            if job.done >= job.frames() {
                self.end_render(None);
                return true;
            }
            if !job.stepped {
                // Frame `f` of `sub` between step `s` and the next step.
                let sub = job.set.sub.max(1);
                let (s, f) = (job.steps[job.done / sub], job.done % sub);
                job.stepped = true;
                if f == 0 {
                    self.set_time(pane, s);
                }
                if let Some(p) = self.pane_mut(pane) {
                    p.tmix = f as f32 / sub as f32;
                }
            }
            self.draw_frame();
            if !self.frame_ready(pane) {
                return false;
            }
            if let Err(e) = self.write_frame() {
                self.end_render(Some(e));
                return true;
            }
            if let Some(job) = &mut self.job {
                (job.done, job.stepped) = (job.done + 1, false);
            }
        }
        false
    }

    /// Draw the temporary view into the offscreen target.
    fn draw_frame(&mut self) {
        // A new GPU frame: the tile array can replace the tiles of the frames before. Without this, the
        // tiles of all the steps stay, and the array is full after some thousand steps.
        if let Some(w) = &mut self.win {
            w.gpu.frame += 1;
        }
        let (Some(win), Some(job)) = (&self.win, &self.job) else { return };
        let (device, queue) = (win.gpu.device.clone(), win.gpu.queue.clone());
        let (ctx, pane, w, h, stamp, legend) = (job.ctx.clone(), job.pane, job.set.width, job.set.height, job.set.stamp, job.set.legend.clone());
        // The text and the lines have the same size on a 1080 pixel high frame as on a screen.
        let ppp = h as f32 / 1080.0;
        ctx.set_zoom_factor(ppp);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32 / ppp, h as f32 / ppp))),
            time: Some(job.done as f64 / job.set.fps.max(0.01) as f64),
            ..Default::default()
        };
        let out = ctx.run_ui(raw, |ui| self.export_ui(ui, pane, [w, h], stamp, &legend));
        let Some(job) = &mut self.job else { return };
        crate::draw_egui(&device, &queue, &mut job.egui, &job.target.view, &ctx, out, [w, h]);
    }

    /// Bring the pixels of the offscreen target back from the GPU, and write them to the output.
    fn write_frame(&mut self) -> Result<(), String> {
        let (Some(win), Some(job)) = (&self.win, &mut self.job) else { return Ok(()) };
        let (w, h, t) = (job.set.width, job.set.height, &job.target);
        let mut px = read_pixels(&win.gpu.device, &win.gpu.queue, &t.texture, &t.buffer, t.row, w, h)?;
        match &mut job.sink {
            Sink::Video(child) => child.stdin.as_mut().ok_or("ffmpeg has no input")?.write_all(&px).map_err(err("ffmpeg")),
            Sink::Images(dir, _) => {
                to_rgba(&mut px, t.bgra);
                write_png(&dir.join(format!("frame_{:05}.png", job.done + 1)), w, h, &px)
            }
        }
    }

    /// The texture of the last frame of the render that runs, for the interface.
    pub fn job_texture(&mut self) -> Option<egui::TextureId> {
        let (Some(j), Some(w)) = (&mut self.job, &mut self.win) else { return None };
        if j.preview.is_none() {
            j.preview = Some(w.egui.register_native_texture(&w.gpu.device, &j.target.view, wgpu::FilterMode::Linear));
        }
        j.preview
    }

    /// Draw the preview of the animate workspace: the scene view at 1 / `proxy` of the size of the frames.
    /// The camera is the camera of the settings (a center and a width in display units).
    pub fn preview_frame(&mut self) {
        let Some(id) = self.scene_pane().filter(|_| self.job.is_none()) else { return };
        let Some(win) = &mut self.win else { return };
        let (device, queue, format) = (win.gpu.device.clone(), win.gpu.queue.clone(), win.gpu_format());
        let k = self.render_set.proxy.clamp(1, 16);
        let (w, h) = (((self.render_set.width / k) & !1).max(16), ((self.render_set.height / k) & !1).max(16));
        if self.preview.as_ref().is_none_or(|p| (p.w, p.h) != (w, h)) {
            if let Some(old) = self.preview.take() {
                win.egui.free_texture(&old.tex);
            }
            let target = target(&device, format, w, h);
            let tex = win.egui.register_native_texture(&device, &target.view, wgpu::FilterMode::Linear);
            let egui = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
            self.preview = Some(Preview { target, ctx: egui::Context::default(), egui, tex, w, h });
        }
        win.gpu.frame += 1;
        let (stamp, view, t) = (self.render_set.stamp, self.render_set.view, self.ctx.input(|i| i.time));
        if let (Some((center, width)), Some(p)) = (view.filter(|v| v.1 > 0.0), self.pane_mut(id)) {
            (p.v.center, p.v.scale) = (center, w as f64 / width);
        }
        let Some(pv) = &self.preview else { return };
        let ctx = pv.ctx.clone();
        // The text of a small preview stays readable.
        let ppp = (h as f32 / 1080.0).max(0.3);
        ctx.set_zoom_factor(ppp);
        let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32 / ppp, h as f32 / ppp))), time: Some(t), ..Default::default() };
        let legend = self.render_set.legend.clone();
        let out = ctx.run_ui(raw, |ui| self.export_ui(ui, id, [w, h], stamp, &legend));
        let Some(pv) = &mut self.preview else { return };
        crate::draw_egui(&device, &queue, &mut pv.egui, &pv.target.view, &ctx, out, [w, h]);
        // Without a camera in the settings (the start, or "Fit"): the camera of the view after its fit.
        if view.is_none()
            && let Some(p) = self.pane(id).filter(|p| !p.v.fit && p.has_warp() && p.v.scale > 0.0)
        {
            self.render_set.view = Some((p.v.center, w as f64 / p.v.scale));
        }
    }

    /// Start the render of view `src` with the settings of the render window.
    pub fn render_start(&mut self, src: u32) {
        let mut set = self.render_set.clone();
        let cam = self.pane(src).filter(|p| p.v.px.width() > 1.0 && p.v.scale > 0.0).map(|p| (p.v.center, p.v.px.width() as f64 / p.v.scale));
        set.view = cam.filter(|_| !set.fit);
        // The project file keeps the camera of the last render.
        self.render_set.view = set.view;
        if let Err(e) = self.start_render(src, set) {
            self.render_msg = Some(e);
        }
    }

    /// Write the frame of the main window to a PNG file (`eoview --shot`).
    pub fn screenshot(&self, path: &str) -> Result<(), String> {
        let win = self.win.as_ref().ok_or("no window")?;
        let texture = win.offscreen_texture().ok_or("no offscreen target")?;
        let (w, h) = (texture.width(), texture.height());
        let row = (w * 4).div_ceil(256) * 256;
        let buffer = win.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot"),
            size: row as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut px = read_pixels(&win.gpu.device, &win.gpu.queue, texture, &buffer, row, w, h)?;
        to_rgba(&mut px, matches!(win.gpu_format(), wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb));
        write_png(Path::new(path), w, h, &px)
    }
}

/// Pixels of `texture` (`w` x `h`, 4 bytes for each pixel) from the GPU, through `buffer` (rows of `row` bytes).
fn read_pixels(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, buffer: &wgpu::Buffer, row: u32, w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit([enc.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(err("GPU"))?;
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    {
        let data = slice.get_mapped_range().map_err(err("GPU"))?;
        for r in 0..h as usize {
            let at = r * row as usize;
            px.extend_from_slice(&data[at..at + w as usize * 4]);
        }
    }
    buffer.unmap();
    Ok(px)
}

/// Pixels for a PNG file: red, green, blue, and no transparency.
fn to_rgba(px: &mut [u8], bgra: bool) {
    for p in px.chunks_exact_mut(4) {
        if bgra {
            p.swap(0, 2);
        }
        p[3] = 255;
    }
}

/// Frame sizes of the render window.
pub const SIZES: &[(&str, u32, u32)] = &[("1280 x 720 (720p)", 1280, 720), ("1920 x 1080 (1080p)", 1920, 1080), ("2560 x 1440 (1440p)", 2560, 1440), ("3840 x 2160 (4K)", 3840, 2160)];

impl App {
    /// Render window: the settings of the render of the active view, the progress and the last frame.
    pub fn render_ui(&mut self, ctx: &egui::Context) {
        if !self.render_open {
            return;
        }
        let src = self.active;
        let (title, n) = self.pane(src).map_or((String::new(), 1), |p| (p.title(), p.timed().map_or(1, |l| l.steps.len())));
        let label = |s: usize| self.pane(src).and_then(|p| p.timed()).map_or(String::new(), |l| l.step_label(s));
        let (first, last) = (self.render_set.first.min(n - 1), self.render_set.last.unwrap_or(n - 1).min(n - 1));
        let (first_label, last_label) = (label(first), label(last));
        if self.ffmpeg_found.is_none() {
            self.ffmpeg_found = Some(ffmpeg(&self.prefs.ffmpeg).is_some());
        }
        let no_ffmpeg = self.ffmpeg_found == Some(false) && self.render_set.video();
        self.job_texture();
        let progress = self.job.as_ref().map(|j| (j.done, j.frames(), j.t0.elapsed().as_secs_f32(), j.preview, j.set.width as f32 / j.set.height as f32));
        let (mut open, mut start, mut stop, mut browse) = (true, false, false, false);
        egui::Window::new("Render").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.set_width(440.0);
            let set = &mut self.render_set;
            ui.add_enabled_ui(progress.is_none(), |ui| {
                egui::Grid::new("render").num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
                    ui.label("View");
                    ui.label(&title);
                    ui.end_row();

                    ui.label("Size");
                    ui.horizontal(|ui| {
                        let cur = SIZES.iter().find(|x| (x.1, x.2) == (set.width, set.height)).map_or("Custom", |x| x.0);
                        egui::ComboBox::from_id_salt("render size").selected_text(cur).show_ui(ui, |ui| {
                            for (name, w, h) in SIZES {
                                if ui.selectable_label((set.width, set.height) == (*w, *h), *name).clicked() {
                                    (set.width, set.height) = (*w, *h);
                                }
                            }
                        });
                        ui.add(egui::DragValue::new(&mut set.width).range(16..=16384));
                        ui.label("x");
                        ui.add(egui::DragValue::new(&mut set.height).range(16..=16384));
                    });
                    ui.end_row();

                    ui.label("Rate");
                    ui.add(egui::DragValue::new(&mut set.fps).range(1.0..=120.0).speed(0.2).suffix(" frames/s"));
                    ui.end_row();

                    // The interface counts the steps from 1.
                    ui.label("First step");
                    ui.horizontal(|ui| {
                        let mut v = first + 1;
                        if ui.add(egui::DragValue::new(&mut v).range(1..=n)).changed() {
                            set.first = v - 1;
                        }
                        ui.weak(&first_label);
                    });
                    ui.end_row();
                    ui.label("Last step");
                    ui.horizontal(|ui| {
                        let mut v = last + 1;
                        if ui.add(egui::DragValue::new(&mut v).range(1..=n)).changed() {
                            set.last = Some(v - 1).filter(|l| l + 1 < n);
                        }
                        ui.weak(&last_label);
                    });
                    ui.end_row();
                    ui.label("Interval");
                    ui.add(egui::DragValue::new(&mut set.stride).range(1..=n.max(1)).suffix(" step(s)"));
                    ui.end_row();
                    ui.label("Frames for a step");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut set.sub).range(1..=120)).on_hover_text("More than 1: the frames between two steps are a blend of the two steps, for a smooth change");
                        let frames = frames(set.steps(n).len(), set.sub);
                        ui.weak(format!("{frames} frames, {:.1} s of video", frames as f32 / set.fps.max(0.01)));
                    });
                    ui.end_row();

                    ui.label("Frame");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut set.fit, false, "The view as it is now");
                        ui.radio_value(&mut set.fit, true, "All the data");
                    });
                    ui.end_row();

                    ui.label("Time");
                    ui.checkbox(&mut set.stamp, "Write the time of the step on the frames");
                    ui.end_row();

                    ui.label("Output");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut set.out).desired_width(280.0)).on_hover_text("A video file (mp4, mov, mkv, webm, gif), or a directory for PNG files");
                        browse = ui.button("Browse...").clicked();
                    });
                    ui.end_row();
                });
            });
            if no_ffmpeg {
                ui.colored_label(egui::Color32::from_rgb(225, 165, 40), "ffmpeg was not found: the frames will be PNG files. Set the path of ffmpeg in the preferences.");
            }
            ui.separator();
            match progress {
                Some((done, frames, secs, preview, aspect)) => {
                    let rate = done as f32 / secs.max(1e-3);
                    let left = if rate > 0.0 { format!("{:.0} s left", (frames - done) as f32 / rate) } else { "waits for data".into() };
                    ui.add(egui::ProgressBar::new(done as f32 / frames.max(1) as f32).text(format!("Frame {done} of {frames}   {rate:.1} frames/s   {left}")));
                    if let Some(id) = preview {
                        let w = ui.available_width();
                        ui.image(egui::load::SizedTexture::new(id, egui::vec2(w, w / aspect)));
                    }
                    stop = ui.button("Stop").clicked();
                }
                None => {
                    start = ui.add_enabled(n >= 1 && !title.is_empty(), egui::Button::new("Render")).clicked();
                    if let Some(m) = &self.render_msg {
                        ui.label(m);
                    }
                }
            }
        });
        if start {
            self.render_start(src);
        }
        if stop {
            self.cancel_render();
        }
        if browse {
            self.dialog = Some(Dialog::RenderOut);
        }
        self.render_open = open;
    }
}
