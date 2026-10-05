//! Conversion of decoded chunks to display-ready planes. u8 data stays u8.
//! Other data goes to f16: stored value = (raw value - off) * k. No data is NaN.
use eo_core::{Array, DType};
use half::f16;
use half::slice::HalfFloatSliceExt;

/// Which value to take from a pixel. For real data, only `Real`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Part {
    Real,
    Amp,
    Phase,
    I,
    Q,
}

impl Part {
    pub const COMPLEX: [(Part, &'static str); 4] =
        [(Part::Amp, "amplitude"), (Part::Phase, "phase"), (Part::I, "I"), (Part::Q, "Q")];
}

/// Display encoding of a layer.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Enc {
    pub u8: bool,
    pub k: f32,
    pub off: f32,
}

pub enum Pixels {
    U8(Vec<u8>),
    F16(Vec<f16>),
}

impl Pixels {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Pixels::U8(v) => v,
            Pixels::F16(v) => bytemuck::cast_slice(v),
        }
    }

    pub fn size(&self) -> usize {
        self.bytes().len()
    }

    /// Plane of `n` values from `bytes()` of a plane. None if the length is not correct.
    pub fn from_bytes(u8: bool, b: &[u8], n: usize) -> Option<Pixels> {
        match u8 {
            true if b.len() == n => Some(Pixels::U8(b.to_vec())),
            false if b.len() == 2 * n => Some(Pixels::F16(b.chunks_exact(2).map(|c| f16::from_ne_bytes([c[0], c[1]])).collect())),
            _ => None,
        }
    }

    /// Plane of `n` no-data values.
    pub fn empty(enc: &Enc, fill: Option<f64>, n: usize) -> Pixels {
        if enc.u8 {
            Pixels::U8(vec![fill.map_or(0, |f| f as u8); n])
        } else {
            Pixels::F16(vec![f16::NAN; n])
        }
    }

    /// Copy a `w` x `h` block from `src` (row length `sw`, start `s0`) to `self` (row length `dw`, start `d0`).
    pub fn copy_from(&mut self, d0: usize, dw: usize, src: &Pixels, s0: usize, sw: usize, w: usize, h: usize) {
        fn go<T: Copy>(d: &mut [T], d0: usize, dw: usize, s: &[T], s0: usize, sw: usize, w: usize, h: usize) {
            for y in 0..h {
                d[d0 + y * dw..][..w].copy_from_slice(&s[s0 + y * sw..][..w]);
            }
        }
        match (self, src) {
            (Pixels::U8(d), Pixels::U8(s)) => go(d, d0, dw, s, s0, sw, w, h),
            (Pixels::F16(d), Pixels::F16(s)) => go(d, d0, dw, s, s0, sw, w, h),
            _ => unreachable!("tile and chunk use the same encoding"),
        }
    }
}

/// Position of the 2D plane of one band and one time step in a decoded chunk, in values.
#[derive(Clone, Copy, Debug)]
pub struct PlaneAt {
    pub base: usize,
    pub sy: usize,
    pub sx: usize,
    pub ch: usize,
    pub cw: usize,
}

/// Region of a chunk plane: rows `r0..r1`, columns `c0..c1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Win {
    pub r0: usize,
    pub r1: usize,
    pub c0: usize,
    pub c1: usize,
}

impl Win {
    pub fn w(&self) -> usize {
        self.c1 - self.c0
    }

    pub fn h(&self) -> usize {
        self.r1 - self.r0
    }
}

impl PlaneAt {
    pub fn full(&self) -> Win {
        Win { r0: 0, r1: self.ch, c0: 0, c1: self.cw }
    }

    /// True if `len` bytes hold all values of region `w`. A short chunk (the last strip of a TIFF) has fewer rows.
    pub fn covers(&self, a: &Array, len: usize, w: &Win) -> bool {
        w.r1 <= w.r0 || w.c1 <= w.c0 || (self.base + (w.r1 - 1) * self.sy + (w.c1 - 1) * self.sx + 1) * a.dtype.size() <= len
    }

