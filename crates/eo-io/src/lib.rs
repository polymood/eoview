//! Format readers, byte sources and codecs. A reader only describes the chunks of a product.
//! The chunk engine in `eo-cache` reads and decodes them.
mod blosc;
pub mod ceos;
pub mod dap;
pub mod envisat;
pub mod hdf4;
pub mod hdf5;
pub mod netcdf;
pub mod points;
pub mod netcdf3;
pub mod nitf;
pub mod codec;
pub mod grib;
mod jp2;
pub mod safe;
pub mod xml;
pub mod zarr;
pub mod source;
pub mod tiff;

pub use source::Source;

use eo_core::{Georef, Product, Result, Variable};
use std::sync::Arc;
use tokio::runtime::Handle;

/// A product and its byte sources. `ChunkLoc::src` is an index into `sources`.
pub struct Dataset {
    pub product: Product,
    pub sources: Vec<Arc<Source>>,
}

/// Open a local path or an HTTP(S) URL. This function blocks: do not call it on the UI thread.
/// A directory can be a SAFE product. A metadata file of a SAFE product opens its directory.
pub fn open(url: &str, rt: &Handle) -> Result<Dataset> {
    // OPeNDAP: a marked URL, or a server path (`/dodsC/`, `/opendap/`, `/dap/`) that answers `.dds`.
    if let Some((u, marked)) = dap::url(url) {
        match dap::open(&u, rt) {
            Ok(d) => return Ok(d),
            Err(e) if marked => return Err(e),
            Err(e) => eprintln!("{}: not OPeNDAP: {e}", eo_core::redact(url)),
        }
    }
    let path = std::path::Path::new(url);
    let remote_zarr = source::is_remote(url) && (url.trim_end_matches('/').ends_with(".zarr") || url.contains(".zarr/"));
    if remote_zarr || (path.is_dir() && zarr::is_zarr(url, rt)) {
        return zarr::open(url.trim_end_matches('/'), rt);
    }
    if !source::is_remote(url) {
        if path.is_dir() {
            // A directory in a SAFE product (for example GRANULE) opens the product.
            let dir = path.ancestors().find(|d| safe::kind(d).is_some()).unwrap_or(path);
            return safe::open(dir, rt);
        }
        if let Some(dir) = path.parent().filter(|d| safe::kind(d).is_some()) {
            return safe::open(dir, rt);
        }
    }
    let src = match Source::open(url, rt) {
        Ok(s) => s,
        // A remote Zarr store without ".zarr" in its name (for example `zarr-v1`): the URL is not an object.
        Err(_) if source::is_remote(url) && zarr::is_zarr(url.trim_end_matches('/'), rt) => return zarr::open(url.trim_end_matches('/'), rt),
        Err(e) => return Err(e),
    };
    // Sparse reads until the engine has the value sample.
    src.sparse(true);
    let head = src.read(0..16.min(src.len()?))?;
    let name = url.rsplit(['/', '\\']).next().unwrap_or("").to_string();
    // A CEOS product has several files: the reader opens the others.
    if ceos::is_ceos(&head) {
        return ceos::open(url, src, rt, name);
    }
    let product = if tiff::is_tiff(&head) {
        tiff::open(&src, 0)?
    } else if jp2::is_jp2(&head) {
        let levels = jp2::arrays(&src, 0)?;
        let rgb = levels[0].len_of("band") >= 3 && jp2::is_rgb(&src);
        let bands = (1..=levels[0].len_of("band") as usize).map(|b| if rgb && b <= 3 { ["red", "green", "blue"][b - 1].into() } else { format!("Band {b}") }).collect();
        let (w, h) = (levels[0].len_of("x"), levels[0].len_of("y"));
        let desc = format!("JPEG 2000 {:?}, {w} x {h}, {} level(s)", levels[0].dtype, levels.len());
        let var = Variable { name: name.clone(), group: String::new(), levels, bands, fill: None, scale: 1.0, offset: 0.0, units: String::new(), georef: Georef::None, times: Default::default(), dims: vec![] };
        Product { name, desc, vars: vec![var], small: vec![], valid: None, info: Default::default() }
    } else if nitf::is_nitf(&head) {
        nitf::open(&src, 0)?
    } else if head.starts_with(&[0x89, b'H', b'D', b'F']) {
        let (mut vars, small, one_d, info) = netcdf::variables(&src, 0)?;
        let all = vars.clone();
        for v in &mut vars {
            v.georef = netcdf::georef(&src, v, &one_d, &all);
        }
        let desc = format!("NetCDF-4 / HDF5, {} variables", vars.len() + small.len());
        Product { name, desc, vars, small, valid: None, info }
    } else if envisat::is_envisat(&head) {
        envisat::open(&src, 0, name)?
    } else if head.starts_with(&hdf4::MAGIC) {
        hdf4::open(&src, 0, name)?
    } else if netcdf3::is_netcdf3(&head) {
        netcdf3::open(&src, 0)?
    } else if grib::is_grib(&src) {
        grib::open(&src, 0, name)?
    } else {
        return Err(format!("{url}: unknown format (TIFF, COG, JPEG 2000, NITF, NetCDF-3, NetCDF-4, HDF4, HDF5, GRIB, Envisat N1, CEOS, Zarr, SAFE or OPeNDAP expected)").into());
    };
    let mut ds = Dataset { product, sources: vec![Arc::new(src)] };
    // Points (1D latitude and longitude): images on a grid of longitude and latitude.
    if ds.product.vars.is_empty() {
        points::add_images(&mut ds, rt)?;
    }
    if ds.product.vars.is_empty() {
        return Err(format!("{url}: no dataset with 2 or more dimensions, and no 1D latitude and longitude").into());
    }
    Ok(ds)
}

