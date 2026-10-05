//! Data model of eoview. A product contains variables. A variable is a chunked N-dimensional
//! array at one or more resolution levels. Readers fill this model. They do not read pixel data.
pub mod geo;

use std::fmt;

#[derive(Debug, Clone)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.into())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    CI16,
    CI32,
    CF32,
    CF64,
}

impl DType {
    /// Size of one value in bytes. A complex value has two parts.
    pub fn size(self) -> usize {
        let n = if self.is_complex() { 2 } else { 1 };
        n * match self.part() {
            DType::U8 | DType::I8 => 1,
            DType::U16 | DType::I16 => 2,
            DType::U32 | DType::I32 | DType::F32 => 4,
            _ => 8,
        }
    }

    pub fn is_complex(self) -> bool {
        matches!(self, DType::CI16 | DType::CI32 | DType::CF32 | DType::CF64)
    }

    /// Type of one part of a complex value. For a real type, the type itself.
    pub fn part(self) -> DType {
        match self {
            DType::CI16 => DType::I16,
            DType::CI32 => DType::I32,
            DType::CF32 => DType::F32,
            DType::CF64 => DType::F64,
            t => t,
        }
    }
}

/// One step of a codec chain, in decode order: the first step applies first to the encoded bytes.
#[derive(Clone, PartialEq, Debug)]
pub enum Codec {
    /// zlib stream (TIFF Deflate, Zarr zlib, HDF5 deflate).
    Deflate,
    Gzip,
    Lzw,
    Zstd,
    PackBits,
    /// LZ4 block. `header`: a 4-byte little-endian decoded size comes first (numcodecs).
    Lz4 { header: bool },
    /// Blosc 1 container (all its compressors except BloscLZ, byte shuffle and bit shuffle).
    Blosc,
    /// Byte shuffle of values of `size` bytes (HDF5 shuffle filter, numcodecs shuffle).
    Shuffle { size: u32 },
    /// Delta filter (numcodecs): each value is the sum of all previous encoded values.
    Delta,
    /// A 4-byte checksum at the end (Zarr crc32c, HDF5 Fletcher-32). The decoder removes it.
    Checksum,
    /// TIFF predictor: 2 (horizontal differencing) or 3 (floating point).
    /// `stride` is the number of values in one pixel. `row` is the number of values in one row.
    Predictor { kind: u16, stride: u32, row: u32 },
    /// JPEG stream (8-bit). `tables`: shared quantization and Huffman tables (TIFF JPEGTables), or empty.
    Jpeg { tables: std::sync::Arc<[u8]> },
    /// JPEG 2000 tile: the chunk has the tile parts of one tile. `header` is the main header of the
    /// codestream. `reduce`: number of resolution levels to discard.
    Jpeg2000 { reduce: u8, header: std::sync::Arc<[u8]> },
}

/// Location of the encoded bytes of one chunk. `len` 0 means that the chunk does not exist:
/// all its values are the fill value.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ChunkLoc {
    /// Index into the byte sources of the product.
    pub src: u32,
    pub off: u64,
    /// Length in bytes. `WHOLE`: all bytes of the source (for example a Zarr chunk object).
    pub len: u64,
}

impl ChunkLoc {
    pub const WHOLE: u64 = u64::MAX;
}

/// Chunked N-dimensional array. Dimension names include "y", "x", "band" and "time".
/// The values in a chunk are in C order (the last dimension changes fastest).
#[derive(Clone, Debug)]
pub struct Array {
    pub dims: Vec<String>,
    pub shape: Vec<u64>,
    pub chunk: Vec<u64>,
    pub dtype: DType,
    /// True for little-endian values.
    pub le: bool,
    pub codecs: Vec<Codec>,
    /// One location for each chunk, in C order over the chunk grid.
    pub chunks: Vec<ChunkLoc>,
    /// Position of an overview on level 0: [kx, ky, ox, oy]. Level-0 pixel position = (ox + col * kx, oy + row * ky).
    /// None: the overview covers the same area as level 0 (kx = level-0 width / width, GDAL convention).
    pub place: Option<[f64; 4]>,
}

impl Array {
    pub fn axis(&self, name: &str) -> Option<usize> {
        self.dims.iter().position(|d| d == name)
    }

    /// Size along the named dimension. 1 if the dimension does not exist.
    pub fn len_of(&self, name: &str) -> u64 {
        self.axis(name).map_or(1, |a| self.shape[a])
    }

    /// Number of chunks along each dimension.
    pub fn grid(&self) -> Vec<u64> {
        self.shape.iter().zip(&self.chunk).map(|(s, c)| s.div_ceil(*c)).collect()
    }

    /// Index in `chunks` of the chunk at grid position `pos`.
    pub fn chunk_index(&self, pos: &[u64]) -> usize {
        let g = self.grid();
        pos.iter().zip(&g).fold(0u64, |i, (p, n)| i * n + p) as usize
    }

    /// Decoded size of one chunk in bytes.
    pub fn chunk_bytes(&self) -> usize {
        self.chunk.iter().product::<u64>() as usize * self.dtype.size()
    }

