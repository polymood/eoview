//! wgpu renderer. The GPU keeps 512 x 512 tiles in texture arrays, with LRU eviction.
//! A view draws all its visible tiles in one instanced draw call. The fragment shader applies
//! the stretch, the gamma, the dB scale and the color map: a display change does not load data again.
use eo_cache::{Pixels, TILE, TileKey};
use std::collections::HashMap;

const T: u32 = TILE as u32;
/// Maximum number of tiles in one draw call.
pub const MAX_DRAWS: usize = 8192;

pub const SHADER: &str = r#"
struct U {
    view: vec2f, scale: f32, gamma: f32,
    lo: f32, hi: f32, a: f32, b: f32,
    fill: f32, flags: u32, p0: f32, p1: f32,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var tiles: texture_2d_array<f32>;
@group(0) @binding(3) var lut: texture_2d<f32>;

struct VO {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
    @location(1) @interpolate(flat) uvmax: vec2f,
    @location(2) @interpolate(flat) layer: u32,
};

// rect: tile corners in level-0 pixels, relative to the view center.
@vertex fn vs(@builtin(vertex_index) i: u32, @location(0) rect: vec4f, @location(1) uvl: vec4f) -> VO {
    let t = vec2f(f32(i & 1u), f32(i >> 1u));
    let ndc = mix(rect.xy, rect.zw, t) * u.scale / (u.view * 0.5);
    return VO(vec4f(ndc.x, -ndc.y, 0.0, 1.0), t * uvl.xy, uvl.xy - vec2f(0.5 / 512.0), u32(uvl.z));
}

@fragment fn fs(v: VO) -> @location(0) vec4f {
    let s = textureSample(tiles, smp, min(v.uv, v.uvmax), v.layer).r;
    // NaN is no data. Test the bits: a compiler can remove the test s != s.
    if ((bitcast<u32>(s) & 0x7fffffffu) > 0x7f800000u) { discard; }
    if ((u.flags & 4u) != 0u && abs(s - u.fill) < 0.5 / 255.0) { discard; }
    let phys = s * u.a + u.b;
    // 20 log10(x) = 6.0206 log2(x)
    let x = select(phys, 6.0206 * log2(max(abs(phys), 1e-10)), (u.flags & 1u) != 0u);
    var t = pow(clamp((x - u.lo) / (u.hi - u.lo), 0.0, 1.0), u.gamma);
    t = select(t, 1.0 - t, (u.flags & 2u) != 0u);
    let c = textureSampleLevel(lut, smp, vec2f(t * (255.0 / 256.0) + 0.5 / 256.0, 0.5), 0.0);
    return vec4f(c.rgb, 1.0);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    /// View size in physical pixels.
    pub view: [f32; 2],
    /// Physical pixels for each level-0 pixel.
    pub scale: f32,
    pub gamma: f32,
    pub lo: f32,
    pub hi: f32,
    /// Physical value = texel * a + b.
    pub a: f32,
    pub b: f32,
    pub fill: f32,
    /// 1: dB. 2: invert. 4: u8 fill value is no data.
    pub flags: u32,
    pub pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Inst {
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

/// Shared GPU state: the pipeline and the tile arrays (one for u8 tiles, one for f16 tiles).
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    arrays: [Option<TileArray>; 2],
    layers: u32,
    /// Frame counter for the LRU order. The application increments it.
    pub frame: u64,
}

impl Gpu {
    /// `budget` is the GPU memory budget in bytes for the tile arrays.
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, format: wgpu::TextureFormat, budget: usize) -> Gpu {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let tex = |binding, dim| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: dim,
                multisampled: false,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex(2, wgpu::TextureViewDimension::D2Array),
                tex(3, wgpu::TextureViewDimension::D2),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("tiles"),
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
                targets: &[Some(format.into())],
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
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
        // The u8 array gets 1/3 of the budget and the f16 array 2/3: both have the same number of layers.
        let layers = device.limits().max_texture_array_layers.min((budget / (3 * TILE as usize * TILE as usize)).max(1) as u32);
        Gpu { device, queue, pipeline, bgl, sampler, arrays: [None, None], layers, frame: 0 }
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
            let view = tex.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
            TileArray { tex, view, bpp: if u8 { 1 } else { 2 }, map: HashMap::new(), free: (0..layers).rev().collect() }
        })
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
            wgpu::TexelCopyTextureInfo {
                texture: &a.tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: layer },
                aspect: wgpu::TextureAspect::All,
            },
            px.bytes(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * a.bpp), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        true
    }

    /// Allocated bytes of the tile arrays, and bytes of the resident tiles.
    pub fn usage(&self) -> (usize, usize) {
        let t = (T * T) as usize;
        self.arrays.iter().flatten().fold((0, 0), |(a, r), x| {
            let b = t * x.bpp as usize;
            (a + b * self.layers as usize, r + b * x.map.len())
        })
    }

    pub fn resident(&self) -> usize {
        self.arrays.iter().flatten().map(|a| a.map.len()).sum()
    }
}

/// GPU resources of one 2D view.
pub struct View2d {
    ubuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    lut: wgpu::Texture,
    bind: [Option<wgpu::BindGroup>; 2],
    /// Instances of the next draw. The application fills it; the capacity stays between frames.
    pub insts: Vec<Inst>,
}

impl View2d {
    pub fn new(gpu: &Gpu) -> View2d {
        let buf = |size, usage| gpu.device.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage, mapped_at_creation: false });
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
        View2d {
            ubuf: buf(std::mem::size_of::<Uniforms>() as u64, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST),
            ibuf: buf((MAX_DRAWS * std::mem::size_of::<Inst>()) as u64, wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST),
            lut,
            bind: [None, None],
            insts: Vec::with_capacity(MAX_DRAWS),
        }
    }

    pub fn set_lut(&self, gpu: &Gpu, rgba: &[[u8; 4]]) {
        gpu.queue.write_texture(
            self.lut.as_image_copy(),
            bytemuck::cast_slice(rgba),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256 * 4), rows_per_image: None },
            wgpu::Extent3d { width: 256, height: 1, depth_or_array_layers: 1 },
        );
    }

    /// Write the uniforms and the instances, and make the paint callback for egui.
    pub fn paint(&mut self, gpu: &mut Gpu, u8: bool, u: &Uniforms, rect: egui::Rect) -> Option<egui::PaintCallback> {
        self.insts.truncate(MAX_DRAWS);
        if self.insts.is_empty() {
            return None;
        }
        gpu.queue.write_buffer(&self.ubuf, 0, bytemuck::bytes_of(u));
        gpu.queue.write_buffer(&self.ibuf, 0, bytemuck::cast_slice(&self.insts));
        let tv = &gpu.array(u8).view.clone();
        let bind = self.bind[u8 as usize].get_or_insert_with(|| {
            let lv = self.lut.create_view(&Default::default());
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &gpu.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.ubuf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(tv) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&lv) },
                ],
            })
        });
        let cb = Draw { pipeline: gpu.pipeline.clone(), bind: bind.clone(), ibuf: self.ibuf.clone(), n: self.insts.len() as u32 };
        Some(egui_wgpu::Callback::new_paint_callback(rect, cb))
    }
}

struct Draw {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    ibuf: wgpu::Buffer,
    n: u32,
}

impl egui_wgpu::CallbackTrait for Draw {
    fn paint(&self, _: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, _: &egui_wgpu::CallbackResources) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.ibuf.slice(..));
        pass.draw(0..4, 0..self.n);
    }
}
