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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const T: u32 = TILE as u32;
/// Maximum number of tiles in one draw call.
pub const MAX_DRAWS: usize = 8192;
/// Maximum number of inputs of a composite.
pub const MAX_INPUTS: usize = 8;
/// Value of an offscreen target where no layer pixel is. The composite discards it.
pub const NO_DATA: f32 = -3.0e38;
/// Rows of the color map texture: one for each layer (4) and one for the difference.
pub const LUT_ROWS: u32 = 5;
/// Subdivisions of each side of a tile when the warp is not affine.
const MESH: u32 = 8;
/// Subdivisions of each side of a tile on the globe: a tile can be a large part of the sphere.
pub const GLOBE_MESH: u32 = 16;
/// Globe camera: 1 / tan(field of view / 2), for a vertical field of view of 45 degrees.
pub const GLOBE_F: f64 = 2.414213562373095;
/// WGS84: square of the first eccentricity.
pub const WGS84_E2: f64 = 0.0066943799901413165;

const LAYER_SHADER: &str = r#"
struct U {
    off: vec2f, scale: f32, n: u32,
    view: vec2f, a: f32, b: f32,
    wsize: vec2f, fill: f32, flags: u32,
    grid: vec2u, lat0: f32, dist: f32,
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
    @location(3) facing: f32,
};

const GLOBE_F: f32 = 2.4142135;
const E2: f32 = 0.00669438;

// Globe view. `rel`: longitude and latitude minus those of the view center, in radians. The camera is
// above the view center and looks down, north is up. Result: the position east, north and up of the view
// center point on the WGS84 ellipsoid (unit: the equatorial radius), and a value that is positive if the
// surface at the point faces the camera (the far side of the globe is not drawn).
// ponytail: f32 positions give about 0.5 m. Compute the position relative to the camera for more zoom.
fn globe(rel: vec2f) -> vec4f {
    let s0 = sin(u.lat0);
    let c0 = cos(u.lat0);
    let lat = u.lat0 + rel.y;
    let s = sin(lat);
    let c = cos(lat);
    let n0 = 1.0 / sqrt(1.0 - E2 * s0 * s0);
    let n = 1.0 / sqrt(1.0 - E2 * s * s);
    // Earth-centered coordinates, with the meridian of the view center at longitude 0.
    let normal = vec3f(c * cos(rel.x), c * sin(rel.x), s);
    let p = vec3f(n * normal.x, n * normal.y, n * (1.0 - E2) * s);
    let p0 = vec3f(n0 * c0, 0.0, n0 * (1.0 - E2) * s0);
    let up = vec3f(c0, 0.0, s0);
    let q = p - p0;
    return vec4f(q.y, c0 * q.z - s0 * q.x, dot(up, q), dot(normal, p0 + up * u.dist - p));
}

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
    // uvl.w: the tile is at this distance to the east, in display units (a layer that repeats in longitude).
    let w = warped(mix(rect.xy, rect.zw, t)) + u.off + vec2f(uvl.w, 0.0);
    if ((u.flags & 8u) != 0u) {
        // A point outside the domain of the projection is not on the globe.
        if (abs(w.x) > 1.0e4 || abs(w.y) > 1.0e4) {
            return VO(vec4f(0.0, 0.0, 0.0, 1.0), t * uvl.xy, uvl.xy - vec2f(0.5 / 512.0), u32(uvl.z), -1.0);
        }
        let g = globe(w * 0.017453293);
        return VO(vec4f(g.x * GLOBE_F * u.view.y / u.view.x, g.y * GLOBE_F, 0.0, u.dist - g.z), t * uvl.xy, uvl.xy - vec2f(0.5 / 512.0), u32(uvl.z), g.w);
    }
    let d = w * u.scale / (u.view * 0.5);
    return VO(vec4f(d.x, d.y, 0.0, 1.0), t * uvl.xy, uvl.xy - vec2f(0.5 / 512.0), u32(uvl.z), 1.0);
}

@fragment fn fs(v: VO) -> @location(0) vec4f {
    let s = textureSample(tiles, smp, min(v.uv, v.uvmax), v.layer).r;
    if (v.facing < 0.0) { discard; }
    // NaN is no data. Test the bits: a compiler can remove the test s != s.
    if ((bitcast<u32>(s) & 0x7fffffffu) > 0x7f800000u) { discard; }
    if ((u.flags & 4u) != 0u && abs(s - u.fill) < 0.5 / 255.0) { discard; }
    return vec4f(s * u.a + u.b, 0.0, 0.0, 1.0);
}
"#;

