//! NetCDF-3 files: classic (CDF-1), 64-bit offset (CDF-2) and 64-bit data (CDF-5). The values are big-endian
//! and not compressed. A variable without the record dimension is one contiguous chunk. A record variable has
//! one chunk for each record: the records of all record variables follow each other at the end of the file.
//! The CF attributes give the scale, the offset, the fill value and the units, as in `netcdf`.
//!
//! Specification: <https://docs.unidata.ucar.edu/netcdf-c/current/file_format_specifications.html>.
use crate::hdf5::Attr;
use crate::{Source, codec, netcdf};
use eo_core::*;
use std::sync::Arc;

pub fn is_netcdf3(head: &[u8]) -> bool {
    head.len() >= 4 && head.starts_with(b"CDF") && matches!(head[3], 1 | 2 | 5)
}

/// Big-endian reader of the header. `v` is the format version (1, 2 or 5).
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
    v: u8,
}

const SHORT: &str = "short NetCDF-3 header";

impl Rd<'_> {
    fn u(&mut self, n: usize) -> Result<u64> {
        let s = self.b.get(self.p..self.p + n).ok_or(SHORT)?;
        self.p += n;
        Ok(s.iter().fold(0, |a, &x| a << 8 | x as u64))
    }

    /// A count or a size: 8 bytes in CDF-5, else 4 bytes.
    fn n(&mut self) -> Result<u64> {
        self.u(if self.v == 5 { 8 } else { 4 })
    }

    /// `n` bytes, then the padding to a multiple of 4.
    fn bytes(&mut self, n: u64) -> Result<&[u8]> {
        let n = usize::try_from(n).map_err(|_| SHORT)?;
        let s = self.b.get(self.p..self.p.saturating_add(n)).ok_or(SHORT)?;
        self.p += n.div_ceil(4) * 4;
        Ok(s)
    }

    fn name(&mut self) -> Result<String> {
        let n = self.n()?;
        Ok(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }

    /// Number of items of a list with tag `tag` (0: the list is absent).
    fn list(&mut self, tag: u64) -> Result<u64> {
        let (t, n) = (self.u(4)?, self.n()?);
        if t != tag && (t, n) != (0, 0) {
            return Err(format!("bad NetCDF-3 header: tag {t} at byte {}", self.p).into());
        }
        Ok(n)
    }

    fn attrs(&mut self) -> Result<Vec<(String, Attr)>> {
        (0..self.list(12)?)
            .map(|_| {
                let name = self.name()?;
                let t = self.u(4)? as u32;
                let n = self.n()?;
                let s = size(t)?;
                let b = self.bytes(n.saturating_mul(s as u64))?;
                let a = if t == 2 { Attr::Text(String::from_utf8_lossy(b).trim_end_matches('\0').to_string()) } else { Attr::Num(b.chunks(s).map(|x| value(t, x)).collect()) };
                Ok((name, a))
            })
            .collect()
    }
}

/// Data type of NetCDF-3 type `t`. None for char (text) and for an unknown type.
fn dtype(t: u32) -> Option<DType> {
    Some(match t {
        1 => DType::I8,
        3 => DType::I16,
        4 => DType::I32,
        5 => DType::F32,
        6 => DType::F64,
        7 => DType::U8,
        8 => DType::U16,
        9 => DType::U32,
        10 => DType::I64,
        11 => DType::U64,
        _ => return None,
    })
}

fn size(t: u32) -> Result<usize> {
    match t {
        2 => Ok(1),
        _ => dtype(t).map(|d| d.size()).ok_or_else(|| format!("unknown NetCDF-3 type {t}").into()),
    }
}

/// One big-endian value of type `t`.
fn value(t: u32, b: &[u8]) -> f64 {
    let u = b.iter().fold(0u64, |a, &x| a << 8 | x as u64);
    match t {
        1 => u as u8 as i8 as f64,
        3 => u as u16 as i16 as f64,
        4 => u as u32 as i32 as f64,
        5 => f32::from_bits(u as u32) as f64,
        6 => f64::from_bits(u),
        10 => u as i64 as f64,
        _ => u as f64,
    }
}

