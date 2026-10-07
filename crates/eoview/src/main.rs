#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod animate;
mod app;
mod bench;
mod icons;
mod lang;
mod layer;
mod outlines;
mod prefs;
mod py;
mod render;
mod splash;
mod theme;
mod tools;
mod ui;
mod view;
mod wind;

use app::App;
use eo_cache::{Engine, Event};
use eo_render::Gpu;
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

pub const APP: &str = "eoview";
/// Maximum time of the splash window after the start of the GPU. Products that are slow to open continue in the main window.
const SPLASH_MAX: std::time::Duration = std::time::Duration::from_secs(15);

pub enum Ev {
    Wake,
}

/// `eoview --shot`: write the frame of the main window to a PNG file, then stop. For the tests of the
/// interface without a person: the commands run first, one for each frame, by their name in the command palette.
pub struct Shot {
    path: String,
    cmds: std::collections::VecDeque<String>,
    /// Frames to draw before the capture.
    wait: u32,
}

/// Wakes the event loop from other threads (the Python server).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct Win {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    egui: egui_wgpu::Renderer,
    egui_state: egui_winit::State,
    pub gpu: Gpu,
    pub name: String,
    /// Benchmark: offscreen target (EOVIEW_BENCH_SIZE). The frames go there, and the frame waits for the GPU.
    offscreen: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// For the surfaces of the windows of the detached views.
    instance: wgpu::Instance,
}

/// The window of a detached view. It has its own egui context. It uses the GPU device of the main window.
pub struct Detached {
    /// The view of this window.
    pub pane: u32,
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    egui: egui_wgpu::Renderer,
    egui_state: egui_winit::State,
    ctx: egui::Context,
    /// The side panel shows in this window.
    pub panel: bool,
    /// Title of the window: the name of the view and its link group.
    pub title: String,
    /// Time of the next frame, if the interface asked for one.
    wake: Option<Instant>,
}

/// Draw the output of an egui pass to `view` (`size` pixels): the textures, the buffers, one render pass
/// that clears the target, and the submit.
/// `clear`: the color of the target before the interface.
pub fn draw_egui(device: &wgpu::Device, queue: &wgpu::Queue, egui: &mut egui_wgpu::Renderer, view: &wgpu::TextureView, ctx: &egui::Context, mut out: egui::FullOutput, size: [u32; 2], clear: wgpu::Color) {
    let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
    for (id, delta) in &out.textures_delta.set {
        delta.iter().for_each(|x| egui.update_texture(device, queue, *id, x));
    }
    let free: Vec<egui::TextureId> = out.textures_delta.free.iter().copied().collect();
    out.textures_delta.clear();
    let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: size, pixels_per_point: out.pixels_per_point };
    let mut enc = device.create_command_encoder(&Default::default());
    let cmds = egui.update_buffers(device, queue, &mut enc, &prims, &sd);
    {
        let mut pass = enc
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        egui.render(&mut pass, &prims, &sd);
    }
    queue.submit(cmds.into_iter().chain([enc.finish()]));
    for id in &free {
        egui.free_texture(id);
    }
}

/// Make the window of detached view `pane`. None: the system did not make the window or its surface.
fn open_detached(el: &ActiveEventLoop, main: &Win, pane: u32) -> Option<Detached> {
    let attrs = Window::default_attributes().with_title(APP).with_inner_size(winit::dpi::LogicalSize::new(1100, 800)).with_window_icon(app_icon(None));
    let window = Arc::new(el.create_window(attrs).ok()?);
    let surface = main.instance.create_surface(window.clone()).ok()?;
    let device = &main.gpu.device;
    let size = window.inner_size();
    // Same format as the main window: the pipelines of the views draw to the two windows.
    let mut config = main.config.clone();
    (config.width, config.height) = (size.width.max(1), size.height.max(1));
    surface.configure(device, &config);
    let ctx = egui::Context::default();
    let egui = egui_wgpu::Renderer::new(device, config.format, egui_wgpu::RendererOptions::default());
    let egui_state = egui_winit::State::new(
        ctx.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(window.scale_factor() as f32),
        None,
        Some(device.limits().max_texture_dimension_2d as usize),
    );
    Some(Detached { pane, window, surface, config, egui, egui_state, ctx, panel: false, title: String::new(), wake: None })
}

impl Win {
    /// Format of the frames. The pipelines of the views draw to targets of this format.
    pub fn gpu_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// The offscreen target of the frames, if the window has one (benchmark, `eoview --shot`).
    pub fn offscreen_texture(&self) -> Option<&wgpu::Texture> {
        self.offscreen.as_ref().map(|o| &o.0)
    }

    /// Size of the frame in physical pixels.
    pub fn size(&self) -> [u32; 2] {
        match &self.offscreen {
            Some((t, _)) => [t.width(), t.height()],
            None => [self.config.width, self.config.height],
        }
    }
}