    pub fn new(a: &Array, band: u64, time: u64) -> PlaneAt {
        let n = a.dims.len();
        let mut st = vec![1usize; n];
        for i in (0..n.saturating_sub(1)).rev() {
            st[i] = st[i + 1] * a.chunk[i + 1] as usize;
        }
        let (y, x) = (a.axis("y").unwrap(), a.axis("x").unwrap());
        let at = |d: &str, i: u64| a.axis(d).map_or(0, |k| (i % a.chunk[k]) as usize * st[k]);
        let base = at("band", band) + at("time", time);
        PlaneAt { base, sy: st[y], sx: st[x], ch: a.chunk[y] as usize, cw: a.chunk[x] as usize }
    }
}

macro_rules! each_type {
    ($dt:expr, $go:ident) => {
        match $dt.part() {
            DType::U8 => $go!(u8),
            DType::I8 => $go!(i8),
            DType::U16 => $go!(u16),
            DType::I16 => $go!(i16),
            DType::U32 => $go!(u32),
            DType::I32 => $go!(i32),
            DType::U64 => $go!(u64),
            DType::I64 => $go!(i64),
            DType::F32 => $go!(f32),
            _ => $go!(f64),
        }
    };
}

/// Values of region `w` of plane `p` of a decoded chunk, as f32, in rows of `w.w()`.
pub fn to_f32(a: &Array, raw: &[u8], p: &PlaneAt, part: Part, w: &Win, out: &mut [f32]) {
    let es = a.dtype.size();
    macro_rules! go {
        ($t:ty) => {{
            const N: usize = std::mem::size_of::<$t>();
            let get: fn(&[u8]) -> f32 = if a.le {
                |b| <$t>::from_le_bytes(b[..N].try_into().unwrap()) as f32
            } else {
                |b| <$t>::from_be_bytes(b[..N].try_into().unwrap()) as f32
            };
            for y in w.r0..w.r1 {
                let row = &mut out[(y - w.r0) * w.w()..][..w.w()];
                let r0 = p.base + y * p.sy + w.c0 * p.sx;
                for (x, o) in row.iter_mut().enumerate() {
                    let b = &raw[(r0 + x * p.sx) * es..];
                    *o = match part {
                        Part::Real | Part::I => get(b),
                        Part::Q => get(&b[N..]),
                        Part::Amp => get(b).hypot(get(&b[N..])),
                        Part::Phase => get(&b[N..]).atan2(get(b)),
                    };
                }
            }
        }};
    }
    each_type!(a.dtype, go)
}

/// Exact value at value index `i` of a decoded chunk.
pub fn value_f64(a: &Array, raw: &[u8], i: usize, part: Part) -> f64 {
    let b = &raw[i * a.dtype.size()..];
    macro_rules! go {
        ($t:ty) => {{
            const N: usize = std::mem::size_of::<$t>();
            let get = |b: &[u8]| {
                let v = b[..N].try_into().unwrap();
                (if a.le { <$t>::from_le_bytes(v) } else { <$t>::from_be_bytes(v) }) as f64
            };
            match part {
                Part::Real | Part::I => get(b),
                Part::Q => get(&b[N..]),
                Part::Amp => get(b).hypot(get(&b[N..])),
                Part::Phase => get(&b[N..]).atan2(get(b)),
            }
        }};
    }
    each_type!(a.dtype, go)
}

