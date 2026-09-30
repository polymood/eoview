//! Format readers, byte sources and codecs. A reader only describes the chunks of a product.
//! The chunk engine in `eo-cache` reads and decodes them.
mod blosc;
pub mod hdf5;
pub mod netcdf;
pub mod nitf;
pub mod codec;
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
    let path = std::path::Path::new(url);
    let remote_zarr = source::is_remote(url) && (url.trim_end_matches('/').ends_with(".zarr") || url.contains(".zarr/"));
    if remote_zarr || (path.is_dir() && zarr::is_zarr(url, rt)) {
        return zarr::open(url.trim_end_matches('/'), rt);
    }
    if !source::is_remote(url) {
        if path.is_dir() {
            return safe::open(path, rt);
        }
        if let Some(dir) = path.parent().filter(|d| safe::kind(d).is_some()) {
            return safe::open(dir, rt);
        }
    }
    let src = Source::open(url, rt)?;
    // Sparse reads until the engine has the value sample.
    src.sparse(true);
    let head = src.read(0..16.min(src.len()?))?;
    let name = url.rsplit(['/', '\\']).next().unwrap_or("").to_string();
    let product = if tiff::is_tiff(&head) {
        tiff::open(&src, 0)?
    } else if jp2::is_jp2(&head) {
        let levels = jp2::arrays(&src, 0)?;
        let bands = (1..=levels[0].len_of("band")).map(|b| format!("Band {b}")).collect();
        let (w, h) = (levels[0].len_of("x"), levels[0].len_of("y"));
        let desc = format!("JPEG 2000 {:?}, {w} x {h}, {} level(s)", levels[0].dtype, levels.len());
        let var = Variable { name: name.clone(), levels, bands, fill: None, scale: 1.0, offset: 0.0, units: String::new(), georef: Georef::None };
        Product { name, desc, vars: vec![var] }
    } else if nitf::is_nitf(&head) {
        nitf::open(&src, 0)?
    } else if head.starts_with(&[0x89, b'H', b'D', b'F']) {
        let (mut vars, one_d) = netcdf::variables(&src, 0)?;
        let all = vars.clone();
        for v in &mut vars {
            v.georef = netcdf::georef(&src, v, &one_d, &all);
        }
        if vars.is_empty() {
            return Err(format!("{url}: no dataset with 2 or more dimensions").into());
        }
        let desc = format!("NetCDF-4 / HDF5, {} variables", vars.len());
        Product { name, desc, vars }
    } else {
        return Err(format!("{url}: unknown format (TIFF, COG, JPEG 2000, NITF, NetCDF-4, HDF5, Zarr or SAFE expected)").into());
    };
    Ok(Dataset { product, sources: vec![Arc::new(src)] })
}
