//! Render of a view to a video file or to PNG files: one frame for each time step, at a set size.
//!
//! The render is for data that is too large for the real time: a frame waits for all its tiles, then the
//! render goes to the next step. Only the data of one step (and of the next steps) is in the memory.
//! `ffmpeg` makes the video file: the frames go to its standard input as raw pixels.
//!
//! The render draws a copy of the view (a temporary view that is not in the dock) into an offscreen
//! target, with its own egui context.

use crate::app::App;
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
    /// A video file (see `VIDEO_EXT`), or a directory for PNG files.
    pub out: String,
    /// Write the time of the step on each frame.
    pub stamp: bool,
    /// Camera: the center of the view and the width of the view, in display units. It does not depend on
    /// the size of the frames. None: the frame shows all the data.
    pub view: Option<([f64; 2], f64)>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { width: 1920, height: 1080, fps: 24.0, first: 0, last: None, stride: 1, out: "eoview.mp4".into(), stamp: true, view: None }
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
    /// PNG files in this directory.
    Images(PathBuf),
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
    /// The output is not the one of the settings (no `ffmpeg`): where the frames go, and why.
    pub note: Option<String>,
}

impl Job {
    pub fn frames(&self) -> usize {
        self.steps.len()
    }
}

fn err<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> String + '_ {
    move |e| format!("{what}: {e}")
}

/// Open the output. Without `ffmpeg`, a video output becomes PNG files in a directory next to it.
fn sink(set: &Settings, w: u32, h: u32, bgra: bool, pref: &str) -> Result<(Sink, Option<String>), String> {
    let images = |dir: PathBuf, note| std::fs::create_dir_all(&dir).map(|_| (Sink::Images(dir), note)).map_err(err(&set.out));
    if !set.video() {
        return images(PathBuf::from(&set.out), None);
    }
    let Some(exe) = ffmpeg(pref) else {
        let dir = Path::new(&set.out).with_extension("frames");
        let note = format!("ffmpeg was not found: the frames are PNG files in {}. Set the path of ffmpeg in the preferences.", dir.display());
        return images(dir, Some(note));
    };
    if let Some(d) = Path::new(&set.out).parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d).map_err(err(&set.out))?;
    }
    let ext = Path::new(&set.out).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let mut c = Command::new(exe);
    c.args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", if bgra { "bgra" } else { "rgba" }]);
    c.args(["-s", &format!("{w}x{h}"), "-r", &format!("{}", set.fps), "-i", "-", "-an"]);
    // H.264 with 4:2:0 colors plays in all players. The other containers use the encoder that ffmpeg selects.
    if ext == "mp4" || ext == "mov" || ext == "mkv" {
        c.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18", "-preset", "medium", "-movflags", "+faststart"]);
    }
    c.arg(&set.out);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000);
    let child = c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().map_err(err("ffmpeg"))?;
    Ok((Sink::Video(child), None))
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
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
        let row = (w * 4).div_ceil(256) * 256;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("render"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("render frame"),
            size: row as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bgra = matches!(format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
        let target = Target { view: texture.create_view(&Default::default()), texture, buffer, row, bgra };
        let egui = egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default());
        let (sink, note) = sink(&set, w, h, bgra, &self.prefs.ffmpeg)?;

        let pane = self.copy_pane(src);
        let stride = set.stride.max(1);
        if let Some(p) = self.pane_mut(pane) {
            p.link = 0;
            p.play = false;
            p.layers.iter_mut().for_each(|l| l.stride = stride);
            match set.view {
                Some((center, width)) if width > 0.0 => (p.v.center, p.v.scale, p.v.fit) = (center, w as f64 / width, false),
                _ => p.v.fit = true,
            }
        }
        // The main window does not draw this view, and no window opens for it (`reconcile`).
        self.floating.push(pane);
        let set = Settings { width: w, height: h, ..set };
        self.job = Some(Job { pane, set, steps, done: 0, stepped: false, target, ctx: egui::Context::default(), egui, sink, t0: Instant::now(), said: Instant::now(), note });
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
            Sink::Images(_) => {}
        }
        if let Some(note) = job.note {
            text = format!("{text}\n{note}");
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
            if job.done >= job.steps.len() {
                self.end_render(None);
                return true;
            }
            if !job.stepped {
                let s = job.steps[job.done];
                job.stepped = true;
                self.set_time(pane, s);
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
        let (ctx, pane, w, h, stamp) = (job.ctx.clone(), job.pane, job.set.width, job.set.height, job.set.stamp);
        // The text and the lines have the same size on a 1080 pixel high frame as on a screen.
        let ppp = h as f32 / 1080.0;
        ctx.set_zoom_factor(ppp);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32 / ppp, h as f32 / ppp))),
            time: Some(job.done as f64 / job.set.fps.max(0.01) as f64),
            ..Default::default()
        };
        let out = ctx.run_ui(raw, |ui| self.export_ui(ui, pane, [w, h], stamp));
        let Some(job) = &mut self.job else { return };
        crate::draw_egui(&device, &queue, &mut job.egui, &job.target.view, &ctx, out, [w, h]);
    }

    /// Bring the pixels of the offscreen target back from the GPU, and write them to the output.
    fn write_frame(&mut self) -> Result<(), String> {
        let (Some(win), Some(job)) = (&self.win, &mut self.job) else { return Ok(()) };
        let (device, queue) = (&win.gpu.device, &win.gpu.queue);
        let (w, h, t) = (job.set.width, job.set.height, &job.target);
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &t.buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(t.row), rows_per_image: None } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let slice = t.buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).map_err(err("GPU"))?;
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        {
            let data = slice.get_mapped_range().map_err(err("GPU"))?;
            for r in 0..h as usize {
                let at = r * t.row as usize;
                px.extend_from_slice(&data[at..at + w as usize * 4]);
            }
        }
        t.buffer.unmap();
        match &mut job.sink {
            Sink::Video(child) => child.stdin.as_mut().ok_or("ffmpeg has no input")?.write_all(&px).map_err(err("ffmpeg")),
            Sink::Images(dir) => {
                for p in px.chunks_exact_mut(4) {
                    if t.bgra {
                        p.swap(0, 2);
                    }
                    p[3] = 255;
                }
                write_png(&dir.join(format!("frame_{:05}.png", job.done + 1)), w, h, &px)
            }
        }
    }
}