/// All physical values of variable `v` of `ds` at level 0, in C order, with NaN for the fill value. For the
/// small variables (`Product::small`): this reads the full array. This function blocks.
pub fn read_all(ds: &Dataset, v: &Variable) -> Result<Vec<f64>> {
    let a = &v.levels[0];
    // The chunk sizes divide below: no size of 0.
    eo_core::chunk_count(&a.shape, &a.chunk, usize::MAX)?;
    let n = a.shape.iter().try_fold(1u64, |n, &s| n.checked_mul(s)).unwrap_or(u64::MAX);
    if n > 50_000_000 {
        return Err(format!("{}: {n} values, more than the limit of 50 million", v.name).into());
    }
    let n = n as usize;
    let (grid, cs) = (a.grid(), a.chunk.clone());
    let mut out = vec![f64::NAN; n];
    for i in 0..a.chunks.len() {
        let c = a.chunks.at(i);
        let src = ds.sources.get(c.src as usize).ok_or("no such source")?;
        let raw = if c.len == 0 { bytes::Bytes::new() } else { src.read_chunk(c)? };
        if raw.is_empty() {
            continue;
        }
        let vals = codec::to_f64(a, &codec::decode(a, &raw)?);
        // Position of the chunk in the grid, then each value of the chunk in the array (edge chunks are padded).
        let mut pos = vec![0u64; grid.len()];
        let mut r = i as u64;
        for k in (0..grid.len()).rev() {
            pos[k] = r % grid[k];
            r /= grid[k];
        }
        for (j, x) in vals.iter().enumerate() {
            let (mut q, mut at, mut inside) = (j as u64, 0u64, true);
            let mut stride = 1u64;
            for k in (0..cs.len()).rev() {
                let p = pos[k] * cs[k] + q % cs[k];
                q /= cs[k];
                inside &= p < a.shape[k];
                at += p * stride;
                stride *= a.shape[k];
            }
            if inside && x.is_finite() && Some(*x) != v.fill {
                out[at as usize] = x * v.scale + v.offset;
            }
        }
    }
    Ok(out)
}

/// Crafted malformed inputs: each one gives an error (or a result), not a panic or a huge allocation.
#[cfg(test)]
mod malformed {
    use eo_core::*;

    fn decode(codecs: Vec<Codec>, src: &[u8]) -> Result<Vec<u8>> {
        let a = Array { dims: vec!["y".into(), "x".into()], shape: vec![4, 4], chunk: vec![4, 4], dtype: DType::F32, le: true, codecs, chunks: Chunks::List(vec![]), place: None };
        crate::codec::decode(&a, src).map(|c| c.into_owned())
    }

    #[test]
    fn malformed_inputs_are_errors() {
        // Blosc: 4 GB of data for a chunk of 64 bytes, then blocks larger than the data.
        let mut h = [2u8, 1, 0, 4, 0xff, 0xff, 0xff, 0xff, 0, 1, 0, 0, 0, 0, 0, 0];
        assert!(decode(vec![Codec::Blosc], &h).is_err());
        h[4..8].copy_from_slice(&64u32.to_le_bytes());
        assert!(decode(vec![Codec::Blosc], &h).is_err());
        // Shuffle with a value size of 0, and with less data than one value.
        assert!(decode(vec![Codec::Shuffle { size: 0 }], &[1, 2, 3]).is_ok());
        assert!(decode(vec![Codec::Shuffle { size: 8 }], &[1, 2, 3]).is_ok());
        // JPEG 2000: a tile width of 0 in the SIZ marker.
        let mut siz = vec![0xFF, 0x4F, 0xFF, 0x51, 0, 41, 0, 0];
        for v in [4u32, 4, 0, 0, 0, 4, 0, 0] {
            siz.extend(v.to_be_bytes());
        }
        siz.extend([0, 1, 7, 1, 1]);
        let tile = [0xFF, 0x90, 0, 10, 0, 0, 0, 0, 0, 12, 0, 1];
        assert!(decode(vec![Codec::Jpeg2000 { reduce: 0, header: siz.into() }], &tile).is_err());
        // GRIB 2: 4 billion data points for a grid of 16 points (section 5, then an empty section 7).
        let mut s = vec![0, 0, 0, 21, 5, 0xff, 0xff, 0xff, 0xff, 0, 0];
        s.extend([0; 8]);
        s.extend([8, 0, 0, 0, 0, 5, 7]);
        assert!(decode(vec![Codec::Grib { edition: 2 }], &s).is_err());
        // Zarr: a chunk size of 0.
        let dir = std::env::temp_dir().join(format!("eoview-malformed-{}.zarr", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = serde_json::json!({"zarr_format": 2, "shape": [4, 4], "chunks": [0, 4], "dtype": "<f4", "compressor": null, "filters": null, "order": "C"});
        std::fs::write(dir.join(".zmetadata"), serde_json::json!({"metadata": {".zgroup": {"zarr_format": 2}, "v/.zarray": a}}).to_string()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let r = crate::zarr::open(dir.to_str().unwrap(), rt.handle());
        std::fs::remove_dir_all(dir).unwrap();
        assert!(r.is_err());
    }
}