    /// Check that the chunk table agrees with the shape.
    pub fn validate(&self) -> Result<()> {
        let n = self.dims.len();
        if self.shape.len() != n || self.chunk.len() != n {
            return Err("array rank mismatch".into());
        }
        if self.shape.contains(&0) || self.chunk.contains(&0) {
            return Err("empty array or chunk".into());
        }
        let need = self.grid().iter().product::<u64>() as usize;
        if self.chunks.len() != need {
            return Err(format!("{} chunk locations, {need} expected", self.chunks.len()).into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Crs {
    pub epsg: Option<u32>,
    /// Short text for the user, for example "WGS 84 / UTM zone 32N".
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Georef {
    /// No georeferencing. The view shows pixel coordinates.
    #[default]
    None,
    /// GDAL order: x = gt[0] + col * gt[1] + row * gt[2], y = gt[3] + col * gt[4] + row * gt[5].
    /// (col, row) is the top-left corner of a pixel at level 0.
    Affine { gt: [f64; 6], crs: Crs },
    /// Longitude and latitude (degrees, WGS 84) at the nodes of a grid of level-0 pixel positions.
    /// Node (i, j) is at position (cols[i], rows[j]) (0, 0 is the top-left corner of the image).
    /// The values are in row order: index j * cols.len() + i. Sources: Sentinel-1 geolocation grid,
    /// tie-point grids, geolocation arrays.
    Grid { cols: Vec<f64>, rows: Vec<f64>, lon: Vec<f64>, lat: Vec<f64> },
    /// Longitude and latitude variables of the product (degrees). Value (i, j) of these variables is at
    /// level-0 position (off[0] + i * step[0], off[1] + j * step[1]). The engine reads them.
    Arrays { lon: String, lat: String, step: [f64; 2], off: [f64; 2] },
}

impl Georef {
    /// Coordinates of level-0 pixel position (col, row) in the CRS of `crs()`. None for `Arrays` (use the engine).
    pub fn map(&self, col: f64, row: f64) -> Option<(f64, f64)> {
        match self {
            Georef::None | Georef::Arrays { .. } => None,
            Georef::Affine { gt, .. } => Some((gt[0] + col * gt[1] + row * gt[2], gt[3] + col * gt[4] + row * gt[5])),
            Georef::Grid { cols, rows, lon, lat } => Some((bilinear(cols, rows, lon, col, row), bilinear(cols, rows, lat, col, row))),
        }
    }

    pub fn crs(&self) -> Option<Crs> {
        match self {
            Georef::None => None,
            Georef::Affine { crs, .. } => Some(crs.clone()),
            _ => Some(Crs::wgs84()),
        }
    }
}

impl Crs {
    pub fn wgs84() -> Crs {
        Crs { epsg: Some(4326), name: "WGS 84".into() }
    }
}

/// Bilinear interpolation of grid values `v` at position (x, y). Outside the grid: linear extrapolation
/// from the edge cell.
pub fn bilinear(xs: &[f64], ys: &[f64], v: &[f64], x: f64, y: f64) -> f64 {
    let cell = |a: &[f64], p: f64| {
        let i = a.partition_point(|&q| q <= p).clamp(1, a.len().max(2) - 1) - 1;
        let t = if a.len() > 1 { (p - a[i]) / (a[i + 1] - a[i]) } else { 0.0 };
        (i, t)
    };
    let ((i, tx), (j, ty)) = (cell(xs, x), cell(ys, y));
    let n = xs.len();
    let at = |i: usize, j: usize| v[j.min(ys.len() - 1) * n + i.min(n - 1)];
    let top = at(i, j) * (1.0 - tx) + at(i + 1, j) * tx;
    let bot = at(i, j + 1) * (1.0 - tx) + at(i + 1, j + 1) * tx;
    top * (1.0 - ty) + bot * ty
}

#[derive(Clone, Debug)]
pub struct Variable {
    pub name: String,
    /// Group of the variable in the product tree, as a path with '/'. The reader sets it: the groups
    /// depend on the format. Empty: the tree uses the directory part of `name`.
    pub group: String,
    /// Level 0 has the full resolution. The next levels are overviews, from fine to coarse.
    /// All levels have the same dimensions in the same order.
    pub levels: Vec<Array>,
    /// Names of the bands. The length agrees with the "band" dimension (1 if it does not exist).
    pub bands: Vec<String>,
    pub fill: Option<f64>,
    /// Physical value = stored value * scale + offset.
    pub scale: f64,
    pub offset: f64,
    pub units: String,
    pub georef: Georef,
}

impl Variable {
    /// Width and height at level 0.
    pub fn size(&self) -> (u64, u64) {
        let a = &self.levels[0];
        (a.len_of("x"), a.len_of("y"))
    }
}

#[derive(Clone, Debug)]
pub struct Product {
    pub name: String,
    /// Short format description for the user.
    pub desc: String,
    pub vars: Vec<Variable>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_index_is_c_order() {
        let a = Array {
            dims: vec!["band".into(), "y".into(), "x".into()],
            shape: vec![3, 100, 250],
            chunk: vec![1, 64, 64],
            dtype: DType::CI16,
            le: true,
            codecs: vec![],
            chunks: vec![ChunkLoc { src: 0, off: 0, len: 0 }; 3 * 2 * 4],
            place: None,
        };
        assert_eq!(a.grid(), [3, 2, 4]);
        assert_eq!(a.chunk_index(&[2, 1, 3]), 23);
        assert_eq!(a.chunk_bytes(), 64 * 64 * 4);
        a.validate().unwrap();
    }

    #[test]
    fn grid_interpolates() {
        let g = Georef::Grid { cols: vec![0., 10.], rows: vec![0., 10., 20.], lon: vec![0., 1., 0., 1., 0., 1.], lat: vec![5., 5., 4., 4., 3., 3.] };
        assert_eq!(g.map(5.0, 15.0), Some((0.5, 3.5)));
        assert_eq!(g.map(20.0, 0.0), Some((2.0, 5.0)));
    }
}