/// Display values of region `w` of plane `p` of a decoded chunk.
pub fn encode(a: &Array, raw: &[u8], p: &PlaneAt, part: Part, enc: &Enc, fill: Option<f64>, w: &Win) -> Pixels {
    let n = w.w() * w.h();
    if enc.u8 {
        let mut v = vec![0u8; n];
        for y in w.r0..w.r1 {
            let r0 = p.base + y * p.sy + w.c0 * p.sx;
            v[(y - w.r0) * w.w()..][..w.w()].iter_mut().enumerate().for_each(|(x, o)| *o = raw[r0 + x * p.sx]);
        }
        return Pixels::U8(v);
    }
    let mut f = vec![0f32; n];
    to_f32(a, raw, p, part, w, &mut f);
    Pixels::F16(encode_f32(&mut f, enc, fill))
}

/// f32 values to f16 display values. Changes `f` in place.
pub fn encode_f32(f: &mut [f32], enc: &Enc, fill: Option<f64>) -> Vec<f16> {
    let fill = fill.map(|v| v as f32);
    for v in f.iter_mut() {
        *v = if !v.is_finite() || Some(*v) == fill { f32::NAN } else { (*v - enc.off) * enc.k };
    }
    let mut out = vec![f16::ZERO; f.len()];
    out.convert_from_f32_slice(f);
    out
}

/// Encoding for a sorted sample of raw values. Keep the stored values far below the f16 limit (65504),
/// and away from the subnormal range. Remove a large common offset (for example temperatures in kelvin).
pub fn choose_enc(dtype: DType, part: Part, sample: &[f32]) -> Enc {
    if dtype == DType::U8 && part == Part::Real {
        return Enc { u8: true, k: 1.0, off: 0.0 };
    }
    let (Some(&lo), Some(&hi)) = (sample.get(sample.len() / 1000), sample.get(sample.len() * 999 / 1000)) else {
        return Enc { u8: false, k: 1.0, off: 0.0 };
    };
    // A large common offset goes to the middle of the range: the f16 step is then span / 4096.
    let (span, mid) = (hi - lo, (lo + hi) / 2.0);
    let off = if mid.abs() > span { mid } else { 0.0 };
    let m = (lo - off).abs().max((hi - off).abs());
    let k = if m > 0.0 && m.is_finite() { 1024.0 / m } else { 1.0 };
    Enc { u8: false, k, off }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eo_core::ChunkLoc;

    #[test]
    fn plane_of_interleaved_band() {
        // 2 x 3 pixels, 2 bands, pixel interleaved, big-endian u16.
        let a = Array {
            dims: vec!["y".into(), "x".into(), "band".into()],
            shape: vec![2, 3, 2],
            chunk: vec![2, 3, 2],
            dtype: DType::U16,
            le: false,
            codecs: vec![],
            chunks: vec![ChunkLoc { src: 0, off: 0, len: 24 }],
            place: None,
        };
        let raw: Vec<u8> = (0u16..12).flat_map(|v| v.to_be_bytes()).collect();
        let p = PlaneAt::new(&a, 1, 0);
        let mut out = [0f32; 6];
        to_f32(&a, &raw, &p, Part::Real, &p.full(), &mut out);
        assert_eq!(out, [1., 3., 5., 7., 9., 11.]);
        let mut out = [0f32; 2];
        to_f32(&a, &raw, &p, Part::Real, &Win { r0: 1, r1: 2, c0: 1, c1: 3 }, &mut out);
        assert_eq!(out, [9., 11.]);
        assert_eq!(value_f64(&a, &raw, 5, Part::Real), 5.0);
    }

    #[test]
    fn enc_keeps_precision() {
        let s: Vec<f32> = (0..1000).map(|i| 270.0 + i as f32 * 0.04).collect();
        let e = choose_enc(DType::F32, Part::Real, &s);
        let mut v = [290.0f32, 290.01];
        let h = encode_f32(&mut v, &e, None);
        let back: Vec<f32> = h.iter().map(|x| x.to_f32() / e.k + e.off).collect();
        // Span 40: the f16 step is 40 / 4096.
        assert!((back[0] - 290.0).abs() < 0.01 && (back[1] - 290.01).abs() < 0.01, "{back:?}");
    }
}