impl App {
    fn render(&mut self, el: &ActiveEventLoop) {
        let t0 = Instant::now();
        if let Some(w) = &mut self.win {
            w.gpu.frame += 1;
        }
        let more = self.events();
        let t1 = Instant::now();
        let raw = {
            let w = self.win.as_mut().unwrap();
            let mut raw = w.egui_state.take_egui_input(&w.window);
            if w.offscreen.is_some() {
                let [x, y] = w.size();
                let ppp = w.window.scale_factor() as f32;
                raw.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(x as f32 / ppp, y as f32 / ppp)));
            }
            raw
        };
        let ctx = self.ctx.clone();
        // The animate workspace shows the preview of this frame.
        self.preview_frame();
        let mut out = ctx.run_ui(raw, |ui| self.ui(ui));
        let t2 = Instant::now();
        // A debug build of egui does not permit the drop of texture changes that are not applied.
        let mut textures = std::mem::take(&mut out.textures_delta);
        if self.quit {
            textures.clear();
            return el.exit();
        }
        ui::dialogs(self);
        let w = self.win.as_mut().unwrap();
        w.egui_state.handle_platform_output(&w.window, out.platform_output);
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let (device, queue) = (w.gpu.device.clone(), w.gpu.queue.clone());
        for (id, d) in &textures.set {
            d.iter().for_each(|d| w.egui.update_texture(&device, &queue, *id, d));
        }
        let free: Vec<egui::TextureId> = textures.free.iter().copied().collect();
        textures.clear();
        let t3 = Instant::now();
        // `eoview --shot`: the window is hidden and its surface gets no frame. The frame goes to the offscreen target.
        let frame = if self.shot.is_some() {
            None
        } else {
            match w.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => Some(f),
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    w.surface.configure(&device, &w.config);
                    w.window.request_redraw();
                    return;
                }
                _ => return,
            }
        };
        let clear = self.themes.iter().find(|t| t.name == self.prefs.theme).unwrap_or(&self.themes[0]).clear();
        let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: w.size(), pixels_per_point: out.pixels_per_point };
        let mut enc = device.create_command_encoder(&Default::default());
        let cmds = w.egui.update_buffers(&device, &queue, &mut enc, &prims, &sd);
        let view = match &w.offscreen {
            Some((_, v)) => v.clone(),
            None => match &frame {
                Some(f) => f.texture.create_view(&Default::default()),
                None => return,
            },
        };
        {
            let mut pass = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clear),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            w.egui.render(&mut pass, &prims, &sd);
        }
        let t4 = Instant::now();
        queue.submit(cmds.into_iter().chain([enc.finish()]));
        if w.offscreen.is_some() {
            // The frame time includes the GPU work. The window shows the last presented frame.
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }
        if let Some(frame) = frame {
            w.window.pre_present_notify();
            queue.present(frame);
        }
        // EOVIEW_DEBUG: time of each part of the frames longer than 12 ms.
        if t0.elapsed().as_millis() > 12 && std::env::var_os("EOVIEW_DEBUG").is_some() {
            let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
            let t5 = Instant::now();
            eprintln!("slow frame {:.1} ms: events {:.1} ui {:.1} tessellate {:.1} acquire {:.1} submit {:.1}", ms(t0, t5), ms(t0, t1), ms(t1, t2), ms(t2, t3), ms(t3, t4), ms(t4, t5));
        }
        for id in &free {
            w.egui.free_texture(id);
        }

        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(std::time::Duration::MAX, |v| v.repaint_delay);
        let mut again = more || delay.is_zero();
        let mut until = Instant::now().checked_add(delay);
        if let Some(b) = &mut self.bench {
            match b.presented(&self.engine, &self.panes, el) {
                bench::Next::Redraw => again = true,
                bench::Next::Open(files) => {
                    if files.len() > 1 {
                        self.link_px = std::env::var("EOVIEW_BENCH_LINK").map_or(true, |m| m != "geo");
                    }
                    self.open_many(self.active, files, false);
                    again = true;
                }
                bench::Next::Select(c) => {
                    let id = self.panes[0].id;
                    self.run(ui::Cmd::SetBand(c), id);
                    again = true;
                }
                bench::Next::Wait(t) => until = Some(until.map_or(t, |u| u.min(t))),
            }
        }
        let w = self.win.as_ref().unwrap();
        if again {
            w.window.request_redraw();
        } else if let Some(t) = until {
            self.wake = Some(t);
        }
        self.shot_tick(el);
    }

    /// `eoview --shot`: when the views are complete, run the next command, or write the frame and stop.
    fn shot_tick(&mut self, el: &ActiveEventLoop) {
        let Some(s) = &self.shot else { return };
        let py = self.cli_python.is_some() || self.py.as_ref().is_some_and(|p| p.child.is_some() || !p.reads.is_empty());
        let busy = py || self.opens_pending() > 0 || self.panes.iter().any(|p| !self.floating.contains(&p.id) && p.painter.is_some() && !p.layers.is_empty() && (p.missing || p.v.fit));
        let (wait, next) = (s.wait, s.cmds.front().cloned());
        let cmd = next.as_ref().and_then(|n| ui::command(self, n));
        let Some(s) = &mut self.shot else { return };
        if busy {
            s.wait = 20;
        } else if let Some(n) = next {
            s.cmds.pop_front();
            s.wait = 20;
            match cmd {
                Some(c) => self.run(c, self.active),
                None => eprintln!("shot: no command {n:?}"),
            }
        } else if wait > 0 {
            s.wait -= 1;
        } else {
            let path = s.path.clone();
            if let Err(e) = self.screenshot(&path) {
                eprintln!("shot: {e}");
            }
            self.shot = None;
            el.exit();
        }
    }
}

