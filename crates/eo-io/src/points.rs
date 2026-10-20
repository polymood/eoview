//! Products of points: 1D variables with a latitude and a longitude of the same length (SMOS grid points,
//! altimetry along a track). The viewer shows images: each 1D variable also becomes an image on a grid of
//! longitude and latitude, the mean of the points in each cell. The levels have cells of 2, 4, 8, ... times the
//! size of level 0, so that a track stays visible in a view of the whole globe. The engine asks for the chunks
//! of these images as for other keyed sources: they are made from the points in memory, when they are needed.
use crate::{Dataset, Source};
use bytes::Bytes;
use eo_core::*;
use std::sync::Arc;
use tokio::runtime::Handle;

/// Chunk size (pixels) of the images.
const CH: u64 = 256;
/// The largest number of cells of level 0.
const MAX_CELLS: f64 = 64e6;

fn is_lat(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    n == "lat" || n.ends_with("latitude") || n.ends_with("_lat")
}

fn is_lon(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    n == "lon" || n.ends_with("longitude") || n.ends_with("_lon")
}

/// One level: its size, and the points of each chunk.
struct Level {
    w: u64,
    h: u64,
    chunks: Vec<Vec<u32>>,
}

/// The cell size (degrees) for the points: the median distance between two successive points, at least 0.01
/// degree, and more if level 0 has more than `MAX_CELLS` cells.
fn cell(lat: &[f64], lon: &[f64], ok: &[usize]) -> f64 {
    let mut d: Vec<f64> = ok.windows(2).map(|p| ((lat[p[1]] - lat[p[0]]).powi(2) + ((lon[p[1]] - lon[p[0]]) * lat[p[0]].to_radians().cos()).powi(2)).sqrt()).filter(|d| *d > 0.0 && *d < 5.0).collect();
    d.sort_by(f64::total_cmp);
    d.get(d.len() / 2).copied().unwrap_or(0.1).clamp(0.01, 1.0)
}

