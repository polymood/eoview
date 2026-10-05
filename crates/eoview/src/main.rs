#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod app;
mod bench;
mod icons;
mod layer;
mod ui;
mod view;

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

pub enum Ev {
    Wake,
}

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
}

impl Win {
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
        let out = ctx.run_ui(raw, |ui| self.ui(ui));
        let t2 = Instant::now();
        if self.quit {
            return el.exit();
        }
        ui::dialogs(self);
        let w = self.win.as_mut().unwrap();
        w.egui_state.handle_platform_output(&w.window, out.platform_output);
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let (device, queue) = (w.gpu.device.clone(), w.gpu.queue.clone());
        for (id, d) in &out.textures_delta.set {
            d.iter().for_each(|d| w.egui.update_texture(&device, &queue, *id, d));
        }
        let t3 = Instant::now();
        let frame = match w.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                w.surface.configure(&device, &w.config);
                w.window.request_redraw();
                return;
            }
            _ => return,
        };
        let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: w.size(), pixels_per_point: out.pixels_per_point };
        let mut enc = device.create_command_encoder(&Default::default());
        let cmds = w.egui.update_buffers(&device, &queue, &mut enc, &prims, &sd);
        let view = match &w.offscreen {
            Some((_, v)) => v.clone(),
            None => frame.texture.create_view(&Default::default()),
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
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.06, g: 0.06, b: 0.07, a: 1.0 }),
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
        w.window.pre_present_notify();
        queue.present(frame);
        // EOVIEW_DEBUG: time of each part of the frames longer than 12 ms.
        if t0.elapsed().as_millis() > 12 && std::env::var_os("EOVIEW_DEBUG").is_some() {
            let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
            let t5 = Instant::now();
            eprintln!("slow frame {:.1} ms: events {:.1} ui {:.1} tessellate {:.1} acquire {:.1} submit {:.1}", ms(t0, t5), ms(t0, t1), ms(t1, t2), ms(t2, t3), ms(t3, t4), ms(t4, t5));
        }
        for id in &out.textures_delta.free {
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
            el.set_control_flow(ControlFlow::WaitUntil(t));
        }
    }
}

fn init_gpu(el: &ActiveEventLoop, ctx: &egui::Context, budget: usize, bench: bool) -> Win {
    let offscreen_size = std::env::var("EOVIEW_BENCH_SIZE").ok().filter(|_| bench).and_then(|s| {
        let (x, y) = s.split_once('x')?;
        Some((x.parse::<u32>().ok()?, y.parse::<u32>().ok()?))
    });
    let size = if bench { winit::dpi::LogicalSize::new(3840, 2160) } else { winit::dpi::LogicalSize::new(1500, 950) };
    let attrs = Window::default_attributes().with_title(APP).with_inner_size(size);
    let window = Arc::new(el.create_window(attrs).unwrap());
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(el.owned_display_handle())));
    let surface = instance.create_surface(window.clone()).unwrap();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        ..Default::default()
    }))
    .expect("no GPU adapter");
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let v = t.create_view(&Default::default());
        (t, v)
    });
    let gpu = Gpu::new(device, queue, config.format, budget);
    Win { window, surface, config, egui, egui_state, gpu, name, offscreen }
}

impl ApplicationHandler<Ev> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_some() {
            return;
        }
        self.win = Some(init_gpu(el, &self.ctx, self.gpu_budget, self.bench.is_some()));
        if self.bench.is_none() {
            self.load_recent();
            let files: Vec<String> = std::env::args().skip(1).collect();
            // --series: the products are the time steps of one layer.
            if files.first().is_some_and(|f| f == "--series") {
                self.open_series(self.active, files[1..].to_vec(), false);
            } else if !files.is_empty() {
                self.open_many(self.active, files, false);
            }
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let (StartCause::ResumeTimeReached { .. }, Some(w)) = (cause, &self.win) {
            w.window.request_redraw();
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, _: Ev) {
        if let Some(w) = &self.win {
            w.window.request_redraw();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
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
                el.set_control_flow(ControlFlow::Wait);
                self.render(el);
            }
            _ if resp.repaint => w.window.request_redraw(),
            _ => {}
        }
    }
}

fn env_mb(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse::<usize>().ok().map(|m| m << 20)
}

/// Give the engine the disk cache for remote data: `eoview/remote` in the cache directory of the user, 10 GB.
/// EOVIEW_DISK_MB sets the budget. 0: no disk cache.
fn disk_cache(e: &Engine) {
    let cap = env_mb("EOVIEW_DISK_MB").unwrap_or(10 << 30) as u64;
    let var = |n: &str| std::env::var_os(n).map(std::path::PathBuf::from);
    let dir = if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Caches"))
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
    };
    if let Some(d) = dir.filter(|_| cap > 0)
        && let Err(err) = e.disk_cache(d.join(APP).join("remote"), cap)
    {
        eprintln!("no disk cache: {err}");
    }
}

/// `eoview --info <path>`: write the structure of a product to stdout.
fn info(path: &str) {
    let (e, rx) = Engine::new(1 << 30, || {});
    disk_cache(&e);
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
    let ram = env_mb("EOVIEW_RAM_MB").unwrap_or(eo_cache::system_ram() / 4);
    let (engine, events) = Engine::new(ram, move || drop(proxy.send_event(Ev::Wake)));
    let args: Vec<String> = std::env::args().collect();
    let bench = (args.get(1).map(String::as_str) == Some("--bench")).then(|| bench::Bench::new(t0, &args[2..]));
    // A benchmark measures the remote reads: no disk cache.
    if bench.is_none() {
        disk_cache(&engine);
    }
    let mut app = App::new(engine, events, env_mb("EOVIEW_GPU_MB").unwrap_or(1 << 30), bench);
    el.run_app(&mut app).unwrap();
}