/// The icon of the window. On Windows, it is the icon resource of the executable (`build.rs`), at `size`
/// pixels or at the default size. On the other systems, it is the 64 pixel image.
fn app_icon(size: Option<u32>) -> Option<winit::window::Icon> {
    #[cfg(windows)]
    {
        use winit::platform::windows::IconExtWindows;
        winit::window::Icon::from_resource(1, size.map(|s| winit::dpi::PhysicalSize::new(s, s))).ok()
    }
    #[cfg(not(windows))]
    {
        let _ = size;
        winit::window::Icon::from_rgba(include_bytes!("../assets/eoview-64.rgba").to_vec(), 64, 64).ok()
    }
}

/// `visible`: show the window now. Else the window stays hidden until its first frame (`App::boot_tick`).
/// `shot`: the frames go to an offscreen target of 1600 x 1000 pixels (`eoview --shot`).
fn init_gpu(el: &ActiveEventLoop, ctx: &egui::Context, budget: usize, bench: bool, visible: bool, shot: bool) -> Win {
    let offscreen_size = std::env::var("EOVIEW_BENCH_SIZE").ok().filter(|_| bench).and_then(|s| {
        let (x, y) = s.split_once('x')?;
        Some((x.parse::<u32>().ok()?, y.parse::<u32>().ok()?))
    });
    let offscreen_size = offscreen_size.or(shot.then_some((1600, 1000)));
    let size = if bench { winit::dpi::LogicalSize::new(3840, 2160) } else { winit::dpi::LogicalSize::new(1500, 950) };
    let attrs = Window::default_attributes().with_title(APP).with_inner_size(size).with_visible(visible).with_window_icon(app_icon(None));
    #[cfg(windows)]
    let attrs = winit::platform::windows::WindowAttributesExtWindows::with_taskbar_icon(attrs, app_icon(Some(256)));
    let window = Arc::new(el.create_window(attrs).unwrap());
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(el.owned_display_handle())));
    let surface = instance.create_surface(window.clone()).unwrap();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .expect("no GPU adapter");
    // The views draw the data to 32-bit float targets. An adapter without this (for example OpenGL ES
    // without the float buffer extension, as on some WSL systems) is not usable: use an other adapter of
    // the system that can do it, a GPU before a software adapter.
    let float = |a: &wgpu::Adapter| a.get_texture_format_features(wgpu::TextureFormat::R32Float).allowed_usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT);
    let adapter = if float(&adapter) {
        adapter
    } else {
        let mut all: Vec<wgpu::Adapter> = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())).into_iter().filter(|a| float(a) && a.is_surface_supported(&surface)).collect();
        all.sort_by_key(|a| a.get_info().device_type == wgpu::DeviceType::Cpu);
        let first = adapter.get_info();
        match all.into_iter().next() {
            Some(a) => {
                eprintln!("GPU adapter {} ({:?}) cannot draw to float targets: the viewer uses {} ({:?})", first.name, first.backend, a.get_info().name, a.get_info().backend);
                a
            }
            None => {
                eprintln!("GPU adapter {} ({:?}) cannot draw to 32-bit float targets, and the system has no other adapter that can. Update the graphics driver, or set WGPU_BACKEND (vulkan, dx12, metal, gl).", first.name, first.backend);
                std::process::exit(1);
            }
        }
    };
    let info = adapter.get_info();
    let name = format!("{} ({:?})", info.name, info.backend);
    if bench {
        println!("{:<40} {name}", "GPU adapter");
    }
    let limits = adapter.limits();
    let desc = wgpu::DeviceDescriptor { required_limits: limits.clone(), ..Default::default() };
    let (device, queue) = pollster::block_on(adapter.request_device(&desc)).unwrap();

    let size = window.inner_size();
    let mut config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).unwrap();
    // Non-sRGB target: colors go to the screen as written. egui also expects this.
    let caps = surface.get_capabilities(&adapter);
    if let Some(f) = caps.formats.iter().find(|f| !f.is_srgb()) {
        config.format = *f;
    }
    // The benchmark measures the frame time without the vertical sync limit.
    config.present_mode = if bench { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::AutoVsync };
    config.desired_maximum_frame_latency = 1;
    surface.configure(&device, &config);

    let egui = egui_wgpu::Renderer::new(&device, config.format, egui_wgpu::RendererOptions::default());
    let egui_state = egui_winit::State::new(
        ctx.clone(),
        egui::ViewportId::ROOT,
        &window,
        Some(window.scale_factor() as f32),
        None,
        Some(limits.max_texture_dimension_2d as usize),
    );
    let offscreen = offscreen_size.map(|(w, h)| {
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let v = t.create_view(&Default::default());
        (t, v)
    });
    let gpu = Gpu::new(device, queue, config.format, budget);
    Win { window, surface, config, egui, egui_state, gpu, name, offscreen, instance }
}