/// Add an image variable for each 1D variable of a product of points (see the module). Nothing if the product
/// has no 1D latitude and longitude.
pub fn add_images(ds: &mut Dataset, rt: &Handle) -> Result<()> {
    let len1 = |v: &Variable| (v.levels[0].shape.len() == 1 && !v.levels[0].dtype.is_complex()).then(|| v.levels[0].shape[0]);
    let small = &ds.product.small;
    let Some(la) = small.iter().find(|v| is_lat(&v.name) && len1(v).is_some()) else { return Ok(()) };
    let n = len1(la).unwrap_or(0);
    let Some(lo) = small.iter().find(|v| is_lon(&v.name) && len1(v) == Some(n)) else { return Ok(()) };
    if n == 0 || n > u32::MAX as u64 {
        return Ok(());
    }
    let (lat, mut lon) = (crate::read_all(ds, la)?, crate::read_all(ds, lo)?);
    lon.iter_mut().for_each(|x| *x = (*x + 180.0).rem_euclid(360.0) - 180.0);
    let ok: Vec<usize> = (0..n as usize).filter(|&i| lat[i].abs() <= 90.0 && lon[i].is_finite()).collect();
    if ok.is_empty() {
        return Ok(());
    }
    let b = ok.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, &i| [b[0].min(lon[i]), b[1].min(lat[i]), b[2].max(lon[i]), b[3].max(lat[i])]);
    let mut c = cell(&lat, &lon, &ok);
    while ((b[2] - b[0]) / c + 1.0) * ((b[3] - b[1]) / c + 1.0) > MAX_CELLS {
        c *= 2.0;
    }
    let (w0, h0) = (((b[2] - b[0]) / c).round() as u64 + 1, ((b[3] - b[1]) / c).round() as u64 + 1);
    // Column and row of each point at level 0. The center of pixel (0, 0) is at (west, north).
    let pos: Vec<(u64, u64)> = ok.iter().map(|&i| (((lon[i] - b[0]) / c).round() as u64, ((b[3] - lat[i]) / c).round() as u64)).collect();
    let mut levels = vec![];
    for k in 0.. {
        let (w, h) = (w0.div_ceil(1 << k), h0.div_ceil(1 << k));
        let gx = w.div_ceil(CH);
        let mut chunks = vec![vec![]; (gx * h.div_ceil(CH)) as usize];
        for (j, &(x, y)) in pos.iter().enumerate() {
            chunks[((y >> k) / CH * gx + (x >> k) / CH) as usize].push(j as u32);
        }
        levels.push(Arc::new(Level { w, h, chunks }));
        if w.max(h) <= CH {
            break;
        }
    }
    let (pos, ok) = (Arc::new(pos), Arc::new(ok));
    let gt = [b[0] - c / 2.0, c, 0.0, b[3] + c / 2.0, 0.0, -c];
    let ones: Vec<Variable> = small.iter().filter(|v| len1(v) == Some(n) && !is_lat(&v.name) && !is_lon(&v.name)).cloned().collect();
    for v in ones {
        let vals = Arc::new(crate::read_all(ds, &v)?);
        let mut arrays = vec![];
        for (k, l) in levels.iter().enumerate() {
            let (lv, vals, pos, ok) = (l.clone(), vals.clone(), pos.clone(), ok.clone());
            let src = Source::fetched(&format!("{}#points/{}/{k}", ds.product.name, v.name), rt, move |i| {
                let (l, vals, pos, ok) = (lv.clone(), vals.clone(), pos.clone(), ok.clone());
                Box::pin(async move {
                    let gx = l.w.div_ceil(CH);
                    let (x0, y0) = (i % gx * CH, i / gx * CH);
                    let (mut sum, mut cnt) = (vec![0.0f64; (CH * CH) as usize], vec![0u32; (CH * CH) as usize]);
                    for &j in l.chunks.get(i as usize).into_iter().flatten() {
                        let (x, y) = pos[j as usize];
                        let at = (((y >> k) - y0) * CH + (x >> k) - x0) as usize;
                        let val = vals[ok[j as usize]];
                        if val.is_finite() {
                            (sum[at], cnt[at]) = (sum[at] + val, cnt[at] + 1);
                        }
                    }
                    let out: Vec<u8> = sum.iter().zip(&cnt).flat_map(|(s, &m)| (if m > 0 { (s / m as f64) as f32 } else { f32::NAN }).to_le_bytes()).collect();
                    Ok(Some(Bytes::from(out)))
                })
            });
            ds.sources.push(Arc::new(src));
            let chunks = Chunks::Keyed { src: ds.sources.len() as u32 - 1, n: l.chunks.len() };
            arrays.push(Array { dims: vec!["y".into(), "x".into()], shape: vec![l.h, l.w], chunk: vec![CH, CH], dtype: DType::F32, le: true, codecs: vec![], chunks, place: None });
        }
        let name = v.name.clone();
        ds.product.vars.push(Variable { name: name.clone(), group: v.group.clone(), levels: arrays, bands: vec![name], fill: None, scale: 1.0, offset: 0.0, units: v.units.clone(), georef: Georef::Affine { gt, crs: Crs::wgs84() }, times: Default::default(), dims: vec![] });
    }
    ds.product.info.attrs.push(("note".into(), format!("{} points on a grid of {c:.4} degrees: a pixel is the mean of the points in it", ok.len())));
    Ok(())
}

#[cfg(test)]
mod tests {
    /// testdata/track.nc: the pixel of the point (latitude 30, longitude 15) of track 1 has ssha = 0.3.
    #[test]
    fn track_as_image() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ds = crate::open(concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/track.nc"), rt.handle()).unwrap();
        let v = &ds.product.vars[0];
        let eo_core::Georef::Affine { gt, .. } = v.georef else { panic!() };
        let (w, (x, y)) = (v.levels[0].shape[1], ((15.0 - gt[0]) / gt[1], (gt[3] - 30.0) / -gt[5]));
        let vals = crate::read_all(&ds, v).unwrap();
        assert!((vals[y as usize * w as usize + x as usize] - 0.3).abs() < 0.002);
        assert!(vals.iter().filter(|x| x.is_finite()).count() > 1000);
    }
}
