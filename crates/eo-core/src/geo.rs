//! CRS transforms (proj4rs, pure Rust) and the warp grid: level-0 pixel position to display coordinates.
use crate::{Error, Georef, Result};

/// Projection of an EPSG code. Geographic CRSs use degrees (x = longitude, y = latitude).
pub struct Proj {
    p: proj4rs::Proj,
    latlong: bool,
}

impl Proj {
    pub fn epsg(code: u32) -> Result<Proj> {
        let c = u16::try_from(code).map_err(|_| Error(format!("EPSG:{code} is not supported")))?;
        let p = proj4rs::Proj::from_epsg_code(c).map_err(|e| Error(format!("EPSG:{code}: {e}")))?;
        Ok(Proj { latlong: p.is_latlong(), p })
    }

    /// Transform a point to `dst`. None if the point is outside the domain of a projection.
    pub fn to(&self, dst: &Proj, x: f64, y: f64) -> Option<(f64, f64)> {
        let mut pt = if self.latlong { (x.to_radians(), y.to_radians()) } else { (x, y) };
        proj4rs::transform::transform(&self.p, &dst.p, &mut pt).ok()?;
        let pt = if dst.latlong { (pt.0.to_degrees(), pt.1.to_degrees()) } else { pt };
        (pt.0.is_finite() && pt.1.is_finite()).then_some(pt)
    }
}

/// Longitude and latitude (degrees, WGS 84) of an ECEF point (meters). Bowring's method: error below 1 mm
/// near the surface of the Earth. The height is not used.
pub fn ecef_to_lonlat(p: [f64; 3]) -> (f64, f64) {
    let (a, f) = (6378137.0, 1.0 / 298.257223563);
    let b = a * (1.0 - f);
    let e2 = f * (2.0 - f);
    let ep2 = e2 / (1.0 - e2);
    let r = p[0].hypot(p[1]);
    let t = (p[2] * a).atan2(r * b);
    let lat = (p[2] + ep2 * b * t.sin().powi(3)).atan2(r - e2 * a * t.cos().powi(3));
    (p[1].atan2(p[0]).to_degrees(), lat.to_degrees())
}

/// Display coordinates at the nodes of a regular grid over the level-0 pixels of a layer.
/// Display coordinates have y up: in a map CRS, y is the northing. In pixel space, y = -row.
/// Values are relative to `origin`, so that they stay precise in f32.
#[derive(Clone, Debug)]
pub struct Warp {
    pub w: f64,
    pub h: f64,
    pub nx: usize,
    pub ny: usize,
    /// Row order (j * nx + i). NaN where the transform fails.
    pub pts: Vec<[f64; 2]>,
    pub origin: [f64; 2],
    /// Cells in each bucket of a regular grid over the display box, for `inverse`. Made at the first use.
    index: std::sync::OnceLock<Index>,
}

#[derive(Clone, Debug)]
struct Index {
    bbox: [f64; 4],
    n: usize,
    cells: Vec<Vec<u32>>,
}

impl Warp {
    /// Warp from pixel position to the display CRS `dst` (EPSG code; None: pixel space).
    /// `georef` must not be `Georef::Arrays` (the engine reads the arrays and makes a `Georef::Grid`).
    pub fn new(georef: &Georef, w: f64, h: f64, dst: Option<u32>) -> Result<Warp> {
        let Some(dst) = dst else { return Ok(Warp::build(w, h, |c, r| [c, -r])) };
        let src = match georef.crs() {
            Some(c) => c.epsg.ok_or_else(|| Error(format!("CRS {} has no EPSG code", c.name)))?,
            None => return Err("layer without georeferencing".into()),
        };
        if matches!(georef, Georef::Arrays { .. }) {
            return Err("geolocation arrays are not read".into());
        }
        let map = |c, r| georef.map(c, r).unwrap_or((f64::NAN, f64::NAN));
        if src == dst {
            return Ok(Warp::build(w, h, |c, r| map(c, r).into()));
        }
        let (a, b) = (Proj::epsg(src)?, Proj::epsg(dst)?);
        Ok(Warp::build(w, h, |c, r| {
            let (x, y) = map(c, r);
            a.to(&b, x, y).map_or([f64::NAN; 2], Into::into)
        }))
    }