impl ApplicationHandler<Ev> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_some() || self.splash.is_some() {
            return;
        }
        // The splash window shows first. The GPU starts after the first frame of the splash window.
        if self.bench.is_none() && self.cli_render.is_none() && self.shot.is_none() {
            self.splash = splash::Splash::new(el);
        }
        if self.splash.is_none() {
            self.boot(el);
        }
    }

    fn new_events(&mut self, el: &ActiveEventLoop, cause: StartCause) {
        if !matches!(cause, StartCause::ResumeTimeReached { .. }) {
            return;
        }
        if self.splash.is_some() {
            self.boot_tick(el);
        } else if self.shot.is_some() && self.win.is_some() {
            self.wake = None;
            self.render(el);
            self.job_tick(el);
            self.set_wake(el);
        } else {
            self.job_tick(el);
            self.redraw_all();
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, _: Ev) {
        if self.splash.is_some() {
            self.boot_tick(el);
        } else {
            self.job_tick(el);
            self.redraw_all();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        if self.splash.as_ref().is_some_and(|s| s.id() == id) {
            match ev {
                WindowEvent::CloseRequested => el.exit(),
                WindowEvent::RedrawRequested => {
                    let first = self.splash.as_mut().is_some_and(|s| {
                        s.draw();
                        !std::mem::replace(&mut s.drawn, true)
                    });
                    if first {
                        self.boot(el);
                    }
                }
                _ => {}
            }
            return;
        }
        if let Some(k) = self.wins.iter().position(|d| d.window.id() == id) {
            let d = &mut self.wins[k];
            let resp = d.egui_state.on_window_event(&d.window, &ev);
            match ev {
                // The close button of the window attaches the view to the main window.
                WindowEvent::CloseRequested => {
                    let pane = d.pane;
                    self.detach(pane);
                    self.reconcile(el);
                }
                WindowEvent::Resized(s) => {
                    (d.config.width, d.config.height) = (s.width.max(1), s.height.max(1));
                    if let Some(w) = &self.win {
                        d.surface.configure(&w.gpu.device, &d.config);
                    }
                    d.window.request_redraw();
                }
                WindowEvent::RedrawRequested => {
                    self.render_detached(k);
                    self.reconcile(el);
                    self.set_wake(el);
                }
                // The other windows follow: linked views, crosshair, side panel.
                _ if resp.repaint => self.redraw_all(),
                _ => {}
            }
            return;
        }
        let Some(w) = &mut self.win else { return };
        let resp = w.egui_state.on_window_event(&w.window, &ev);
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => {
                w.config.width = s.width.max(1);
                w.config.height = s.height.max(1);
                w.surface.configure(&w.gpu.device, &w.config);
                w.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.wake = None;
                self.render(el);
                self.reconcile(el);
                // A render also gets time here: with frames all the time, the timer does not run.
                self.job_tick(el);
                self.set_wake(el);
            }
            _ if resp.repaint => self.redraw_all(),
            _ => {}
        }
    }
}

