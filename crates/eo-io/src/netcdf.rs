//! NetCDF-4 files (HDF5 inside): each dataset with 2 or more dimensions is a variable. The CF attributes
//! give the scale, the offset, the fill value and the units. The last two dimensions are the rows and
//! the columns. A dimension before them with the length of the `time` coordinate is the time dimension.
//!
//! Specifications: NetCDF-4 format, <https://docs.unidata.ucar.edu/netcdf-c/current/file_format_specifications.html>;
//! CF conventions, <https://cfconventions.org/>.
use crate::hdf5::{self, Dataset as H5};
use crate::{Source, codec};
use eo_core::*;

/// Variables of one file. `idx` is the index of `src` in the dataset sources. The 1D datasets come back too
/// (coordinates).
pub fn variables(src: &Source, idx: u32) -> Result<(Vec<Variable>, Vec<H5>)> {
    let ds = hdf5::datasets(src, 1)?;
    let mut vars = vec![];
    let mut one_d = vec![];
    for d in ds {
        if d.shape.len() < 2 {
            one_d.push(d);
            continue;
        }
        let a = match d.array(idx) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{}: {}: {e}", src.name(), d.path);
                continue;
            }
        };
        let name = d.path.clone();
        vars.push(Variable {
            bands: vec![name.rsplit('/').next().unwrap_or(&name).to_string()],
            name,
            group: String::new(),
            levels: vec![a],
            fill: d.num("_FillValue"),
            scale: d.num("scale_factor").unwrap_or(1.0),
            offset: d.num("add_offset").unwrap_or(0.0),
            units: d.text("units").unwrap_or("").to_string(),
            georef: Georef::None,
            times: vec![],
        });
    }
    // Time dimension: the reader does not read the dimension lists of HDF5, it compares the lengths.
    // The coordinate in the group of the variable is used first, then the coordinate nearest to the root.
    let leaf = |p: &str| p.rsplit('/').next().unwrap_or(p).to_lowercase();
    let dir = |p: &str| p.rsplit_once('/').map_or(String::new(), |d| d.0.to_string());
    let mut coords: Vec<&H5> = one_d.iter().filter(|d| leaf(&d.path) == "time").collect();
    coords.sort_by_key(|d| d.path.len());
    for v in &mut vars {
        let n = v.levels[0].shape.len();
        let t = coords.iter().filter(|c| dir(&v.name).starts_with(&dir(&c.path))).rev().chain(&coords);
        let Some((c, k)) = t.filter_map(|c| Some((*c, v.levels[0].shape[..n - 2].iter().position(|&s| s == c.shape[0])?))).next() else { continue };
        v.levels[0].dims[k] = "time".into();
        if let (Some((unit, t0)), Ok(vals)) = (c.text("units").and_then(time::cf), read_1d(src, c)) {
            v.times = vals.into_iter().map(|x| t0 + x * unit).collect();
        }
    }
    Ok((vars, one_d))
}

/// Values of a 1D dataset (small: coordinates), with scale and offset.
pub fn read_1d(src: &Source, d: &H5) -> Result<Vec<f64>> {
    let a = d.array(0)?;
    let n = d.shape[0] as usize;
    let mut out = Vec::with_capacity(n);
    for c in &a.chunks {
        let b = if c.len == 0 { bytes::Bytes::new() } else { src.read(c.off..c.off + c.len)? };
        out.extend(codec::to_f64(&a, &codec::decode(&a, &b)?));
    }
    out.truncate(n);
    let (s, o) = (d.num("scale_factor").unwrap_or(1.0), d.num("add_offset").unwrap_or(0.0));
    Ok(out.into_iter().map(|v| v * s + o).collect())
}

/// Georeferencing from CF coordinates: 1D latitude and longitude (a regular grid), or 2D latitude and
/// longitude variables of the same shape as `v` (geolocation arrays).
pub fn georef(src: &Source, v: &Variable, one_d: &[H5], vars: &[Variable]) -> Georef {
    let is = |n: &str, k: &str| {
        let n = n.rsplit('/').next().unwrap_or(n).to_lowercase();
        n == k || n == &k[..3] || n.starts_with(&format!("{k}_"))
    };
    let (w, h) = v.size();
    let lat = one_d.iter().find(|d| is(&d.path, "latitude") && d.shape[0] == h);
    let lon = one_d.iter().find(|d| is(&d.path, "longitude") && d.shape[0] == w);
    if let (Some(la), Some(lo)) = (lat, lon)
        && let (Ok(la), Ok(lo)) = (read_1d(src, la), read_1d(src, lo))
        && la.len() > 1
        && lo.len() > 1
    {
        let (dx, dy) = (lo[1] - lo[0], la[1] - la[0]);
        return Georef::Affine { gt: [lo[0] - dx / 2.0, dx, 0.0, la[0] - dy / 2.0, 0.0, dy], crs: Crs::wgs84() };
    }
    // 2D arrays: same shape, and the same name suffix if there is one (Sentinel-3 SLSTR: _an, _in, ...).
    let suffix = v.name.rsplit('_').next().filter(|s| s.len() <= 3).unwrap_or("");
    let find = |k: &str| {
        let c: Vec<&Variable> = vars.iter().filter(|x| is(&x.name, k) && x.size() == (w, h)).collect();
        c.iter().find(|x| x.name.ends_with(&format!("_{suffix}"))).or(c.first()).map(|x| x.name.clone())
    };
    match (find("longitude"), find("latitude")) {
        (Some(lon), Some(lat)) if lon != v.name && lat != v.name => Georef::Arrays { lon, lat, step: [1.0, 1.0], off: [0.5, 0.5] },
        _ => Georef::None,
    }
}
