//! Filters: the operation of a computed layer as data (a name and its parameters), and the functions that
//! make the values of a window from the values of its inputs. The engine reads the inputs with a halo (the
//! pixels around the window that the filter uses), across the edges of the tiles: there are no seams. At the
//! edge of the image, the edge pixels repeat. NaN is no data: a pixel without data stays without data, and
//! the means, the medians and the minimums use only the pixels with data.
//!
//! The parameters in pixels (radius, sigma, box size) are in pixels of level 0. At a coarse level, the
//! filter uses the same size on the ground: the result is near the result at level 0, reduced. An export
//! and the inspector use level 0: they are exact.
use serde::{Deserialize, Serialize};

/// The largest halo in pixels of a level.
const MAX_HALO: usize = 64;

/// A filter and its parameters. In a file: `{"op": "gaussian", "sigma": 2.0}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Filter {
    /// v * k + o.
    Scale { k: f64, o: f64 },
    /// 10 log10(v). No data where v <= 0.
    Db,
    /// (a - b) / (a + b) of two inputs: NDVI, NDWI, NBR, NDSI, NDMI.
    NormDiff,
    /// Soil adjusted vegetation index of NIR and red: (1 + l) (nir - red) / (nir + red + l).
    Savi { l: f64 },
    /// Enhanced vegetation index of NIR, red and blue: 2.5 (nir - red) / (nir + 6 red - 7.5 blue + 1).
    Evi,
    /// A mask: 1 where lo <= v <= hi, else 0.
    Threshold { lo: f64, hi: f64 },
    /// Gaussian smoothing, `sigma` in pixels.
    Gaussian { sigma: f64 },
    /// The mean of a box of `w` x `h` pixels (boxcar speckle filter, multilook on the same grid).
    Boxcar { w: f64, h: f64 },
    /// The median of a square of 2 r + 1 pixels.
    Median { r: f64 },
    /// Morphology on a square of 2 r + 1 pixels: minimum, maximum, maximum of the minimum, minimum of the maximum.
    Erode { r: f64 },
    Dilate { r: f64 },
    Open { r: f64 },
    Close { r: f64 },
    /// The magnitude of the gradient (Sobel), in value units for each pixel of level 0.
    Sobel,
    /// The Laplacian (4 neighbors), for each pixel of level 0.
    Laplace,
    /// Lee speckle filter on a square of 2 r + 1 pixels, for an image of intensity with `looks` looks.
    Lee { r: f64, looks: f64 },
    /// Light on a DEM from the direction `azimuth` (degrees from north, clockwise) at `altitude` degrees
    /// above the horizon: 0 (shadow) to 1. `z`: the vertical exaggeration.
    Hillshade { azimuth: f64, altitude: f64, z: f64 },
    /// The slope of a DEM in degrees. `z`: the factor from the units of the values to meters.
    Slope { z: f64 },
    /// The direction to which the slope of a DEM faces, in degrees from north, clockwise. No data where it is flat.
    Aspect,
}

impl Filter {
    /// The number of inputs.
    pub fn inputs(&self) -> usize {
        match self {
            Filter::NormDiff | Filter::Savi { .. } => 2,
            Filter::Evi => 3,
            _ => 1,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Filter::Scale { .. } => "Scale and offset",
            Filter::Db => "Decibels",
            Filter::NormDiff => "Normalized difference",
            Filter::Savi { .. } => "SAVI",
            Filter::Evi => "EVI",
            Filter::Threshold { .. } => "Threshold",
            Filter::Gaussian { .. } => "Gaussian",
            Filter::Boxcar { .. } => "Boxcar mean",
            Filter::Median { .. } => "Median",
            Filter::Erode { .. } => "Erode",
            Filter::Dilate { .. } => "Dilate",
            Filter::Open { .. } => "Open",
            Filter::Close { .. } => "Close",
            Filter::Sobel => "Sobel edges",
            Filter::Laplace => "Laplacian",
            Filter::Lee { .. } => "Lee speckle filter",
            Filter::Hillshade { .. } => "Hillshade",
            Filter::Slope { .. } => "Slope",
            Filter::Aspect => "Aspect",
        }
    }