impl App {
    /// Work of a render (see `render_tick`). `eoview --render`: start the render when the products are
    /// open, and stop the application at its end. The main window is hidden and gets no frames: this
    /// function also takes the events of the engine.
    fn job_tick(&mut self, el: &ActiveEventLoop) {
        if self.win.is_none() {
            return;
        }
        if self.job.is_none() && self.cli_render.is_some() {
            self.events();
            if self.opens_pending() > 0 {
                return self.set_wake(el);
            }
            let set = self.cli_render.clone().unwrap();
            if let (Some(o), Some(p)) = (self.cli_overlays, self.pane_mut(self.active)) {
                p.overlays = o;
            }
            let (cmap, smooth) = self.cli_look;
            if let Some(p) = self.pane_mut(self.active) {
                p.smooth |= smooth;
                for l in p.layers.iter_mut().filter(|l| l.kind != layer::Kind::Rgb) {
                    if let Some(c) = cmap {
                        l.set_cmap(c);
                    }
                }
            }
            let failed = self.error.clone().or_else(|| self.start_render(self.active, set).err());
            if let (Some(st), Some(j)) = (self.cli_stretch, &self.job) {
                let pane = j.pane;
                for l in self.pane_mut(pane).into_iter().flat_map(|p| p.layers.iter_mut()) {
                    match st {
                        // The limits are for the data layers. A color image keeps its colors.
                        Some(_) if l.kind == layer::Kind::Rgb => {}
                        Some((lo, hi)) => l.st.iter_mut().for_each(|s| (s.lo, s.hi, l.auto_pending) = (lo, hi, false)),
                        None => l.auto_pending = true,
                    }
                }
            }
            if let Some(e) = failed {
                eprintln!("render: {e}");
                self.cli_render = None;
                return el.exit();
            }
        }
        if self.render_tick() && self.cli_render.take().is_some() {
            println!("{}", self.render_msg.clone().unwrap_or_default());
            return el.exit();
        }
        if let Some(j) = &mut self.job {
            // `eoview --render`: the progress, one line for each 2 seconds.
            if self.cli_render.is_some() && j.said.elapsed().as_secs() >= 2 {
                j.said = Instant::now();
                let (pane, done, frames, rate) = (j.pane, j.done, j.frames(), j.done as f64 / j.t0.elapsed().as_secs_f64().max(1e-3));
                let wait = self.frame_wait(pane).map_or(String::new(), |w| format!(", waits for {w}"));
                eprintln!("frame {done} of {frames}, {rate:.1} frames/s{wait}");
            }
            self.set_wake(el);
        }
    }

    /// Ask for a frame of all windows.
    fn redraw_all(&self) {
        self.win.iter().for_each(|w| w.window.request_redraw());
        self.wins.iter().for_each(|d| d.window.request_redraw());
    }

    /// Wait for events, or until the earliest frame that a window asked for.
    fn set_wake(&self, el: &ActiveEventLoop) {
        let job = (self.job.is_some() || self.cli_render.is_some() || self.shot.is_some()).then(|| Instant::now() + std::time::Duration::from_millis(15));
        let t = self.wins.iter().filter_map(|d| d.wake).chain(self.wake).chain(job).min();
        el.set_control_flow(t.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    /// Open the windows of the views that the user detached. Close the windows of the views that are
    /// not detached now.
    fn reconcile(&mut self, el: &ActiveEventLoop) {
        // The temporary view of a render is in `floating`, and has no window.
        let job = self.job.as_ref().map(|j| j.pane);
        let floating: Vec<u32> = self.floating.iter().copied().filter(|&f| Some(f) != job).collect();
        let n = self.wins.len();
        self.wins.retain(|d| floating.contains(&d.pane));
        let mut changed = self.wins.len() != n;
        for id in floating {
            if self.wins.iter().any(|d| d.pane == id) {
                continue;
            }
            match self.win.as_ref().and_then(|w| open_detached(el, w, id)) {
                Some(d) => {
                    self.theme().apply(&d.ctx);
                    self.wins.push(d);
                }
                // No window: the view goes back to the dock.
                None => self.detach(id),
            }
            changed = true;
        }
        if changed {
            self.redraw_all();
        }
    }

    /// Draw one frame of the window of a detached view (`wins[k]`).
    fn render_detached(&mut self, k: usize) {
        let Some(main) = &self.win else { return };
        let (device, queue) = (main.gpu.device.clone(), main.gpu.queue.clone());
        // The main window can be minimized: this frame also takes the events of the engine.
        if self.events() {
            self.redraw_all();
        }
        let (raw, ctx, size) = {
            let d = &mut self.wins[k];
            (d.egui_state.take_egui_input(&d.window), d.ctx.clone(), [d.config.width, d.config.height])
        };
        let mut out = ctx.run_ui(raw, |ui| self.detached_ui(ui, k, size));
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(std::time::Duration::MAX, |v| v.repaint_delay);
        let d = &mut self.wins[k];
        d.egui_state.handle_platform_output(&d.window, std::mem::take(&mut out.platform_output));
        let frame = match d.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                d.surface.configure(&device, &d.config);
                d.window.request_redraw();
                return;
            }
            _ => return,
        };
        let clear = self.theme().clear();
        let d = &mut self.wins[k];
        draw_egui(&device, &queue, &mut d.egui, &frame.texture.create_view(&Default::default()), &ctx, out, size, clear);
        d.window.pre_present_notify();
        queue.present(frame);
        d.wake = None;
        if delay.is_zero() {
            d.window.request_redraw();
        } else {
            d.wake = Instant::now().checked_add(delay);
        }
    }