fn num(a: &[(String, Attr)], k: &str) -> Option<f64> {
    match &a.iter().find(|x| x.0 == k)?.1 {
        Attr::Num(v) => v.first().copied(),
        Attr::Text(t) => t.parse().ok(),
    }
}

fn text<'a>(a: &'a [(String, Attr)], k: &str) -> Option<&'a str> {
    match &a.iter().find(|x| x.0 == k)?.1 {
        Attr::Text(t) => Some(t),
        _ => None,
    }
}

struct Var {
    name: String,
    dims: Vec<usize>,
    attrs: Vec<(String, Attr)>,
    t: u32,
    begin: u64,
}

/// The header: format version, number of records, dimensions (name, length; length 0 is the record dimension), global
/// attributes and variables.
type Header = (u8, u64, Vec<(String, u64)>, Vec<(String, Attr)>, Vec<Var>);

fn header(b: &[u8]) -> Result<Header> {
    let mut r = Rd { b, p: 4, v: b[3] };
    let recs = r.n()?;
    let dims = (0..r.list(10)?).map(|_| Ok((r.name()?, r.n()?))).collect::<Result<Vec<_>>>()?;
    let global = r.attrs()?;
    let mut vars = vec![];
    for _ in 0..r.list(11)? {
        let name = r.name()?;
        let dims_of = (0..r.n()?).map(|_| r.n().map(|d| d as usize)).collect::<Result<Vec<_>>>()?;
        if dims_of.iter().any(|&d| d >= dims.len()) {
            return Err(format!("{name}: bad dimension id").into());
        }
        let attrs = r.attrs()?;
        let t = r.u(4)? as u32;
        size(t)?;
        r.n()?; // vsize: computed below (it does not fit in 32 bits for a large variable)
        let begin = r.u(if r.v == 1 { 4 } else { 8 })?;
        vars.push(Var { name, dims: dims_of, attrs, t, begin });
    }
    Ok((r.v, recs, dims, global, vars))
}

/// Read the header of `src`: first 64 kB, then more if the header is longer.
fn read_header(src: &Source) -> Result<Header> {
    let len = src.len()?;
    let mut n = 1 << 16;
    loop {
        match header(&src.read(0..n.min(len))?) {
            Err(e) if e.0 == SHORT && n < len => n *= 8,
            r => return r,
        }
    }
}