/// Composite shader. `LAYERS` is replaced with the code of the layers and of the compare mode (it sets `col`).
const COMPOSITE_SHADER: &str = r#"
struct L {
    lo: vec4f, hi: vec4f, gamma: vec4f,
    flags: u32, opacity: f32, p0: f32, p1: f32,
};
struct C {
    vo: vec2f, cmp: u32, n: u32,
    swipe: f32, vertical: u32, show_b: u32, diff: u32,
    dlo: f32, dhi: f32, dflags: u32, p0: u32,
    l: array<L, 4>,
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

// Stretch of channel c of layer k to 0..1: dB (flag bit c), limits, gamma.
fn stretch(x: f32, k: u32, c: u32) -> f32 {
    let l = u.l[k];
    let y = select(x, 6.0206 * log2(max(abs(x), 1e-10)), (l.flags & (1u << c)) != 0u);
    return pow(clamp((y - l.lo[c]) / (l.hi[c] - l.lo[c]), 0.0, 1.0), l.gamma[c]);
}

// Color map row `row` of the LUT (rows 0 to 3: layers, row 4: difference).
fn cmap(t: f32, row: u32, inv: bool) -> vec3f {
    let s = select(t, 1.0 - t, inv);
    return textureSampleLevel(lut, smp, vec2f(s * (255.0 / 256.0) + 0.5 / 256.0, (f32(row) + 0.5) / 5.0), 0.0).rgb;
}

fn nd(x: f32) -> bool {
    return x <= -1.0e38;
}

fn finite(x: f32) -> bool {
    let b = bitcast<u32>(x) & 0x7fffffffu;
    return b < 0x7f800000u;
}

fn fin(x: f32) -> f32 {
    return select(0.0, x, finite(x));
}

// src over dst (straight alpha).
fn over(dst: vec4f, src: vec4f) -> vec4f {
    let a = src.a + dst.a * (1.0 - src.a);
    if (a <= 0.0) { return vec4f(0.0); }
    return vec4f((src.rgb * src.a + dst.rgb * dst.a * (1.0 - src.a)) / a, a);
}

@fragment fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let pf = pos.xy - u.vo;
    let p = vec2i(pf);
    let v0 = textureLoad(i0, p, 0).r;
    let v1 = textureLoad(i1, p, 0).r;
    let v2 = textureLoad(i2, p, 0).r;
    let v3 = textureLoad(i3, p, 0).r;
    let v4 = textureLoad(i4, p, 0).r;
    let v5 = textureLoad(i5, p, 0).r;
    let v6 = textureLoad(i6, p, 0).r;
    let v7 = textureLoad(i7, p, 0).r;
    var col = vec4f(0.0);
    LAYERS
    if (col.a <= 0.0) { discard; }
    return col;
}
"#;

/// How a layer makes its color. The strings are WGSL expressions of the input values v0 .. v7 of the view
/// (see `bandmath`).
#[derive(Clone, PartialEq, Debug)]
pub enum Mode {
    /// One value with the color map.
    Gray(String),
    /// Red, green and blue. Each has its own stretch.
    Rgb([String; 3]),
    /// A vector field: the speed with the color map, and arrows in the direction of the components
    /// `u` (to the east) and `v` (to the north).
    Wind { speed: String, u: String, v: String },
}

impl Mode {
    /// Expression of the value of the layer: the gray value, or the red value.
    fn value(&self) -> &str {
        match self {
            Mode::Gray(e) => e,
            Mode::Rgb(e) => &e[0],
            Mode::Wind { speed, .. } => speed,
        }
    }
}