    /// Grid of 2^k + 1 nodes on each side. It gets finer until the bilinear interpolation of the grid
    /// agrees with `f` at the cell centers to 0.05 pixel (or the grid has 257 x 257 nodes).
    pub fn build(w: f64, h: f64, f: impl Fn(f64, f64) -> [f64; 2]) -> Warp {
        let mut n = 2;
        loop {
            let pts: Vec<[f64; 2]> =
                (0..n).flat_map(|j| (0..n).map(move |i| (i, j))).map(|(i, j)| f(w * i as f64 / (n - 1) as f64, h * j as f64 / (n - 1) as f64)).collect();
            let mut wp = Warp { w, h, nx: n, ny: n, pts, origin: [0.0; 2], index: Default::default() };
            let px = wp.px_size().max(1e-12);
            let mut err = 0f64;
            for j in 0..n - 1 {
                for i in 0..n - 1 {
                    let (c, r) = (w * (i as f64 + 0.5) / (n - 1) as f64, h * (j as f64 + 0.5) / (n - 1) as f64);
                    let (e, g) = (f(c, r), wp.at(c, r));
                    if e[0].is_finite() && g[0].is_finite() {
                        err = err.max((e[0] - g[0]).hypot(e[1] - g[1]) / px);
                    }
                }
            }
            if err < 0.05 || n >= 257 {
                let c = wp.pts[(n / 2) * n + n / 2];
                wp.origin = if c[0].is_finite() { c } else { wp.pts.iter().copied().find(|p| p[0].is_finite()).unwrap_or([0.0; 2]) };
                let o = wp.origin;
                wp.pts.iter_mut().for_each(|p| *p = [p[0] - o[0], p[1] - o[1]]);
                return wp;
            }
            n = 2 * n - 1;
        }
    }

    fn node(&self, i: usize, j: usize) -> [f64; 2] {
        self.pts[j.min(self.ny - 1) * self.nx + i.min(self.nx - 1)]
    }

    /// Display coordinates (absolute) of pixel position (col, row). Bilinear on the grid.
    pub fn at(&self, col: f64, row: f64) -> [f64; 2] {
        let gx = (col / self.w * (self.nx - 1) as f64).clamp(0.0, (self.nx - 1) as f64);
        let gy = (row / self.h * (self.ny - 1) as f64).clamp(0.0, (self.ny - 1) as f64);
        let (i, j) = ((gx as usize).min(self.nx - 2), (gy as usize).min(self.ny.max(2) - 2));
        let (tx, ty) = (gx - i as f64, gy - j as f64);
        let (a, b, c, d) = (self.node(i, j), self.node(i + 1, j), self.node(i, j + 1), self.node(i + 1, j + 1));
        let l = |k: usize| (a[k] * (1.0 - tx) + b[k] * tx) * (1.0 - ty) + (c[k] * (1.0 - tx) + d[k] * tx) * ty;
        [l(0) + self.origin[0], l(1) + self.origin[1]]
    }

    /// Display units for one level-0 pixel, near the center of the image.
    pub fn px_size(&self) -> f64 {
        let (c, r) = (self.w / 2.0, self.h / 2.0);
        let (a, b, d) = (self.at(c, r), self.at(c + 1.0, r), self.at(c, r + 1.0));
        ((b[0] - a[0]).hypot(b[1] - a[1]) * (d[0] - a[0]).hypot(d[1] - a[1])).sqrt()
    }