    /// Start the GPU, make the main window and open the products of the command line. This blocks the
    /// thread. With a splash window, the main window stays hidden (see `boot_tick`).
    fn boot(&mut self, el: &ActiveEventLoop) {
        self.win = Some(init_gpu(el, &self.ctx, self.gpu_budget, self.bench.is_some(), self.splash.is_none() && self.cli_render.is_none() && self.shot.is_none(), self.shot.is_some()));
        if self.bench.is_none() {
            self.load_recent();
            // The messages of `--render` are in English.
            if self.cli_render.is_some() {
                self.prefs.lang.clear();
            }
            self.apply_prefs();
            let mut files: Vec<String> = self.cli_files.take().unwrap_or_else(|| std::env::args().skip(1).collect());
            // --globe: the first view is a globe view.
            if let Some(i) = files.iter().position(|f| f == "--globe") {
                files.remove(i);
                self.set_globe(self.active, true);
            }
            // --stack: the products are the layers of one view, the first product at the bottom.
            let stack = files.iter().position(|f| f == "--stack").map(|i| files.remove(i)).is_some();
            // --series: the products are the time steps of one layer.
            if files.first().is_some_and(|f| f == "--series") {
                self.open_series(self.active, files[1..].to_vec(), false);
            } else if stack && !files.is_empty() {
                let first = files.remove(0);
                self.open(self.active, first, false);
                self.open_many(self.active, files, true);
            } else if !files.is_empty() {
                self.open_many(self.active, files, false);
            }
        }
        if self.cli_render.is_some() || self.shot.is_some() {
            self.set_wake(el);
        }
        let opens = self.opens_pending();
        if let Some(s) = &mut self.splash {
            (s.opens, s.t0) = (opens, Instant::now());
            s.set(0.5);
        }
        self.boot_tick(el);
    }

    /// Splash window: move the progress bar while the products open. Then draw the first frame of the
    /// main window, show the main window and close the splash window.
    fn boot_tick(&mut self, el: &ActiveEventLoop) {
        let Some(s) = &self.splash else { return };
        if self.win.is_none() {
            return;
        }
        let (total, late) = (s.opens.max(1), s.t0.elapsed() > SPLASH_MAX);
        self.events();
        let left = self.opens_pending();
        if left > 0 && !late {
            if let Some(s) = &mut self.splash {
                s.set(0.5 + 0.45 * (1.0 - left as f32 / total as f32));
            }
            return el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + std::time::Duration::from_millis(200)));
        }
        if let Some(s) = &mut self.splash {
            s.set(1.0);
        }
        // The first frame goes to the hidden window: the window then shows with its content.
        self.render(el);
        self.splash = None;
        if let Some(w) = &self.win {
            w.window.set_visible(true);
            w.window.focus_window();
            w.window.request_redraw();
        }
    }
}

fn env_mb(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse::<usize>().ok().map(|m| m << 20)
}

/// A budget in bytes: the environment variable `name` (MB), else the preference (MB, 0: not set), else `default`.
fn budget(name: &str, pref: usize, default: usize) -> usize {
    env_mb(name).or((pref > 0).then_some(pref << 20)).unwrap_or(default)
}

/// Give the engine the disk cache for remote data: `eoview/remote` in the cache directory of the user, 10 GB.
/// EOVIEW_DISK_MB or the preferences set the budget and the directory. EOVIEW_DISK_MB=0: no disk cache.
fn disk_cache(e: &Engine, prefs: &prefs::Prefs) {
    let cap = budget("EOVIEW_DISK_MB", prefs.disk_mb, 10 << 30) as u64;
    let var = |n: &str| std::env::var_os(n).map(std::path::PathBuf::from);
    let dir = if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Caches"))
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
    };
    let dir = if prefs.cache_dir.is_empty() { dir.map(|d| d.join(APP).join("remote")) } else { Some(prefs.cache_dir.clone().into()) };
    if let Some(d) = dir.filter(|_| cap > 0)
        && let Err(err) = e.disk_cache(d, cap)
    {
        eprintln!("no disk cache: {err}");
    }
}

/// `eoview --info <path>`: write the structure of a product to stdout.
fn info(path: &str) {
    let (e, rx) = Engine::new(1 << 30, || {});
    disk_cache(&e, &prefs::Prefs::load());
    let t = Instant::now();
    e.open(path.into());
    let l = loop {
        if let Ok(Event::Opened { res, .. }) = rx.recv() {
            break res;
        }
    };
    let l = match l {
        Ok(l) => l,
        Err(e) => return eprintln!("{path}: {e}"),
    };
    let p = &l.ds.product;
    println!("{}\n{}\nopen and first layer: {:.1} ms, {} source(s)", p.name, p.desc, t.elapsed().as_secs_f64() * 1e3, l.ds.sources.len());
    for v in &p.vars {
        let a = &v.levels[0];
        let lv: Vec<String> = v.levels.iter().map(|a| format!("{}x{}", a.len_of("x"), a.len_of("y"))).collect();
        println!(
            "  {}: {:?} {:?} chunk {:?}, levels [{}], bands {:?}, scale {} offset {} fill {:?} {}",
            v.name,
            a.dtype,
            a.dims,
            a.chunk,
            lv.join(" "),
            v.bands,
            v.scale,
            v.offset,
            v.fill,
            v.units
        );
        if v.steps() > 1 {
            let t = |i: usize| v.times.get(i).map_or("?".into(), |&t| eo_core::time::text(t));
            println!("    {} time steps, {} to {}", v.steps(), t(0), t(v.steps() as usize - 1));
        }
        match &v.georef {
            eo_core::Georef::Affine { gt, crs } => println!("    affine {gt:?} {}", crs.name),
            eo_core::Georef::Grid { cols, rows, .. } => println!("    grid {} x {} nodes", cols.len(), rows.len()),
            g => println!("    {g:?}"),
        }
    }
}

