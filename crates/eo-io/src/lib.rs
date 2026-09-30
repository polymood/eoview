//! Format readers, byte sources and codecs. A reader only describes the chunks of a product.
//! The chunk engine in `eo-cache` reads and decodes them.
pub mod codec;
pub mod source;
pub mod tiff;

pub use source::Source;

use eo_core::{Product, Result};
use std::sync::Arc;
use tokio::runtime::Handle;

/// A product and its byte sources. `ChunkLoc::src` is an index into `sources`.
pub struct Dataset {
    pub product: Product,
    pub sources: Vec<Arc<Source>>,
}

/// Open a local path or an HTTP(S) URL. This function blocks: do not call it on the UI thread.
pub fn open(url: &str, rt: &Handle) -> Result<Dataset> {
    let src = Source::open(url, rt)?;
    // Sparse reads until the engine has the value sample.
    src.sparse(true);
    let head = src.read(0..16.min(src.len()))?;
    let product = if tiff::is_tiff(&head) {
        tiff::open(&src)?
    } else {
        return Err(format!("{url}: unknown format (TIFF or COG expected)").into());
    };
    Ok(Dataset { product, sources: vec![Arc::new(src)] })
}