/// The product of a NetCDF-3 file. `idx` is the index of `src` in the dataset sources.
pub fn open(src: &Source, idx: u32) -> Result<Product> {
    let (version, mut recs, dims, global, vars) = read_header(src)?;
    let is_rec = |v: &Var| v.dims.first().is_some_and(|&d| dims[d].1 == 0);
    // Bytes of one record of a variable (without the record dimension), and the size of one record of the file:
    // each variable has a padding to 4 bytes, except if there is only one record variable.
    let bytes = |v: &Var| v.dims.iter().filter(|&&d| dims[d].1 != 0).map(|&d| dims[d].1).product::<u64>() * size(v.t).unwrap_or(1) as u64;
    let rec_vars: Vec<&Var> = vars.iter().filter(|v| is_rec(v)).collect();
    let recsize = match rec_vars[..] {
        [v] => bytes(v),
        _ => rec_vars.iter().map(|v| bytes(v).div_ceil(4) * 4).sum(),
    };
    // Streaming files: the number of records is not in the header.
    if (recs == u32::MAX as u64 || recs == u64::MAX)
        && let Some(v) = rec_vars.first()
    {
        recs = (src.len()?.saturating_sub(v.begin)).checked_div(recsize).unwrap_or(0);
    }
    // One chunk location for each record: the number comes from the file.
    if !rec_vars.is_empty() && recs > MAX_CHUNKS as u64 {
        return Err(format!("NetCDF-3 file with {recs} records").into());
    }
    let len = |d: usize| if dims[d].1 == 0 { recs } else { dims[d].1 };
    let mut big = vec![];
    let mut small = vec![];
    for v in vars.iter().filter(|v| v.t != 2) {
        let shape: Vec<u64> = v.dims.iter().map(|&d| len(d)).collect();
        if shape.contains(&0) {
            continue;
        }
        let mut chunk = shape.clone();
        let chunks: Vec<ChunkLoc> = if is_rec(v) {
            chunk[0] = 1;
            (0..recs).map(|i| ChunkLoc { src: idx, off: v.begin + i * recsize, len: bytes(v) }).collect()
        } else {
            vec![ChunkLoc { src: idx, off: v.begin, len: bytes(v) }]
        };
        let names: Vec<String> = v.dims.iter().map(|&d| dims[d].0.clone()).collect();
        let n = names.len();
        // Image axes: the last two dimensions. A dimension "time" before them is the time dimension.
        let axes = names.iter().enumerate().map(|(k, d)| match n - k {
            1 if n > 1 => "x".into(),
            2 => "y".into(),
            _ if d.eq_ignore_ascii_case("time") && n > 1 => "time".into(),
            _ if n > 1 && ["y", "x", "time", "band"].contains(&d.as_str()) => format!("{d}_"),
            _ => d.clone(),
        });
        let a = Array { dims: axes.collect(), shape, chunk, dtype: dtype(v.t).unwrap(), le: false, codecs: vec![], chunks: chunks.into(), place: None };
        let var = Variable {
            name: v.name.clone(),
            group: String::new(),
            levels: vec![a],
            bands: vec![v.name.clone()],
            fill: num(&v.attrs, "_FillValue"),
            scale: num(&v.attrs, "scale_factor").unwrap_or(1.0),
            offset: num(&v.attrs, "add_offset").unwrap_or(0.0),
            units: text(&v.attrs, "units").unwrap_or("").to_string(),
            georef: Georef::None,
            times: Default::default(),
            dims: names,
        };
        if n >= 2 { big.push(var) } else { small.push(var) }
    }
    // Values of a 1D variable, with scale and offset (coordinates: small).
    let read = |v: &Variable| -> Option<Vec<f64>> {
        let a = &v.levels[0];
        let mut out = vec![];
        for c in a.chunks.iter() {
            out.extend(codec::to_f64(a, &src.read(c.off..c.off + c.len).ok()?));
        }
        Some(out.into_iter().map(|x| x * v.scale + v.offset).collect())
    };
    let one_d: Vec<&Variable> = small.iter().filter(|v| v.dims.len() == 1).collect();
    let names: Vec<(&str, u64)> = one_d.iter().map(|v| (v.name.as_str(), v.levels[0].shape[0])).collect();
    // The times of the coordinate variable "time", shared by all variables.
    let times = one_d.iter().find(|v| v.dims[0].eq_ignore_ascii_case("time") && v.name == v.dims[0]).and_then(|c| {
        let (unit, t0) = time::cf(text(&vars.iter().find(|v| v.name == c.name)?.attrs, "units")?)?;
        Some(Arc::new(read(c)?.into_iter().map(|x| t0 + x * unit).collect::<Vec<f64>>()))
    });
    let all = big.clone();
    for v in &mut big {
        v.georef = netcdf::cf_georef(v, &names, |i| read(one_d[i]), &all);
        if let Some(t) = times.as_ref().filter(|_| v.levels[0].axis("time").is_some()) {
            v.times = t.clone();
        }
    }
    let attrs = |a: &[(String, Attr)]| a.iter().map(|(k, x)| (k.clone(), x.text())).collect();
    let info = Info {
        attrs: attrs(&global),
        dims: (0..dims.len()).map(|d| (dims[d].0.clone(), len(d))).collect(),
        vars: vars
            .iter()
            .map(|v| Meta { name: v.name.clone(), dims: v.dims.iter().map(|&d| (dims[d].0.clone(), len(d))).collect(), dtype: dtype(v.t).map_or("char".into(), |d| format!("{d:?}").to_lowercase()), attrs: attrs(&v.attrs) })
            .collect(),
    };
    if big.is_empty() {
        return Err(format!("{}: no variable with 2 or more dimensions", src.name()).into());
    }
    let name = src.name().rsplit(['/', '\\']).next().unwrap_or("").to_string();
    let kind = ["", "classic", "64-bit offset", "", "", "64-bit data"][version as usize];
    let desc = format!("NetCDF-3 {kind}, {} variables", big.len());
    Ok(Product { name, desc, vars: big, small, valid: None, info })
}