/// Code of the arrows of wind layer `k`: one arrow for each cell of the screen, with the vector at the
/// center of the cell (`cu`, `cv`: the components, expressions of the values c0 .. c7 at the center).
/// `p0` of the layer is the size of a cell in pixels, and `p1` the phase of a light pulse that goes from
/// the tail to the head (0 to 1). The view has north up: the code does not turn the arrows for the
/// convergence of a projection, or on the globe.
fn arrows(k: usize, cu: &str, cv: &str, nd: &str) -> String {
    let loads: String = (0..MAX_INPUTS).map(|j| format!("let c{j} = textureLoad(i{j}, q, 0).r;\n            ")).collect();
    format!(
        "let cell = max(u.l[{k}].p0, 8.0);
            let ctr = (floor(pf / cell) + 0.5) * cell;
            let q = vec2i(ctr);
            {loads}let au = f32({cu});
            let av = f32({cv});
            let sp = length(vec2f(au, av));
            if (!({nd}) && finite(sp) && sp > 0.0) {{
                // The y axis of the screen goes down.
                let dir = vec2f(au, -av) / sp;
                let len = cell * 0.44 * clamp(stretch(sp, {k}u, 0u), 0.2, 1.0);
                let d = pf - ctr;
                let along = dot(d, dir);
                let across = abs(dot(d, vec2f(-dir.y, dir.x)));
                let head = cell * 0.3;
                let back = len - along;
                let w = cell / 30.0;
                // The arrow: a shaft, and a head that is a triangle. `m` is the distance to its edge: the
                // arrow is where m > 0, and a dark line 1.6 w wide goes around it.
                let shaft = min(1.2 * w - across, min(along + len, back - head * 0.8));
                let tip = min(back * 0.62 - across, min(back, head - back));
                let m = max(shaft, tip);
                let fill = smoothstep(-0.6, 0.6, m);
                let line = smoothstep(-0.6, 0.6, m + 1.6 * w);
                let pulse = 0.84 + 0.16 * cos(6.2831853 * ((along + len) / (2.0 * len) - u.l[{k}].p1));
                col = over(col, vec4f(vec3f(0.0), 0.7 * line * u.l[{k}].opacity));
                col = over(col, vec4f(vec3f(pulse), fill * u.l[{k}].opacity));
            }}"
    )
}

/// One layer of a view: its mode and the inputs (view input indices) that it uses.
#[derive(Clone, PartialEq, Debug)]
pub struct LayerSpec {
    pub mode: Mode,
    pub inputs: Vec<usize>,
}

/// Compare mode of the first two layers (A = layer 0, B = layer 1).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Compare {
    /// All layers, each over the layers below it with its opacity (the blend mode is this with the opacity of B).
    #[default]
    Stack,
    /// B only on one side of a line.
    Swipe,
    /// A or B, one after the other.
    Flicker,
    /// A - B, A / B or 10 log10(A / B) with the difference color map.
    Difference,
}

/// Fragment code of a composite.
fn program(layers: &[LayerSpec], cmp: Compare) -> String {
    let nd = |ins: &[usize]| if ins.is_empty() { "false".to_string() } else { ins.iter().map(|k| format!("nd(v{k})")).collect::<Vec<_>>().join(" || ") };
    if cmp == Compare::Difference && layers.len() >= 2 {
        let (a, b) = (&layers[0], &layers[1]);
        return format!(
            "if (!({}) && !({})) {{
        let xa = f32({});
        let xb = f32({});
        let d = select(select(xa - xb, xa / xb, u.diff == 1u), 10.0 * log2(xa / xb) * 0.30103, u.diff == 2u);
        if (finite(d)) {{
            let t = clamp((d - u.dlo) / (u.dhi - u.dlo), 0.0, 1.0);
            col = vec4f(cmap(t, 4u, (u.dflags & 8u) != 0u), 1.0);
        }}
    }}",
            nd(&a.inputs),
            nd(&b.inputs),
            a.mode.value(),
            b.mode.value()
        );
    }
    let mut out = String::new();
    for (k, l) in layers.iter().enumerate().take(4) {
        let show = match (cmp, k) {
            (Compare::Swipe, 1) => "select(pf.y, pf.x, u.vertical != 0u) > u.swipe",
            (Compare::Flicker, 0) => "u.show_b == 0u",
            (Compare::Flicker, 1) => "u.show_b == 1u",
            _ => "true",
        };
        let color = match &l.mode {
            Mode::Gray(e) => format!(
                "let x = f32({e});
            if (finite(x)) {{
                col = over(col, vec4f(cmap(stretch(x, {k}u, 0u), {k}u, (u.l[{k}].flags & 8u) != 0u), u.l[{k}].opacity));
            }}"
            ),
            Mode::Rgb(e) => format!(
                "let c = vec3f(stretch(fin(f32({})), {k}u, 0u), stretch(fin(f32({})), {k}u, 1u), stretch(fin(f32({})), {k}u, 2u));
            col = over(col, vec4f(c, u.l[{k}].opacity));",
                e[0], e[1], e[2]
            ),
            Mode::Wind { speed, u, v } => {
                // The same expressions with the values at the center of the cell.
                let center = |e: &str| (0..MAX_INPUTS).fold(e.to_string(), |s, j| s.replace(&format!("v{j}"), &format!("c{j}")));
                let ndc = l.inputs.iter().map(|j| format!("nd(c{j})")).collect::<Vec<_>>().join(" || ");
                format!(
                    "let x = f32({speed});
            if (finite(x)) {{
                col = over(col, vec4f(cmap(stretch(x, {k}u, 0u), {k}u, (u.l[{k}].flags & 8u) != 0u), u.l[{k}].opacity));
            }}
            {}",
                    arrows(k, &center(u), &center(v), if ndc.is_empty() { "false" } else { &ndc })
                )
            }
        };
        out += &format!("// layer {k}\n    if ({show} && !({})) {{\n            {color}\n    }}\n    ", nd(&l.inputs));
    }
    out
}