const RENDER_USAGE: &str = "usage: eoview --render [--out FILE or DIRECTORY] [--size WIDTHxHEIGHT] [--fps N] [--steps FIRST:LAST:INTERVAL] [--bbox WEST,SOUTH,EAST,NORTH] [--no-stamp] <project file or products>
  --cmap NAME  color map of the data layers (Gray, Viridis, Magma, Inferno, Plasma, Cividis, Turbo, Jet, Hot, Terrain, RdBu, RdYlGn)
  --smooth  smooth pixels (linear), not squares: for data at a low resolution
  --legend TEXT  text of the legend of the frames
  --overlays LIST  map overlays on the frames: coasts, borders, names (for example coasts,borders,names)
  --keep    keep the frames as PNG files next to the video. A render that stopped continues after its last frame
  --sub N   frames for each time step. More than 1: the frames between two steps are a blend of the steps
  --stretch LOW,HIGH  limits of the color map, in the units of the data. Without it and without a project file: the automatic stretch of the first frame
  --bbox    the frame shows this area, in the units of the display CRS (degrees for longitude and latitude). The height of the area is the height of the frame at this width
  --out     a video file (mp4, mov, mkv, webm, gif: ffmpeg writes it), or a directory for PNG files
  --steps   time steps of the frames, from 0. Empty parts are the defaults: `100:` from step 100, `::4` one step of 4
A project file (.eoview) has its render settings: the options change them.";

/// Options of `eoview --render`, and the paths of the products.
type RenderArgs = (render::Settings, Vec<String>, Option<(f32, f32)>, Option<outlines::Overlays>, (Option<usize>, bool));

fn render_args(args: &[String]) -> Result<RenderArgs, String> {
    let (mut set, mut files, mut it) = (None::<render::Settings>, vec![], args.iter());
    let mut opts: Vec<(&str, String)> = vec![];
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-stamp" => opts.push(("--no-stamp", String::new())),
            "--keep" => opts.push(("--keep", String::new())),
            "--smooth" => opts.push(("--smooth", String::new())),
            "--out" | "--size" | "--fps" | "--steps" | "--bbox" | "--stretch" | "--sub" | "--overlays" | "--cmap" | "--legend" => opts.push((a, it.next().ok_or(format!("{a}: no value"))?.clone())),
            _ => files.push(a.clone()),
        }
    }
    if files.is_empty() {
        return Err("no product".into());
    }
    // The settings of a project file are the defaults.
    if let [f] = &files[..]
        && f.ends_with(&format!(".{}", app::WORKSPACE_EXT))
    {
        let v: Option<serde_json::Value> = std::fs::read_to_string(f).ok().and_then(|s| serde_json::from_str(&s).ok());
        set = v.and_then(|v| serde_json::from_value(v.get("render")?.clone()).ok());
    }
    let mut set = set.unwrap_or_default();
    let mut stretch = None;
    let mut overlays = None;
    // Color map of the data layers, and smooth pixels.
    let mut look: (Option<usize>, bool) = (None, false);
    let bad = |o: &str, v: &str| format!("{o}: bad value {v}");
    for (o, v) in opts {
        match o {
            "--no-stamp" => set.stamp = false,
            "--keep" => set.keep = true,
            "--smooth" => look.1 = true,
            "--legend" => set.legend = v,
            "--cmap" => look.0 = Some(layer::CMAPS.iter().position(|c| c.0.eq_ignore_ascii_case(&v)).ok_or(format!("--cmap: no color map {v}. The color maps: {}", layer::CMAPS.iter().map(|c| c.0).collect::<Vec<_>>().join(", ")))?),
            "--out" => set.out = v,
            "--stretch" => stretch = Some(v.split_once(',').and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?))).ok_or(bad(o, &v))?),
            "--bbox" => {
                let b: Vec<f64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                let [w, s, e, n] = b[..] else { return Err(bad(o, &v)) };
                (set.fit, set.view) = (false, Some(([(w + e) / 2.0, (s + n) / 2.0], e - w)));
            }
            "--fps" => set.fps = v.parse().ok().filter(|f| *f > 0.0).ok_or(bad(o, &v))?,
            "--overlays" => overlays = Some(outlines::Overlays { coasts: v.contains("coasts"), borders: v.contains("borders"), names: v.contains("names") }),
            "--sub" => set.sub = v.parse().ok().filter(|n| *n >= 1).ok_or(bad(o, &v))?,
            "--size" => {
                let (w, h) = v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))).ok_or(bad(o, &v))?;
                (set.width, set.height) = (w, h);
            }
            _ => {
                let p: Vec<&str> = v.split(':').collect();
                let num = |i: usize| p.get(i).filter(|s| !s.is_empty()).map(|s| s.parse::<usize>().map_err(|_| bad(o, &v))).transpose();
                (set.first, set.last, set.stride) = (num(0)?.unwrap_or(0), num(1)?, num(2)?.unwrap_or(1).max(1));
            }
        }
    }
    Ok((set, files, stretch, overlays, look))
}

