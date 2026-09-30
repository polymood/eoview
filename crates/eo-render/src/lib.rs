//! wgpu renderer. The GPU keeps 512 x 512 tiles in texture arrays, with LRU eviction.
//!
//! A view has inputs (layers) and a composite. Each input draws its visible tiles in one instanced draw
//! call into an offscreen f32 target of the view size: the vertex shader moves the vertices of a mesh
//! on each tile through the warp grid of the layer (reprojection on the GPU). The fragment shader writes the
//! physical value. Then one composite pass reads all targets at the same pixel and computes the color:
//! one band with a color map, an RGB composite, or a band math expression. A display change does not load data.
pub mod bandmath;

use eo_cache::{Pixels, TILE, TileKey};
use std::collections::HashMap;

const T: u32 = TILE as u32;
/// Maximum number of tiles in one draw call.
pub const MAX_DRAWS: usize = 8192;
/// Maximum number of inputs of a composite.
pub const MAX_INPUTS: usize = 8;
/// Value of an offscreen target where no layer pixel is. The composite discards it.
pub const NO_DATA: f32 = -3.0e38;
/// Subdivisions of each side of a tile when the warp is not affine.
const MESH: u32 = 8;

const LAYER_SHADER: &str = r#"
struct U {
    off: vec2f, scale: f32, n: u32,
    view: vec2f, a: f32, b: f32,
    wsize: vec2f, fill: f32, flags: u32,
    grid: vec2u, p0: u32, p1: u32,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var tiles: texture_2d_array<f32>;
@group(0) @binding(3) var warp: texture_2d<f32>;

struct VO {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
    @location(1) @interpolate(flat) uvmax: vec2f,
    @location(2) @interpolate(flat) layer: u32,
};

fn node(i: vec2u) -> vec2f {
    return textureLoad(warp, min(i, u.grid - 1u), 0).rg;
}

// Display coordinates (relative to the warp origin) of level-0 pixel position p: bilinear on the grid.
fn warped(p: vec2f) -> vec2f {
    let g = clamp(p / u.wsize, vec2f(0.0), vec2f(1.0)) * vec2f(u.grid - 1u);
    let i = min(vec2u(g), u.grid - 2u);
    let t = g - vec2f(i);
    let top = mix(node(i), node(i + vec2u(1u, 0u)), t.x);
    let bot = mix(node(i + vec2u(0u, 1u)), node(i + vec2u(1u, 1u)), t.x);
    return mix(top, bot, t.y);
}

// rect: tile corners in level-0 pixels. The tile is a mesh of n x n quads.
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) rect: vec4f, @location(1) uvl: vec4f) -> VO {
    let q = vi / 6u;
    let k = vi % 6u;
    let corner = array(vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0), vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0));
    let t = (vec2f(f32(q % u.n), f32(q / u.n)) + corner[k]) / f32(u.n);
    let d = (warped(mix(rect.xy, rect.zw, t)) + u.off) * u.scale / (u.view * 0.5);
    return VO(vec4f(d.x, d.y, 0.0, 1.0), t * uvl.xy, uvl.xy - vec2f(0.5 / 512.0), u32(uvl.z));
}

@fragment fn fs(v: VO) -> @location(0) vec4f {
    let s = textureSample(tiles, smp, min(v.uv, v.uvmax), v.layer).r;
    // NaN is no data. Test the bits: a compiler can remove the test s != s.
    if ((bitcast<u32>(s) & 0x7fffffffu) > 0x7f800000u) { discard; }
    if ((u.flags & 4u) != 0u && abs(s - u.fill) < 0.5 / 255.0) { discard; }
    return vec4f(s * u.a + u.b, 0.0, 0.0, 1.0);
}
"#;

/// Composite shader. `EXPR` is replaced with the WGSL of the mode (it sets `c`, the color).
const COMPOSITE_SHADER: &str = r#"
struct C {
    vo: vec2f, n: u32, flags: u32,
    lo: array<vec4f, 2>, hi: array<vec4f, 2>, gamma: array<vec4f, 2>,
};
@group(0) @binding(0) var<uniform> u: C;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var lut: texture_2d<f32>;
@group(0) @binding(3) var i0: texture_2d<f32>;
@group(0) @binding(4) var i1: texture_2d<f32>;
@group(0) @binding(5) var i2: texture_2d<f32>;
@group(0) @binding(6) var i3: texture_2d<f32>;
@group(0) @binding(7) var i4: texture_2d<f32>;
@group(0) @binding(8) var i5: texture_2d<f32>;
@group(0) @binding(9) var i6: texture_2d<f32>;
@group(0) @binding(10) var i7: texture_2d<f32>;