/// Parameters of one layer of the composite.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LayerParams {
    pub lo: [f32; 4],
    pub hi: [f32; 4],
    pub gamma: [f32; 4],
    /// Bit c: channel c in dB. Bit 3: invert the color map.
    pub flags: u32,
    pub opacity: f32,
    /// A wind layer: the size in pixels of the cell of an arrow, and the phase of the pulse of the arrows.
    pub pad: [f32; 2],
}

impl Default for LayerParams {
    fn default() -> Self {
        LayerParams { lo: [0.0; 4], hi: [1.0; 4], gamma: [1.0; 4], flags: 0, opacity: 1.0, pad: [0.0; 2] }
    }
}

/// Uniforms of the composite pass.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CompositeUniforms {
    /// View origin in window pixels.
    pub vo: [f32; 2],
    pub cmp: u32,
    pub n: u32,
    /// Swipe line position in view pixels, and 1 for a vertical line.
    pub swipe: f32,
    pub vertical: u32,
    /// Flicker: 1 when B is shown.
    pub show_b: u32,
    /// Difference: 0 A - B, 1 A / B, 2 10 log10(A / B).
    pub diff: u32,
    pub dlo: f32,
    pub dhi: f32,
    /// Bit 3: invert the difference color map.
    pub dflags: u32,
    pub pad: u32,
    pub l: [LayerParams; 4],
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
    /// 4: u8 fill value is no data. 8: globe view (the display coordinates are longitude and latitude).
    pub flags: u32,
    pub grid: [u32; 2],
    /// Globe view: latitude of the view center in radians, and distance of the camera from the surface
    /// in equatorial radii.
    pub lat0: f32,
    pub dist: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Inst {
    /// Tile corners in level-0 pixels.
    pub rect: [f32; 4],
    /// uv max x, uv max y, array layer, and the distance of the tile to the east in display units (a copy
    /// of a layer that repeats in longitude).
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
    /// Linear when magnified: smooth pixels (a view with `View2d::smooth`).
    smooth: wgpu::Sampler,
    lut_sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    arrays: [Option<TileArray>; 2],
    layers: u32,
    /// Frame counter for the LRU order. The application increments it.
    pub frame: u64,
    /// Bytes of the offscreen targets of all views.
    pub target_bytes: usize,
    /// Staging buffers for tile uploads. A buffer is ready when it is mapped again after its copy.
    staging: Vec<(wgpu::Buffer, Arc<AtomicBool>)>,
}