    /// Cells of the grid (i, j) with their display bounding box (absolute).
    fn cells(&self) -> impl Iterator<Item = (usize, usize, [f64; 4])> + '_ {
        (0..self.ny - 1).flat_map(move |j| {
            (0..self.nx - 1).filter_map(move |i| {
                let q = [self.node(i, j), self.node(i + 1, j), self.node(i, j + 1), self.node(i + 1, j + 1)];
                if q.iter().any(|p| !p[0].is_finite()) {
                    return None;
                }
                let (o, f) = (self.origin, |k: usize, m: fn(f64, f64) -> f64| q.iter().map(|p| p[k]).fold(q[0][k], m));
                Some((i, j, [f(0, f64::min) + o[0], f(1, f64::min) + o[1], f(0, f64::max) + o[0], f(1, f64::max) + o[1]]))
            })
        })
    }

    fn cell_box(&self, i: usize, j: usize) -> Option<[f64; 4]> {
        let q = [self.node(i, j), self.node(i + 1, j), self.node(i, j + 1), self.node(i + 1, j + 1)];
        if q.iter().any(|p| !p[0].is_finite()) {
            return None;
        }
        let (o, f) = (self.origin, |k: usize, m: fn(f64, f64) -> f64| q.iter().map(|p| p[k]).fold(q[0][k], m));
        Some([f(0, f64::min) + o[0], f(1, f64::min) + o[1], f(0, f64::max) + o[0], f(1, f64::max) + o[1]])
    }

    fn index(&self) -> &Index {
        self.index.get_or_init(|| {
            let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for (_, _, c) in self.cells() {
                b = [b[0].min(c[0]), b[1].min(c[1]), b[2].max(c[2]), b[3].max(c[3])];
            }
            let n = 64;
            let mut cells = vec![vec![]; n * n];
            let (sx, sy) = ((b[2] - b[0]).max(1e-300) / n as f64, (b[3] - b[1]).max(1e-300) / n as f64);
            let k = |v: f64, o: f64, s: f64| (((v - o) / s) as usize).min(n - 1);
            for (i, j, c) in self.cells() {
                let id = (j * (self.nx - 1) + i) as u32;
                for by in k(c[1], b[1], sy)..=k(c[3], b[1], sy) {
                    for bx in k(c[0], b[0], sx)..=k(c[2], b[0], sx) {
                        cells[by * n + bx].push(id);
                    }
                }
            }
            Index { bbox: b, n, cells }
        })
    }

    /// Pixel position of display point (x, y), or None if it is outside the image.
    pub fn inverse(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (cw, ch) = (self.w / (self.nx - 1) as f64, self.h / (self.ny - 1) as f64);
        if self.nx == 2 && self.ny == 2 {
            // Affine: exact inverse from the corners.
            let (o, a, b) = (self.at(0.0, 0.0), self.at(self.w, 0.0), self.at(0.0, self.h));
            let (ax, ay, bx, by) = ((a[0] - o[0]) / self.w, (a[1] - o[1]) / self.w, (b[0] - o[0]) / self.h, (b[1] - o[1]) / self.h);
            let det = ax * by - bx * ay;
            if det == 0.0 || !det.is_finite() {
                return None;
            }
            let (ex, ey) = (x - o[0], y - o[1]);
            let (c, r) = ((by * ex - bx * ey) / det, (ax * ey - ay * ex) / det);
            return ((0.0..=self.w).contains(&c) && (0.0..=self.h).contains(&r)).then_some((c, r));
        }
        let ix = self.index();
        let b = ix.bbox;
        if x < b[0] || x > b[2] || y < b[1] || y > b[3] {
            return None;
        }
        let k = |v: f64, o: f64, s: f64| (((v - o) / s) as usize).min(ix.n - 1);
        let (bx, by) = (k(x, b[0], (b[2] - b[0]).max(1e-300) / ix.n as f64), k(y, b[1], (b[3] - b[1]).max(1e-300) / ix.n as f64));
        for &id in &ix.cells[by * ix.n + bx] {
            let (i, j) = (id as usize % (self.nx - 1), id as usize / (self.nx - 1));
            let Some(c) = self.cell_box(i, j) else { continue };
            if x < c[0] || x > c[2] || y < c[1] || y > c[3] {
                continue;
            }
            // Newton iterations on the bilinear map of the cell.
            let (mut u, mut v) = (0.5, 0.5);
            for _ in 0..8 {
                let (col, row) = ((i as f64 + u) * cw, (j as f64 + v) * ch);
                let p = self.at(col, row);
                let (px, py) = (self.at(col + cw * 1e-3, row), self.at(col, row + ch * 1e-3));
                let (a, b) = ((px[0] - p[0]) / 1e-3, (py[0] - p[0]) / 1e-3);
                let (cc, d) = ((px[1] - p[1]) / 1e-3, (py[1] - p[1]) / 1e-3);
                let det = a * d - b * cc;
                if det.abs() < 1e-300 {
                    break;
                }
                let (ex, ey) = (x - p[0], y - p[1]);
                u += (d * ex - b * ey) / det;
                v += (a * ey - cc * ex) / det;
            }
            if (-1e-6..=1.0 + 1e-6).contains(&u) && (-1e-6..=1.0 + 1e-6).contains(&v) {
                return Some(((i as f64 + u) * cw, (j as f64 + v) * ch));
            }
        }
        None
    }

    /// Pixel rectangle (col0, row0, col1, row1) that contains the pixels visible in the display rectangle
    /// (x0, y0, x1, y1): the pixel positions of points on the edges of the rectangle, and the points on the
    /// image edges that are in the rectangle, with a small margin. None if no pixel is visible.
    pub fn pixel_bbox(&self, r: [f64; 4]) -> Option<[f64; 4]> {
        const N: usize = 16;
        let mut b: Option<[f64; 4]> = None;
        let mut add = |c: f64, rw: f64| {
            b = Some(b.map_or([c, rw, c, rw], |b| [b[0].min(c), b[1].min(rw), b[2].max(c), b[3].max(rw)]));
        };
        let t = |k: usize| k as f64 / N as f64;
        for k in 0..=N {
            let (x, y) = (r[0] + (r[2] - r[0]) * t(k), r[1] + (r[3] - r[1]) * t(k));
            for (px, py) in [(x, r[1]), (x, r[3]), (r[0], y), (r[2], y), ((r[0] + r[2]) / 2.0, y)] {
                if let Some((c, rw)) = self.inverse(px, py) {
                    add(c, rw);
                }
            }
            let (c, rw) = (self.w * t(k), self.h * t(k));
            for (pc, pr) in [(c, 0.0), (c, self.h), (0.0, rw), (self.w, rw)] {
                let p = self.at(pc, pr);
                if p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3] {
                    add(pc, pr);
                }
            }
        }
        // Margin: the edges between the points can bend outwards.
        b.map(|b| {
            let (mx, my) = ((b[2] - b[0]) * 0.02 + 1.0, (b[3] - b[1]) * 0.02 + 1.0);
            [(b[0] - mx).max(0.0), (b[1] - my).max(0.0), (b[2] + mx).min(self.w), (b[3] + my).min(self.h)]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Crs;

    #[test]
    fn utm_to_geographic() {
        // Sentinel-2 tile 32TLP: upper left corner. Expected values from gdaltransform (PROJ).
        let a = Proj::epsg(32632).unwrap();
        let g = Proj::epsg(4326).unwrap();
        let (lon, lat) = a.to(&g, 300000.0, 4900020.0).unwrap();
        assert!((lon - 6.49592859291465).abs() < 1e-9 && (lat - 44.2259641543025).abs() < 1e-9, "{lon} {lat}");
    }

    #[test]
    fn ecef_point() {
        // ECEF of lon 4.35, lat 51.25, height 0 (computed with the closed formula).
        let (lon, lat) = (4.35f64.to_radians(), 51.25f64.to_radians());
        let (a, e2) = (6378137.0, 6.69437999014e-3);
        let n = a / (1.0 - e2 * lat.sin().powi(2)).sqrt();
        let p = [n * lat.cos() * lon.cos(), n * lat.cos() * lon.sin(), n * (1.0 - e2) * lat.sin()];
        let (lo, la) = ecef_to_lonlat(p);
        assert!((lo - 4.35).abs() < 1e-10 && (la - 51.25).abs() < 1e-9, "{lo} {la}");
    }

    #[test]
    fn warp_refines_and_inverts() {
        let geo = Georef::Affine { gt: [300000.0, 10.0, 0.0, 4900020.0, 0.0, -10.0], crs: Crs { epsg: Some(32632), name: String::new() } };
        let same = Warp::new(&geo, 10980.0, 10980.0, Some(32632)).unwrap();
        assert_eq!(same.nx, 2);
        let w = Warp::new(&geo, 10980.0, 10980.0, Some(4326)).unwrap();
        assert!(w.nx > 2);
        let p = w.at(1234.5, 6789.25);
        let (c, r) = w.inverse(p[0], p[1]).unwrap();
        assert!((c - 1234.5).abs() < 1e-3 && (r - 6789.25).abs() < 1e-3, "{c} {r}");
        let b = w.pixel_bbox([p[0] - 1e-4, p[1] - 1e-4, p[0] + 1e-4, p[1] + 1e-4]).unwrap();
        assert!(b[0] <= 1234.5 && b[2] >= 1234.5 && b[1] <= 6789.25 && b[3] >= 6789.25);
    }
}