    /// The parameters, by name, to change them.
    pub fn params(&mut self) -> Vec<(&'static str, &mut f64)> {
        match self {
            Filter::Scale { k, o } => vec![("scale", k), ("offset", o)],
            Filter::Savi { l } => vec![("L", l)],
            Filter::Threshold { lo, hi } => vec![("minimum", lo), ("maximum", hi)],
            Filter::Gaussian { sigma } => vec![("sigma", sigma)],
            Filter::Boxcar { w, h } => vec![("width", w), ("height", h)],
            Filter::Median { r } | Filter::Erode { r } | Filter::Dilate { r } | Filter::Open { r } | Filter::Close { r } => vec![("radius", r)],
            Filter::Lee { r, looks } => vec![("radius", r), ("looks", looks)],
            Filter::Hillshade { azimuth, altitude, z } => vec![("azimuth", azimuth), ("altitude", altitude), ("z factor", z)],
            Filter::Slope { z } => vec![("z factor", z)],
            Filter::Db | Filter::NormDiff | Filter::Evi | Filter::Sobel | Filter::Laplace | Filter::Aspect => vec![],
        }
    }

    /// The units of the result for inputs in `units`.
    pub fn units(&self, units: &str) -> String {
        match self {
            Filter::Db => "dB".into(),
            Filter::Slope { .. } | Filter::Aspect => "degrees".into(),
            Filter::Scale { .. } | Filter::Gaussian { .. } | Filter::Boxcar { .. } | Filter::Median { .. } => units.into(),
            Filter::Erode { .. } | Filter::Dilate { .. } | Filter::Open { .. } | Filter::Close { .. } | Filter::Lee { .. } => units.into(),
            _ => String::new(),
        }
    }

    /// A radius in pixels of level 0 in pixels of a level with `k` level-0 pixels for each pixel.
    fn at(r: f64, k: f64) -> usize {
        ((r / k).round().max(0.0) as usize).min(MAX_HALO / 2)
    }

    /// The pixels around a pixel that the filter uses, at a level with `k` level-0 pixels for each pixel.
    pub fn halo(&self, k: f64) -> usize {
        match self {
            Filter::Gaussian { sigma } => {
                let s = sigma / k;
                if s < 0.3 { 0 } else { ((3.0 * s).ceil() as usize).min(MAX_HALO) }
            }
            Filter::Boxcar { w, h } => Filter::at((w - 1.0) / 2.0, k).max(Filter::at((h - 1.0) / 2.0, k)),
            Filter::Median { r } | Filter::Erode { r } | Filter::Dilate { r } | Filter::Lee { r, .. } => Filter::at(*r, k),
            Filter::Open { r } | Filter::Close { r } => 2 * Filter::at(*r, k),
            Filter::Sobel | Filter::Laplace | Filter::Hillshade { .. } | Filter::Slope { .. } | Filter::Aspect => 1,
            _ => 0,
        }
    }