@vertex fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

fn lim(k: u32, v: array<vec4f, 2>) -> f32 {
    return v[k / 4u][k % 4u];
}

// Stretch of channel k to 0..1: dB (flag bit 8 + k), limits, gamma.
fn stretch(x: f32, k: u32) -> f32 {
    let y = select(x, 6.0206 * log2(max(abs(x), 1e-10)), (u.flags & (256u << k)) != 0u);
    return pow(clamp((y - lim(k, u.lo)) / (lim(k, u.hi) - lim(k, u.lo)), 0.0, 1.0), lim(k, u.gamma));
}

fn cmap(t: f32) -> vec3f {
    let s = select(t, 1.0 - t, (u.flags & 2u) != 0u);
    return textureSampleLevel(lut, smp, vec2f(s * (255.0 / 256.0) + 0.5 / 256.0, 0.5), 0.0).rgb;
}

fn nd(x: f32) -> bool {
    return x <= -1.0e38;
}

fn finite(x: f32) -> bool {
    let b = bitcast<u32>(x) & 0x7fffffffu;
    return b < 0x7f800000u;
}

@fragment fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let p = vec2i(pos.xy - u.vo);
    let v0 = textureLoad(i0, p, 0).r;
    let v1 = textureLoad(i1, p, 0).r;
    let v2 = textureLoad(i2, p, 0).r;
    let v3 = textureLoad(i3, p, 0).r;
    let v4 = textureLoad(i4, p, 0).r;
    let v5 = textureLoad(i5, p, 0).r;
    let v6 = textureLoad(i6, p, 0).r;
    let v7 = textureLoad(i7, p, 0).r;
    var c = vec3f(0.0);
    EXPR
    return vec4f(c, 1.0);
}
"#;

/// How a view makes the color from its inputs. The strings are WGSL expressions of the input values
/// v0 .. v7 (see `bandmath`).
#[derive(Clone, PartialEq, Debug)]
pub enum Mode {
    /// One value with the color map.
    Gray(String),
    /// Red, green and blue. Each has its own stretch.
    Rgb([String; 3]),
}

impl Mode {
    /// Fragment code. `n` inputs are used: a pixel without data in one of them has no color.
    fn wgsl(&self, n: usize) -> String {
        let nd = if n == 0 { "false".into() } else { (0..n).map(|k| format!("nd(v{k})")).collect::<Vec<_>>().join(" || ") };
        match self {
            Mode::Gray(e) => format!("if ({nd}) {{ discard; }}\n    let x = f32({e});\n    if (!finite(x)) {{ discard; }}\n    c = cmap(stretch(x, 0u));"),
            Mode::Rgb(e) => format!(
                "if ({nd}) {{ discard; }}\n    c = vec3f(stretch(f32({}), 0u), stretch(f32({}), 1u), stretch(f32({}), 2u));",
                e[0], e[1], e[2]
            ),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LayerUniforms {
    /// Warp origin minus view center, in display units.
    pub off: [f32; 2],
    /// Physical pixels for each display unit.
    pub scale: f32,
    pub n: u32,
    /// View size in physical pixels.
    pub view: [f32; 2],
    /// Physical value = texel * a + b.
    pub a: f32,
    pub b: f32,
    /// Level-0 size of the layer in pixels.
    pub wsize: [f32; 2],
    pub fill: f32,
    /// 4: u8 fill value is no data.
    pub flags: u32,
    pub grid: [u32; 2],
    pub pad: [u32; 2],
}

/// Stretch and color flags of a composite.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CompositeUniforms {
    /// View origin in window pixels.
    pub vo: [f32; 2],
    pub n: u32,
    /// 2: invert the color map. 256 << k: channel k in dB.
    pub flags: u32,
    pub lo: [f32; 8],
    pub hi: [f32; 8],
    pub gamma: [f32; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Inst {
    /// Tile corners in level-0 pixels.
    pub rect: [f32; 4],
    /// uv max x, uv max y, array layer, unused.
    pub uvl: [f32; 4],
}

struct Slot {
    layer: u32,
    done: bool,
    used: u64,
}

/// One texture array: one layer for each tile.
struct TileArray {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    bpp: u32,
    map: HashMap<TileKey, Slot>,
    free: Vec<u32>,
}

/// Shared GPU state: pipelines and the tile arrays (one for u8 tiles, one for f16 tiles).
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    layer_pipe: wgpu::RenderPipeline,
    layer_bgl: wgpu::BindGroupLayout,
    comp_bgl: wgpu::BindGroupLayout,
    comp_pipes: HashMap<String, wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
    lut_sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    arrays: [Option<TileArray>; 2],
    layers: u32,
    /// Frame counter for the LRU order. The application increments it.
    pub frame: u64,
    /// Bytes of the offscreen targets of all views.
    pub target_bytes: usize,
}

fn tex_entry(binding: u32, dim: wgpu::TextureViewDimension, filterable: bool, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable }, view_dimension: dim, multisampled: false },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None }
}