fn main() {
    let t0 = Instant::now();
    if let [_, flag, path] = &std::env::args().collect::<Vec<_>>()[..]
        && flag == "--info"
    {
        return info(path);
    }
    // WSLg: Vulkan uses the CPU (lavapipe) and the Wayland socket is not stable. Mesa d3d12 GL over X11 uses the GPU.
    #[cfg(target_os = "linux")]
    let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some();
    #[cfg(target_os = "linux")]
    if wsl && std::env::var_os("GALLIUM_DRIVER").is_none() {
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("GALLIUM_DRIVER", "d3d12") };
    }
    #[allow(unused_mut)]
    let mut builder = EventLoop::<Ev>::with_user_event();
    #[cfg(target_os = "linux")]
    if wsl {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let el = builder.build().unwrap();
    let proxy = el.create_proxy();
    let args: Vec<String> = std::env::args().collect();
    let is_bench = args.get(1).map(String::as_str) == Some("--bench");
    // A benchmark does not use the preferences of the user.
    let prefs = if is_bench { prefs::Prefs::default() } else { prefs::Prefs::load() };
    let ram = budget("EOVIEW_RAM_MB", prefs.ram_mb, eo_cache::system_ram() / 4);
    let (engine, events) = Engine::new(ram, move || drop(proxy.send_event(Ev::Wake)));
    let bench = is_bench.then(|| bench::Bench::new(t0, &args[2..]));
    // A benchmark measures the remote reads: no disk cache.
    if bench.is_none() {
        disk_cache(&engine, &prefs);
    }
    let mut app = App::new(engine, events, budget("EOVIEW_GPU_MB", prefs.gpu_mb, 1 << 30), bench);
    app.prefs = prefs;
    // Python scripts and notebooks. Not in a benchmark and not in a render of the command line.
    let proxy = el.create_proxy();
    if !is_bench && args.get(1).map(String::as_str) != Some("--render") {
        match py::Py::start(Arc::new(move || drop(proxy.send_event(Ev::Wake)))) {
            Ok(p) => app.py = Some(p),
            Err(e) => eprintln!("no Python server: {e}"),
        }
    }
    // eoview [--shot ...] --python FILE [products]: run the script when the products show.
    let mut args = args;
    if let Some(i) = args.iter().position(|a| a == "--python")
        && i + 1 < args.len()
    {
        app.cli_python = Some(args.remove(i + 1));
        args.remove(i);
        if args.get(1).map(String::as_str) != Some("--shot") {
            app.cli_files = Some(args[1..].to_vec());
        }
    }
    // eoview --shot FILE.png [--do "command name"]... [products]
    if args.get(1).map(String::as_str) == Some("--shot") && args.len() > 2 {
        let (mut cmds, mut files, mut it) = (std::collections::VecDeque::new(), vec![], args[3..].iter());
        while let Some(a) = it.next() {
            if a == "--do" { cmds.extend(it.next().cloned()) } else { files.push(a.clone()) }
        }
        (app.shot, app.cli_files) = (Some(Shot { path: args[2].clone(), cmds, wait: 20 }), Some(files));
    }
    if args.get(1).map(String::as_str) == Some("--render") {
        match render_args(&args[2..]) {
            Ok((set, files, stretch, overlays, look)) => {
                app.cli_overlays = overlays;
                app.cli_look = look;
                // Products without a project file have no stretch from a person: the limits of the option, or
                // the automatic stretch of the first frame (the first step of a data cube can have no data).
                let project = files.len() == 1 && files[0].ends_with(&format!(".{}", app::WORKSPACE_EXT));
                app.cli_stretch = stretch.map(Some).or((!project).then_some(None));
                (app.cli_render, app.cli_files) = (Some(set), Some(files));
            }
            Err(e) => return eprintln!("{e}\n{RENDER_USAGE}"),
        }
    }
    el.run_app(&mut app).unwrap();
    // ponytail: no orderly stop of the engine. Its threads stop with the process: at the end of `main`,
    // a thread with a timer panics when the runtime goes away before it. Stop the engine in order if
    // the engine gets work that must complete at the exit.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}
