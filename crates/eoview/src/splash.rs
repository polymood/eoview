//! Splash window: it shows before the main window, while the GPU starts and the products of the command
//! line open. The CPU draws it (softbuffer): it does not wait for the GPU.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

/// Inner rectangle of the progress bar in `assets/splash.png`: x, y, width, height.
/// `SPLASH_BAR` in `scripts/make_icons.py` has the same values.
const BAR: [u32; 4] = [210, 198, 292, 16];
/// Color of the progress bar (0x00RRGGBB): `PAPER` of `scripts/make_icons.py`.
const BAR_COLOR: u32 = 0x00f1_f4f3;

pub struct Splash {
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    _context: softbuffer::Context<Arc<Window>>,
    /// Pixels of the image (0x00RRGGBB), its width and its height.
    px: Vec<u32>,
    w: u32,
    h: u32,
    /// Pixels of the image for each unit of `BAR`: 1, or 2 on a high-density screen.
    scale: u32,
    /// 0 to 1.
    progress: f32,
    /// The window has its first frame: the start of the GPU can block the thread.
    pub drawn: bool,
    /// Number of products that opened at the start, and the time of the start.
    pub opens: usize,
    pub t0: Instant,
}

impl Splash {
    /// Make the window at the center of the primary monitor. `None`: no splash window on this system.
    pub fn new(el: &ActiveEventLoop) -> Option<Splash> {
        let monitor = el.primary_monitor();
        let (png, scale): (&[u8], u32) = if monitor.as_ref().is_some_and(|m| m.scale_factor() >= 1.5) {
            (include_bytes!("../assets/splash@2x.png"), 2)
        } else {
            (include_bytes!("../assets/splash.png"), 1)
        };
        let (px, w, h) = decode(png)?;
        let mut attrs = Window::default_attributes()
            .with_title(crate::APP)
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(w, h))
            .with_window_icon(crate::app_icon(None));
        if let Some(m) = &monitor {
            let (p, s) = (m.position(), m.size());
            attrs = attrs.with_position(PhysicalPosition::new(p.x + (s.width as i32 - w as i32) / 2, p.y + (s.height as i32 - h as i32) / 2));
        }
        let window = Arc::new(el.create_window(attrs).ok()?);
        let context = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&context, window.clone()).ok()?;
        window.request_redraw();
        Some(Splash { window, surface, _context: context, px, w, h, scale, progress: 0.1, drawn: false, opens: 0, t0: Instant::now() })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    /// Set the progress (0 to 1) and draw the window.
    pub fn set(&mut self, progress: f32) {
        self.progress = progress;
        self.draw();
    }

    /// Draw the image and the progress bar. If the window does not have the size of the image, the rest is black.
    pub fn draw(&mut self) {
        let size = self.window.inner_size();
        let (Some(sw), Some(sh)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
        if self.surface.resize(sw, sh).is_err() {
            return;
        }
        let Ok(mut buf) = self.surface.buffer_mut() else { return };
        let (bw, w, h) = (size.width as usize, self.w as usize, self.h as usize);
        let [x0, y0, bar_w, bar_h] = BAR.map(|v| (v * self.scale) as usize);
        let end = x0 + (bar_w as f32 * self.progress.clamp(0.0, 1.0)) as usize;
        let n = w.min(bw);
        for (y, row) in buf.chunks_exact_mut(bw).enumerate() {
            if y >= h {
                row.fill(0);
                continue;
            }
            row[..n].copy_from_slice(&self.px[y * w..y * w + n]);
            row[n..].fill(0);
            if (y0..y0 + bar_h).contains(&y) {
                row[x0.min(n)..end.min(n)].fill(BAR_COLOR);
            }
        }
        let _ = buf.present();
    }
}

/// Decode an 8-bit RGB PNG file to 0x00RRGGBB pixels, with its width and its height.
fn decode(png: &[u8]) -> Option<(Vec<u32>, u32, u32)> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().ok()?;
    let mut rgb = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgb).ok()?;
    if info.color_type != png::ColorType::Rgb || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let px = rgb[..info.buffer_size()].chunks_exact(3).map(|c| u32::from_be_bytes([0, c[0], c[1], c[2]])).collect();
    Some((px, info.width, info.height))
}