impl Gpu {
    /// `budget` is the GPU memory budget in bytes for the tile arrays.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat, budget: usize) -> Gpu {
        let d2 = wgpu::TextureViewDimension::D2;
        let frag = wgpu::ShaderStages::FRAGMENT;
        let layer_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer"),
            entries: &[
                uniform_entry(0),
                sampler_entry(1),
                tex_entry(2, wgpu::TextureViewDimension::D2Array, true, frag),
                tex_entry(3, d2, false, wgpu::ShaderStages::VERTEX),
            ],
        });
        let mut ce = vec![uniform_entry(0), sampler_entry(1), tex_entry(2, d2, true, frag)];
        ce.extend((0..MAX_INPUTS as u32).map(|k| tex_entry(3 + k, d2, false, frag)));
        let comp_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("composite"), entries: &ce });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("layer"), source: wgpu::ShaderSource::Wgsl(LAYER_SHADER.into()) });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layer_bgl)], immediate_size: 0 });
        let layer_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layer"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Inst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::R32Float.into())],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        // Nearest when magnified (show the real pixels). Linear when minified.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let lut_sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let dummy = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("no input"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        // The u8 array gets 1/3 of the budget and the f16 array 2/3: both have the same number of layers.
        let layers = device.limits().max_texture_array_layers.min((budget / (3 * TILE as usize * TILE as usize)).max(1) as u32);
        Gpu {
            device,
            queue,
            format,
            layer_pipe,
            layer_bgl,
            comp_bgl,
            comp_pipes: HashMap::new(),
            sampler,
            lut_sampler,
            dummy,
            arrays: [None, None],
            layers,
            frame: 0,
            target_bytes: 0,
        }
    }

    fn array(&mut self, u8: bool) -> &mut TileArray {
        let (i, layers, dev) = (u8 as usize, self.layers, &self.device);
        self.arrays[i].get_or_insert_with(|| {
            let tex = dev.create_texture(&wgpu::TextureDescriptor {
                label: Some(if u8 { "tiles u8" } else { "tiles f16" }),
                size: wgpu::Extent3d { width: T, height: T, depth_or_array_layers: layers },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: if u8 { wgpu::TextureFormat::R8Unorm } else { wgpu::TextureFormat::R16Float },
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
            TileArray { tex, view, bpp: if u8 { 1 } else { 2 }, map: HashMap::new(), free: (0..layers).rev().collect() }
        })
    }

    /// Composite pipeline of a mode for `n` inputs. Errors come from the WGSL compiler (band math).
    fn composite(&mut self, mode: &Mode, n: usize) -> Result<wgpu::RenderPipeline, String> {
        let src = COMPOSITE_SHADER.replace("EXPR", &mode.wgsl(n));
        if let Some(p) = self.comp_pipes.get(&src) {
            return Ok(p.clone());
        }
        let dev = &self.device;
        let scope = dev.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("composite"), source: wgpu::ShaderSource::Wgsl(src.clone().into()) });
        let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&self.comp_bgl)], immediate_size: 0 });
        let pipe = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(self.format.into())],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(e.to_string());
        }
        self.comp_pipes.insert(src, pipe.clone());
        Ok(pipe)
    }

    /// Array layer of a resident tile, and true if the tile is complete. Marks the tile as used in this frame.
    pub fn lookup(&mut self, key: &TileKey, u8: bool) -> Option<(u32, bool)> {
        let frame = self.frame;
        let s = self.arrays[u8 as usize].as_mut()?.map.get_mut(key)?;
        s.used = frame;
        Some((s.layer, s.done))
    }

    /// Put a tile on the GPU. If the array is full, remove the least recently used tile that is not
    /// used in this frame or the previous frame. Return false if there is no free layer.
    pub fn upload(&mut self, key: TileKey, w: u32, h: u32, px: &Pixels, done: bool) -> bool {
        let (frame, u8) = (self.frame, matches!(px, Pixels::U8(_)));
        let queue = self.queue.clone();
        let a = self.array(u8);
        let layer = match a.map.get_mut(&key) {
            Some(s) => {
                s.done = done;
                s.layer
            }
            None => {
                let layer = match a.free.pop() {
                    Some(l) => l,
                    None => {
                        // ponytail: O(n) scan for the oldest tile. Use an ordered index if arrays get much larger than 2048 layers.
                        let Some((&k, s)) = a.map.iter().filter(|(_, s)| s.used + 1 < frame).min_by_key(|(_, s)| s.used) else {
                            return false;
                        };
                        let l = s.layer;
                        a.map.remove(&k);
                        l
                    }
                };
                a.map.insert(key, Slot { layer, done, used: frame });
                layer
            }
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &a.tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: layer }, aspect: wgpu::TextureAspect::All },
            px.bytes(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * a.bpp), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        true
    }

    /// Allocated bytes of the tile arrays and targets, and bytes of the resident tiles.
    pub fn usage(&self) -> (usize, usize) {
        let t = (T * T) as usize;
        self.arrays.iter().flatten().fold((self.target_bytes, 0), |(a, r), x| {
            let b = t * x.bpp as usize;
            (a + b * self.layers as usize, r + b * x.map.len())
        })
    }

    pub fn resident(&self) -> usize {
        self.arrays.iter().flatten().map(|a| a.map.len()).sum()
    }
}