/// Size of one staging buffer. The application uploads at most about this much in one frame.
pub const STAGING: u64 = 12 << 20;
/// Time limit of the tile copy in one frame.
const UPLOAD_TIME: std::time::Duration = std::time::Duration::from_millis(3);
/// Maximum number of staging buffers.
const STAGING_BUFFERS: usize = 3;

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
        let smooth = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
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
            smooth,
            lut_sampler,
            dummy,
            arrays: [None, None],
            layers,
            frame: 0,
            target_bytes: 0,
            staging: vec![],
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

    /// Composite pipeline of layers and a compare mode. Errors come from the WGSL compiler (band math).
    fn composite(&mut self, layers: &[LayerSpec], cmp: Compare) -> Result<wgpu::RenderPipeline, String> {
        let src = COMPOSITE_SHADER.replace("LAYERS", &program(layers, cmp));
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
                // Layers with an opacity below 1 over no data: blend with the background of the window.
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
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

    /// Array layer of a resident tile. This does not mark the tile as used: the tile can leave the array
    /// at the next upload.
    pub fn peek(&self, key: &TileKey, u8: bool) -> Option<u32> {
        Some(self.arrays[u8 as usize].as_ref()?.map.get(key)?.layer)
    }

    /// Number of tiles that each tile array can hold.
    pub fn capacity(&self) -> usize {
        self.layers as usize
    }

    /// Array layer for a tile. If the array is full, remove the least recently used tile that is not
    /// used in this frame or the previous frame. None if there is no free layer.
    fn slot(&mut self, key: TileKey, u8: bool, done: bool) -> Option<u32> {
        let frame = self.frame;
        let a = self.array(u8);
        if let Some(s) = a.map.get_mut(&key) {
            s.done = done;
            return Some(s.layer);
        }
        let layer = match a.free.pop() {
            Some(l) => l,
            None => {
                // ponytail: O(n) scan for the oldest tile. Use an ordered index if arrays get much larger than 2048 layers.
                let (&k, s) = a.map.iter().filter(|(_, s)| s.used + 1 < frame).min_by_key(|(_, s)| s.used)?;
                let l = s.layer;
                a.map.remove(&k);
                l
            }
        };
        a.map.insert(key, Slot { layer, done, used: frame });
        Some(layer)
    }

    /// A mapped staging buffer: a ready one of the ring, or a new one if the ring is not full.
    fn staging(&mut self) -> Option<wgpu::Buffer> {
        let _ = self.device.poll(wgpu::PollType::Poll);
        if let Some((b, r)) = self.staging.iter().find(|(_, r)| r.load(Ordering::Acquire)) {
            r.store(false, Ordering::Release);
            return Some(b.clone());
        }
        if self.staging.len() >= STAGING_BUFFERS {
            return None;
        }
        let b = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile upload"),
            size: STAGING,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::MAP_WRITE,
            mapped_at_creation: true,
        });
        self.staging.push((b.clone(), Arc::new(AtomicBool::new(false))));
        Some(b)
    }

    /// Put tiles (key, width, height, pixels, complete) on the GPU through one staging buffer of a ring:
    /// one submit for all tiles of a frame, and no new allocation. The copy stops after `UPLOAD_TIME`.
    /// Return the number of tiles uploaded (the first ones): the others wait for the next frame.
    pub fn upload(&mut self, tiles: &[(TileKey, u32, u32, &Pixels, bool)]) -> usize {
        if tiles.is_empty() {
            return 0;
        }
        let t0 = std::time::Instant::now();
        let Some(buf) = self.staging() else { return 0 };
        let mut placed = vec![];
        let mut size = 0u64;
        {
            let mut m = buf.slice(..).get_mapped_range_mut().expect("staging buffer is mapped");
            for &(key, w, h, px, done) in tiles {
                let u8 = matches!(px, Pixels::U8(_));
                let row = w as u64 * if u8 { 1 } else { 2 };
                let prow = row.div_ceil(256) * 256;
                if size + prow * h as u64 > STAGING || t0.elapsed() > UPLOAD_TIME {
                    break;
                }
                let src = px.bytes();
                for r in 0..h as usize {
                    let d = (size + r as u64 * prow) as usize;
                    m.slice(d..d + row as usize).copy_from_slice(&src[r * row as usize..(r + 1) * row as usize]);
                }
                placed.push((key, u8, done, w, h, prow, size));
                size += prow * h as u64;
            }
        }
        buf.unmap();
        let mut enc = self.device.create_command_encoder(&Default::default());
        for &(key, u8, done, w, h, prow, off) in &placed {
            let Some(layer) = self.slot(key, u8, done) else { continue };
            let tex = &self.arrays[u8 as usize].as_ref().unwrap().tex;
            enc.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout { offset: off, bytes_per_row: Some(prow as u32), rows_per_image: None },
                },
                wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: 0, z: layer }, aspect: wgpu::TextureAspect::All },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        self.queue.submit([enc.finish()]);
        let ready = self.staging.iter().find(|(b, _)| *b == buf).map(|(_, r)| r.clone()).unwrap();
        buf.map_async(wgpu::MapMode::Write, .., move |r| ready.store(r.is_ok(), Ordering::Release));
        placed.len()
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
    /// Bind group, with its tile array (u8), warp and sampler (smooth).
    bind: Option<(wgpu::BindGroup, bool, u64, bool)>,
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
    /// Magnified pixels are smooth (linear), not squares. Data at a low resolution, for example a
    /// weather model. The tiles do not have the pixels of the next tiles: their edges show.
    pub smooth: bool,
}