    /// The result for a window of `w` x `h` pixels at a level with `k` level-0 pixels for each pixel.
    /// `inputs[i]` has the values of input i on (w + 2 r) x (h + 2 r) pixels, r = `halo(k)`. `cell`: the
    /// size of a pixel of this level on the ground, in meters (x, y).
    pub fn run(&self, inputs: &[Vec<f32>], w: usize, h: usize, k: f64, cell: (f64, f64)) -> Vec<f32> {
        let r = self.halo(k);
        let (pw, ph) = (w + 2 * r, h + 2 * r);
        let p = &inputs[0][..];
        // The pixel ops: the same pixel of each input.
        let px = |f: &dyn Fn(&[f64]) -> f64| -> Vec<f32> {
            let mut v = vec![0f64; inputs.len()];
            (0..w * h)
                .map(|i| {
                    v.iter_mut().zip(inputs).for_each(|(x, a)| *x = a[i] as f64);
                    let y = f(&v);
                    if y.is_finite() { y as f32 } else { f32::NAN }
                })
                .collect()
        };
        match *self {
            Filter::Scale { k, o } => px(&|v| v[0] * k + o),
            Filter::Db => px(&|v| if v[0] > 0.0 { 10.0 * v[0].log10() } else { f64::NAN }),
            Filter::NormDiff => px(&|v| (v[0] - v[1]) / (v[0] + v[1])),
            Filter::Savi { l } => px(&|v| (1.0 + l) * (v[0] - v[1]) / (v[0] + v[1] + l)),
            Filter::Evi => px(&|v| 2.5 * (v[0] - v[1]) / (v[0] + 6.0 * v[1] - 7.5 * v[2] + 1.0)),
            Filter::Threshold { lo, hi } => px(&|v| if v[0].is_nan() { f64::NAN } else { (lo <= v[0] && v[0] <= hi) as u8 as f64 }),
            _ if r == 0 && !matches!(self, Filter::Sobel | Filter::Laplace | Filter::Hillshade { .. } | Filter::Slope { .. } | Filter::Aspect) => p.to_vec(),
            Filter::Gaussian { sigma } => {
                let s = sigma / k;
                let g: Vec<f64> = (0..2 * r + 1).map(|i| (-((i as f64 - r as f64).powi(2)) / (2.0 * s * s)).exp()).collect();
                conv(p, pw, ph, &g, &g)
            }
            Filter::Boxcar { w: bw, h: bh } => {
                let bx = |n: usize| -> Vec<f64> { (0..2 * r + 1).map(|i| (i.abs_diff(r) <= n) as u8 as f64).collect() };
                conv(p, pw, ph, &bx(Filter::at((bw - 1.0) / 2.0, k)), &bx(Filter::at((bh - 1.0) / 2.0, k)))
            }
            Filter::Median { .. } => reduce(p, pw, ph, r, &|v| {
                v.sort_unstable_by(f32::total_cmp);
                let n = v.len();
                if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
            }),
            Filter::Erode { .. } => reduce(p, pw, ph, r, &min),
            Filter::Dilate { .. } => reduce(p, pw, ph, r, &max),
            Filter::Open { .. } => reduce(&reduce(p, pw, ph, r / 2, &min), pw - r, ph - r, r / 2, &max),
            Filter::Close { .. } => reduce(&reduce(p, pw, ph, r / 2, &max), pw - r, ph - r, r / 2, &min),
            Filter::Lee { looks, .. } => {
                let cu2 = 1.0 / looks.max(1e-6);
                let mut out = vec![f32::NAN; w * h];
                for y in 0..h {
                    for x in 0..w {
                        let c = p[(y + r) * pw + x + r];
                        if c.is_nan() {
                            continue;
                        }
                        let (mut s, mut s2, mut m) = (0f64, 0f64, 0usize);
                        for j in 0..2 * r + 1 {
                            for &v in p[(y + j) * pw + x..(y + j) * pw + x + 2 * r + 1].iter().filter(|v| v.is_finite()) {
                                (s, s2, m) = (s + v as f64, s2 + (v as f64).powi(2), m + 1);
                            }
                        }
                        let mean = s / m as f64;
                        let ci2 = ((s2 / m as f64 - mean * mean).max(0.0)) / (mean * mean);
                        // A homogeneous area (variation below the speckle): the mean. An edge or a point: the pixel.
                        let wt = if ci2 > cu2 && ci2.is_finite() { 1.0 - cu2 / ci2 } else { 0.0 };
                        out[y * w + x] = (mean + wt * (c as f64 - mean)) as f32;
                    }
                }
                out
            }
            Filter::Sobel | Filter::Laplace | Filter::Hillshade { .. } | Filter::Slope { .. } | Filter::Aspect => {
                let mut out = vec![f32::NAN; w * h];
                let (dx, dy) = (cell.0.max(1e-9), cell.1.max(1e-9));
                for y in 0..h {
                    for x in 0..w {
                        let z = |i: usize, j: usize| p[(y + r - 1 + j) * pw + x + r - 1 + i] as f64;
                        let [a, b, c, d, e, f, g, hh, i] = [z(0, 0), z(1, 0), z(2, 0), z(0, 1), z(1, 1), z(2, 1), z(0, 2), z(1, 2), z(2, 2)];
                        // Horn: the change to the east and to the south for one pixel.
                        let (ge, gs) = (((c + 2.0 * f + i) - (a + 2.0 * d + g)) / 8.0, ((g + 2.0 * hh + i) - (a + 2.0 * b + c)) / 8.0);
                        let v = match *self {
                            Filter::Sobel => ge.hypot(gs) / k,
                            Filter::Laplace => (b + d + f + hh - 4.0 * e) / (k * k),
                            Filter::Hillshade { azimuth, altitude, z } => {
                                // The normal of the surface and the direction of the light (east, north, up).
                                let (ge, gn) = (z * ge / dx, -z * gs / dy);
                                let (az, al) = (azimuth.to_radians(), altitude.to_radians());
                                let nl = -ge * az.sin() * al.cos() - gn * az.cos() * al.cos() + al.sin();
                                (nl / (ge * ge + gn * gn + 1.0).sqrt()).max(0.0)
                            }
                            Filter::Slope { z } => (z * ge / dx).hypot(z * gs / dy).atan().to_degrees(),
                            _ => {
                                // The direction down the slope: (-ge, gs) to the east and to the north.
                                let (e, n) = (-ge / dx, gs / dy);
                                if e == 0.0 && n == 0.0 { f64::NAN } else { e.atan2(n).to_degrees().rem_euclid(360.0) }
                            }
                        };
                        out[y * w + x] = if v.is_finite() { v as f32 } else { f32::NAN };
                    }
                }
                out
            }
        }
    }
}