/// GPU resources of one input of a view.
pub struct Input {
    ubuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    warp: Option<(wgpu::Texture, u64)>,
    bind: Option<(wgpu::BindGroup, bool, u64)>,
    /// Instances of the next draw, coarse tiles first. The application fills it.
    pub insts: Vec<Inst>,
}

impl Input {
    pub fn new(gpu: &Gpu) -> Input {
        let buf = |size, usage| gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage, mapped_at_creation: false });
        Input {
            ubuf: buf(std::mem::size_of::<LayerUniforms>() as u64, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST),
            ibuf: buf((MAX_DRAWS * std::mem::size_of::<Inst>()) as u64, wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST),
            warp: None,
            bind: None,
            insts: Vec::with_capacity(MAX_DRAWS),
        }
    }

    /// Set the warp grid (display coordinates relative to the warp origin). `id` identifies the grid:
    /// the upload happens only when it changes.
    pub fn set_warp(&mut self, gpu: &Gpu, id: u64, nx: usize, ny: usize, pts: &[[f64; 2]]) {
        if self.warp.as_ref().is_some_and(|w| w.1 == id) {
            return;
        }
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("warp"),
            size: wgpu::Extent3d { width: nx as u32, height: ny as u32, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // A point outside the domain of the projection gets a far position: its triangles are off screen.
        let data: Vec<[f32; 2]> = pts.iter().map(|p| if p[0].is_finite() { [p[0] as f32, p[1] as f32] } else { [1e30, 1e30] }).collect();
        gpu.queue.write_texture(
            tex.as_image_copy(),
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(nx as u32 * 8), rows_per_image: None },
            wgpu::Extent3d { width: nx as u32, height: ny as u32, depth_or_array_layers: 1 },
        );
        self.warp = Some((tex, id));
        self.bind = None;
    }
}

/// GPU resources of one 2D view: offscreen targets, color map and composite.
pub struct View2d {
    lut: wgpu::Texture,
    cbuf: wgpu::Buffer,
    targets: Vec<(wgpu::Texture, wgpu::TextureView)>,
    size: (u32, u32),
    pub inputs: Vec<Input>,
}

impl View2d {
    pub fn new(gpu: &Gpu) -> View2d {
        let lut = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lut"),
            size: wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let cbuf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite"),
            size: std::mem::size_of::<CompositeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        View2d { lut, cbuf, targets: vec![], size: (0, 0), inputs: vec![] }
    }