impl View2d {
    pub fn new(gpu: &Gpu) -> View2d {
        let lut = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lut"),
            size: wgpu::Extent3d { width: 256, height: LUT_ROWS, depth_or_array_layers: 1 },
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
        View2d { lut, cbuf, targets: vec![], size: (0, 0), inputs: vec![], smooth: false }
    }

    /// Color map of row `row`: 0 to 3 for the layers, 4 for the difference.
    pub fn set_lut(&self, gpu: &Gpu, row: u32, rgba: &[[u8; 4]]) {
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.lut, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: row.min(LUT_ROWS - 1), z: 0 }, aspect: wgpu::TextureAspect::All },
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
    /// `inputs[k]` are the uniforms and the u8 flag of input k. `specs` are the layers of the composite.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        gpu: &mut Gpu,
        inputs: &[(LayerUniforms, bool)],
        specs: &[LayerSpec],
        cmp: Compare,
        cu: &CompositeUniforms,
        rect: egui::Rect,
        px: (u32, u32),
    ) -> Result<Option<egui::PaintCallback>, String> {
        let layers = inputs;
        let n = layers.len().min(self.inputs.len()).min(MAX_INPUTS);
        if n == 0 || specs.is_empty() || px.0 == 0 || px.1 == 0 {
            return Ok(None);
        }
        let pipe = gpu.composite(specs, cmp)?;
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
            let smooth = self.smooth;
            if inp.bind.as_ref().is_none_or(|b| b.1 != *u8 || b.2 != *wid || b.3 != smooth) {
                let wv = warp.create_view(&Default::default());
                let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &gpu.layer_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: inp.ubuf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(if smooth { &gpu.smooth } else { &gpu.sampler }) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&tv) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&wv) },
                    ],
                });
                inp.bind = Some((bg, *u8, *wid, smooth));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// All compare modes with gray and RGB layers compile (WGSL front end and validation of naga).
    #[test]
    fn composite_programs_are_valid() {
        let gray = LayerSpec { mode: Mode::Gray("((v0 - v1) / (v0 + v1))".into()), inputs: vec![0, 1] };
        let rgb = LayerSpec { mode: Mode::Rgb(["v2".into(), "v3".into(), "(v2 / v3)".into()]), inputs: vec![2, 3] };
        let wind = LayerSpec { mode: Mode::Wind { speed: "sqrt((pow(v4, 2.0) + pow(v5, 2.0)))".into(), u: "v4".into(), v: "v5".into() }, inputs: vec![4, 5] };
        for cmp in [Compare::Stack, Compare::Swipe, Compare::Flicker, Compare::Difference] {
            for layers in [vec![gray.clone()], vec![gray.clone(), rgb.clone()], vec![rgb.clone(), gray.clone(), gray.clone()], vec![wind.clone()], vec![gray.clone(), wind.clone(), wind.clone()]] {
                let src = COMPOSITE_SHADER.replace("LAYERS", &program(&layers, cmp));
                let m = naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{cmp:?}: {}", e.emit_to_string(&src)));
                naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::default())
                    .validate(&m)
                    .unwrap_or_else(|e| panic!("{cmp:?}: {e:?}"));
            }
        }
        assert_eq!(std::mem::size_of::<CompositeUniforms>(), 48 + 4 * 64);
        // The layer shader (2D and globe).
        let m = naga::front::wgsl::parse_str(LAYER_SHADER).unwrap_or_else(|e| panic!("{}", e.emit_to_string(LAYER_SHADER)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::default()).validate(&m).unwrap();
        assert_eq!(std::mem::size_of::<LayerUniforms>(), 64);
    }
}