fn min(v: &mut Vec<f32>) -> f32 {
    v.iter().copied().fold(f32::INFINITY, f32::min)
}

fn max(v: &mut Vec<f32>) -> f32 {
    v.iter().copied().fold(f32::NEG_INFINITY, f32::max)
}

/// `f` of the values with data in the square of 2 r + 1 pixels around each pixel of `p` (pw x ph pixels),
/// for the (pw - 2 r) x (ph - 2 r) pixels that have the full square. NaN where the pixel has no data.
fn reduce(p: &[f32], pw: usize, ph: usize, r: usize, f: &dyn Fn(&mut Vec<f32>) -> f32) -> Vec<f32> {
    let (w, h) = (pw - 2 * r, ph - 2 * r);
    let mut out = vec![f32::NAN; w * h];
    let mut v = Vec::with_capacity((2 * r + 1).pow(2));
    for y in 0..h {
        for x in 0..w {
            if p[(y + r) * pw + x + r].is_nan() {
                continue;
            }
            v.clear();
            for j in 0..2 * r + 1 {
                v.extend(p[(y + j) * pw + x..(y + j) * pw + x + 2 * r + 1].iter().filter(|v| v.is_finite()));
            }
            out[y * w + x] = f(&mut v);
        }
    }
    out
}

/// The weighted mean with the weights `kx` along the rows and `ky` along the columns (2 r + 1 values each)
/// of the values with data around each pixel of `p`. NaN where the pixel has no data.
fn conv(p: &[f32], pw: usize, ph: usize, kx: &[f64], ky: &[f64]) -> Vec<f32> {
    let r = kx.len() / 2;
    let (w, h) = (pw - 2 * r, ph - 2 * r);
    // Along the rows: the sums of the weighted values and of the weights, for all the rows of `p`.
    let (mut num, mut den) = (vec![0f64; w * ph], vec![0f64; w * ph]);
    for y in 0..ph {
        for x in 0..w {
            let (mut s, mut c) = (0.0, 0.0);
            for (i, &k) in kx.iter().enumerate() {
                let v = p[y * pw + x + i];
                if v.is_finite() && k != 0.0 {
                    (s, c) = (s + k * v as f64, c + k);
                }
            }
            (num[y * w + x], den[y * w + x]) = (s, c);
        }
    }
    let mut out = vec![f32::NAN; w * h];
    for y in 0..h {
        for x in 0..w {
            if p[(y + r) * pw + x + r].is_nan() {
                continue;
            }
            let (mut s, mut c) = (0.0, 0.0);
            for (j, &k) in ky.iter().enumerate() {
                (s, c) = (s + k * num[(y + j) * w + x], c + k * den[(y + j) * w + x]);
            }
            out[y * w + x] = (s / c) as f32;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window of `w` x `h` pixels with a halo of `r` pixels: the values of `f(x, y)`, x and y from -r.
    fn pad(w: usize, h: usize, r: usize, f: impl Fn(f64, f64) -> f32) -> Vec<f32> {
        (0..(h + 2 * r) * (w + 2 * r)).map(|i| f((i % (w + 2 * r)) as f64 - r as f64, (i / (w + 2 * r)) as f64 - r as f64)).collect()
    }

    #[test]
    fn radiometry_and_indices() {
        let one = |f: Filter, v: &[f32]| f.run(&v.iter().map(|&x| vec![x]).collect::<Vec<_>>(), 1, 1, 1.0, (1.0, 1.0))[0];
        assert_eq!(one(Filter::Scale { k: 2.0, o: -1.0 }, &[3.0]), 5.0);
        assert_eq!(one(Filter::Db, &[100.0]), 20.0);
        assert!(one(Filter::Db, &[0.0]).is_nan());
        assert!((one(Filter::NormDiff, &[0.5, 0.1]) - 0.4 / 0.6).abs() < 1e-6);
        assert!((one(Filter::Savi { l: 0.5 }, &[0.5, 0.1]) - 1.5 * 0.4 / 1.1).abs() < 1e-6);
        assert!((one(Filter::Evi, &[0.5, 0.1, 0.05]) - 2.5 * 0.4 / (0.5 + 0.6 - 0.375 + 1.0)).abs() < 1e-6);
        let t = Filter::Threshold { lo: 2.0, hi: 10.0 };
        assert_eq!((one(t.clone(), &[1.0]), one(t.clone(), &[5.0])), (0.0, 1.0));
        assert!(one(t, &[f32::NAN]).is_nan());
    }

    #[test]
    fn space_filters() {
        // A constant stays the constant. A no-data pixel stays without data, and the others do not use it.
        for f in [Filter::Gaussian { sigma: 1.0 }, Filter::Boxcar { w: 3.0, h: 5.0 }, Filter::Median { r: 2.0 }, Filter::Open { r: 1.0 }, Filter::Lee { r: 1.0, looks: 1.0 }] {
            let r = f.halo(1.0);
            let p = pad(4, 4, r, |x, y| if (x, y) == (1.0, 0.0) { f32::NAN } else { 7.0 });
            let out = f.run(&[p], 4, 4, 1.0, (1.0, 1.0));
            assert!(out[1].is_nan() && out.iter().filter(|v| !v.is_nan()).all(|&v| (v - 7.0).abs() < 1e-5), "{f:?} {out:?}");
        }
        // One bright pixel: the median and the opening remove it, the dilation makes it a 3 x 3 square.
        let dot = |r| pad(5, 5, r, |x, y| if (x, y) == (2.0, 2.0) { 9.0 } else { 1.0 });
        assert!(Filter::Median { r: 1.0 }.run(&[dot(1)], 5, 5, 1.0, (1.0, 1.0)).iter().all(|&v| v == 1.0));
        assert!(Filter::Open { r: 1.0 }.run(&[dot(2)], 5, 5, 1.0, (1.0, 1.0)).iter().all(|&v| v == 1.0));
        let d = Filter::Dilate { r: 1.0 }.run(&[dot(1)], 5, 5, 1.0, (1.0, 1.0));
        assert_eq!(d.iter().filter(|&&v| v == 9.0).count(), 9);
        // A ramp of 3 for each pixel: Sobel 3, Laplacian 0. x squared: Laplacian 2.
        assert!(Filter::Sobel.run(&[pad(3, 3, 1, |x, _| 3.0 * x as f32)], 3, 3, 1.0, (1.0, 1.0)).iter().all(|&v| (v - 3.0).abs() < 1e-6));
        assert!(Filter::Laplace.run(&[pad(3, 3, 1, |x, _| (x * x) as f32)], 3, 3, 1.0, (1.0, 1.0)).iter().all(|&v| (v - 2.0).abs() < 1e-6));
        // At level 2 (4 level-0 pixels for each pixel), a radius of 1 is no filter.
        assert_eq!(Filter::Median { r: 1.0 }.halo(4.0), 0);
    }

    #[test]
    fn lee_filter() {
        // Values 0 with a center 100: mean 11.11, Ci2 = 8, W = 1 - 1/8, the result is 88.89.
        let p = pad(1, 1, 1, |x, y| if (x, y) == (0.0, 0.0) { 100.0 } else { 0.0 });
        let v = Filter::Lee { r: 1.0, looks: 1.0 }.run(&[p], 1, 1, 1.0, (1.0, 1.0))[0];
        assert!((v - 88.888_89).abs() < 1e-3, "{v}");
        // A variation below the speckle: the mean (5).
        let p: Vec<f32> = (1..=9).map(|v| v as f32).collect();
        assert!((Filter::Lee { r: 1.0, looks: 1.0 }.run(&[p], 1, 1, 1.0, (1.0, 1.0))[0] - 5.0).abs() < 1e-6);
    }

    #[test]
    fn terrain() {
        // A plane that goes up 10 m to the east for each pixel of 10 m: slope 45, faces west, the light
        // from the west at 45 degrees is on its normal.
        let p = pad(3, 3, 1, |x, _| 10.0 * x as f32);
        let run = |f: Filter| f.run(&[p.clone()], 3, 3, 1.0, (10.0, 10.0))[4];
        assert!((run(Filter::Slope { z: 1.0 }) - 45.0).abs() < 1e-4);
        assert!((run(Filter::Aspect) - 270.0).abs() < 1e-4);
        assert!((run(Filter::Hillshade { azimuth: 270.0, altitude: 45.0, z: 1.0 }) - 1.0).abs() < 1e-5);
        assert!(run(Filter::Hillshade { azimuth: 90.0, altitude: 0.0, z: 1.0 }).abs() < 1e-6);
        // Up to the south: faces north.
        assert!((Filter::Aspect.run(&[pad(3, 3, 1, |_, y| y as f32)], 3, 3, 1.0, (1.0, 1.0))[4] - 0.0).abs() < 1e-4);
    }

    #[test]
    fn file_format() {
        let f = Filter::Gaussian { sigma: 2.0 };
        let s = serde_json::to_string(&f).unwrap();
        assert_eq!(s, r#"{"op":"gaussian","sigma":2.0}"#);
        assert_eq!(serde_json::from_str::<Filter>(&s).unwrap(), f);
        assert_eq!(serde_json::from_str::<Filter>(r#"{"op":"norm_diff"}"#).unwrap(), Filter::NormDiff);
    }
}