    pub fn set_lut(&self, gpu: &Gpu, rgba: &[[u8; 4]]) {
        gpu.queue.write_texture(
            self.lut.as_image_copy(),
            bytemuck::cast_slice(rgba),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256 * 4), rows_per_image: None },
            wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
        );
    }

    /// Offscreen targets: one for each input, of the view size. They change only when the size or the count changes.
    fn targets(&mut self, gpu: &mut Gpu, w: u32, h: u32, n: usize) {
        if self.size == (w, h) && self.targets.len() == n {
            return;
        }
        gpu.target_bytes -= self.targets.len() * (self.size.0 * self.size.1 * 4) as usize;
        self.targets = (0..n)
            .map(|_| {
                let t = gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("input target"),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R32Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let v = t.create_view(&Default::default());
                (t, v)
            })
            .collect();
        self.size = (w, h);
        gpu.target_bytes += n * (w * h * 4) as usize;
    }

    /// Write the buffers and make the paint callback for egui. `layers[k]` are the uniforms and the u8 flag
    /// of input k. `rect` is the view in points, `px` its size in physical pixels, `origin` in window pixels.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        gpu: &mut Gpu,
        layers: &[(LayerUniforms, bool)],
        mode: &Mode,
        cu: &CompositeUniforms,
        rect: egui::Rect,
        px: (u32, u32),
    ) -> Result<Option<egui::PaintCallback>, String> {
        let n = layers.len().min(self.inputs.len()).min(MAX_INPUTS);
        if n == 0 || px.0 == 0 || px.1 == 0 {
            return Ok(None);
        }
        let pipe = gpu.composite(mode, n)?;
        self.targets(gpu, px.0, px.1, n);
        let mut passes = vec![];
        for (k, (lu, u8)) in layers.iter().take(n).enumerate() {
            let inp = &mut self.inputs[k];
            inp.insts.truncate(MAX_DRAWS);
            let Some((warp, wid)) = &inp.warp else {
                passes.push((self.targets[k].1.clone(), None));
                continue;
            };
            gpu.queue.write_buffer(&inp.ubuf, 0, bytemuck::bytes_of(lu));
            if !inp.insts.is_empty() {
                gpu.queue.write_buffer(&inp.ibuf, 0, bytemuck::cast_slice(&inp.insts));
            }
            let tv = gpu.array(*u8).view.clone();
            if inp.bind.as_ref().is_none_or(|b| b.1 != *u8 || b.2 != *wid) {
                let wv = warp.create_view(&Default::default());
                let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &gpu.layer_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: inp.ubuf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&tv) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&wv) },
                    ],
                });
                inp.bind = Some((bg, *u8, *wid));
            }
            let draw = (!inp.insts.is_empty()).then(|| LayerDraw {
                bind: inp.bind.as_ref().unwrap().0.clone(),
                ibuf: inp.ibuf.clone(),
                verts: 6 * lu.n * lu.n,
                insts: inp.insts.len() as u32,
            });
            passes.push((self.targets[k].1.clone(), draw));
        }
        gpu.queue.write_buffer(&self.cbuf, 0, bytemuck::bytes_of(cu));
        let lv = self.lut.create_view(&Default::default());
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 0, resource: self.cbuf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.lut_sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&lv) },
        ];
        for k in 0..MAX_INPUTS {
            let v = self.targets.get(k).map_or(&gpu.dummy, |t| &t.1);
            entries.push(wgpu::BindGroupEntry { binding: 3 + k as u32, resource: wgpu::BindingResource::TextureView(v) });
        }
        // ponytail: a new composite bind group each frame. Keep it if profiles show the cost.
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &gpu.comp_bgl, entries: &entries });
        let cb = Draw { layer_pipe: gpu.layer_pipe.clone(), passes, pipe, bind };
        Ok(Some(egui_wgpu::Callback::new_paint_callback(rect, cb)))
    }
}

/// Mesh vertices of a tile: 6 per quad, `MESH` x `MESH` quads for a warp that is not affine.
pub fn mesh(affine: bool) -> u32 {
    if affine { 1 } else { MESH }
}

struct LayerDraw {
    bind: wgpu::BindGroup,
    ibuf: wgpu::Buffer,
    verts: u32,
    insts: u32,
}

struct Draw {
    layer_pipe: wgpu::RenderPipeline,
    passes: Vec<(wgpu::TextureView, Option<LayerDraw>)>,
    pipe: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
}

impl egui_wgpu::CallbackTrait for Draw {
    fn prepare(
        &self,
        _: &wgpu::Device,
        _: &wgpu::Queue,
        _: &egui_wgpu::ScreenDescriptor,
        enc: &mut wgpu::CommandEncoder,
        _: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // One pass for each input: clear to NO_DATA, then all visible tiles in one instanced draw.
        for (target, draw) in &self.passes {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("layer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: NO_DATA as f64, g: 0.0, b: 0.0, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(d) = draw {
                pass.set_pipeline(&self.layer_pipe);
                pass.set_bind_group(0, &d.bind, &[]);
                pass.set_vertex_buffer(0, d.ibuf.slice(..));
                pass.draw(0..d.verts, 0..d.insts);
            }
        }
        vec![]
    }

    fn paint(&self, _: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, _: &egui_wgpu::CallbackResources) {
        pass.set_pipeline(&self.pipe);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..3, 0..1);
    }
}
