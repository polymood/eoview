//! HDF4 files (HDF-EOS 2 files are HDF4 files): the scientific data sets (SDS) of the SD interface. The
//! reader finds the variables, their dimensions, their attributes and the byte ranges of their data. It
//! does not read data: the chunk engine reads and decodes the chunks.
//!
//! Specifications: HDF4 Specification and Developer's Guide, <https://support.hdfgroup.org/archive/support/release4/doc/DS.pdf>;
//! HDF-EOS 2 grids, <https://hdfeos.org/>.
//!
//! The file is a list of data descriptors (DD): a tag, a reference number, an offset and a length. The HDF4
//! library writes a variable as a vgroup of class `Var0.0`. Its dimensions are vgroups of class `Dim0.0`
//! (or `UDim0.0`), its attributes are vdatas of class `Attr0.0`, and its data is the element of tag SD.
//! The SDD element gives the shape and the NT element gives the number type. The global attributes are in
//! the vgroup of class `CDF0.0`. The values are big-endian in most files.
//!
//! Supported: contiguous data, deflate compression, chunked data (each HDF4 chunk is one chunk of the
//! array, with or without deflate), linked blocks (all vdatas, and the data of a variable if the blocks are
//! adjacent in the file), HDF-EOS 2 grids with the geographic projection (GCTP_GEO). Not supported: the
//! compressions RLE, skipping Huffman, NBIT, SZIP and JPEG, external files, the data of a variable in
//! linked blocks that are not adjacent, the other GCTP projections.
use crate::hdf5::Attr;
use crate::{Source, codec, netcdf};
use eo_core::*;
use std::collections::{HashMap, HashSet};

const SPECIAL: u16 = 0x4000;
const LINKED: u16 = 20;
const COMPRESSED: u16 = 40;
const NT: u16 = 106;
const SDD: u16 = 701;
const SD: u16 = 702;
const NDG: u16 = 720;
const VH: u16 = 1962;
const VS: u16 = 1963;
const VG: u16 = 1965;

pub const MAGIC: [u8; 4] = [0x0e, 0x03, 0x13, 0x01];

/// Cursor over a byte slice, big-endian.
struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl Cur<'_> {
    fn u(&mut self, n: usize) -> Result<u64> {
        let s = self.b.get(self.p..self.p + n).ok_or("truncated HDF4 element")?;
        self.p += n;
        Ok(s.iter().fold(0u64, |v, &x| v << 8 | x as u64))
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(self.u(2)? as u16)
    }

    /// A text with its length first (16 bits).
    fn text(&mut self) -> Result<String> {
        let n = self.u(2)? as usize;
        let s = self.b.get(self.p..self.p + n).ok_or("truncated HDF4 element")?;
        self.p += n;
        Ok(String::from_utf8_lossy(s).into_owned())
    }
}

/// Header of a vdata: name, class, number of records, record size, and the fields (name, number type,
/// offset in the record, number of values).
struct Vdata {
    name: String,
    class: String,
    n: usize,
    size: usize,
    fields: Vec<(String, u16, usize, usize)>,
}

/// A vgroup: its elements (tag, reference), name and class.
type Vgroup = (Vec<(u16, u16)>, String, String);

/// The file: its byte source and its data descriptors, (tag, reference) to (offset, length).
struct File<'a> {
    src: &'a Source,
    dds: HashMap<(u16, u16), (u64, u64)>,
}

impl File<'_> {
    /// Bytes of a plain element. Empty if it has no data.
    fn raw(&self, tag: u16, r: u16) -> Result<Vec<u8>> {
        let &(off, len) = self.dds.get(&(tag, r)).ok_or_else(|| format!("no HDF4 element {tag}/{r}"))?;
        if off == u32::MAX as u64 || len == 0 || len == u32::MAX as u64 {
            return Ok(vec![]);
        }
        Ok(self.src.read(off..off + len)?.to_vec())
    }

    /// All bytes of an element: a plain element or linked blocks.
    fn read(&self, tag: u16, r: u16) -> Result<Vec<u8>> {
        if self.dds.contains_key(&(tag, r)) {
            return self.raw(tag, r);
        }
        let (len, blocks) = self.linked(&self.raw(tag | SPECIAL, r)?)?;
        let mut out = vec![];
        // The blocks can come again: stop at the length of the element.
        for (o, n) in blocks {
            if out.len() as u64 >= len {
                break;
            }
            out.extend_from_slice(&self.src.read(o..o + n)?);
        }
        out.truncate(len as usize);
        Ok(out)
    }

    /// Length and blocks (offset, length) of a linked-block element, from its special header.
    fn linked(&self, h: &[u8]) -> Result<(u64, Vec<(u64, u64)>)> {
        let mut c = Cur { b: h, p: 0 };
        if c.u(2)? != 1 {
            return Err("HDF4 special element: linked blocks expected".into());
        }
        let (len, _block, n) = (c.u(4)?, c.u(4)?, c.u(4)? as usize);
        let mut link = c.u16()?;
        let mut out = vec![];
        let mut seen = HashSet::new();
        // Each link table has the reference of the next table, then `n` block references (0: no block).
        for _ in 0..1 << 16 {
            if link == 0 {
                break;
            }
            if !seen.insert(link) {
                return Err("HDF4 link tables make a cycle".into());
            }
            let t = self.raw(LINKED, link)?;
            let mut c = Cur { b: &t, p: 0 };
            link = c.u16()?;
            for _ in 0..n {
                match c.u16()? {
                    0 => {}
                    b => out.push(*self.dds.get(&(LINKED, b)).ok_or("missing HDF4 linked block")?),
                }
            }
        }
        Ok((len, out))
    }

    /// Location of the data of an element: (offset, length, codecs). None: the element has no data.
    fn loc(&self, tag: u16, r: u16) -> Result<Option<(u64, u64, Vec<Codec>)>> {
        if let Some(&(off, len)) = self.dds.get(&(tag, r)) {
            return Ok((off != u32::MAX as u64 && len != 0).then_some((off, len, vec![])));
        }
        let Ok(h) = self.raw(tag | SPECIAL, r) else { return Ok(None) };
        let mut c = Cur { b: &h, p: 0 };
        match c.u(2)? {
            1 => {
                let (len, blocks) = self.linked(&h)?;
                match blocks.first() {
                    None => Ok(None),
                    Some(&(off, _)) if blocks.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0) => Ok(Some((off, len, vec![]))),
                    _ => Err("HDF4 data in linked blocks that are not adjacent is not supported".into()),
                }
            }
            // Compressed data in compressed data: a malformed file (the recursion can be endless).
            3 if tag == COMPRESSED => Err("HDF4 compressed element in a compressed element".into()),
            3 => {
                // Version, decoded length, reference of the compressed data, model (no model data), coder.
                let (_ver, _len, cref, _model, coder) = (c.u(2)?, c.u(4)?, c.u16()?, c.u(2)?, c.u(2)?);
                let codec = match coder {
                    0 => None,
                    4 => Some(Codec::Deflate),
                    k => {
                        let name = match k {
                            1 => "RLE",
                            2 => "NBIT",
                            3 => "skipping Huffman",
                            5 => "SZIP",
                            7 => "JPEG",
                            _ => "unknown",
                        };
                        return Err(format!("HDF4 compression {name} is not supported").into());
                    }
                };
                Ok(self.loc(COMPRESSED, cref)?.map(|(o, n, mut cs)| {
                    cs.extend(codec);
                    (o, n, cs)
                }))
            }
            2 => Err("HDF4 data in an external file is not supported".into()),
            k => Err(format!("HDF4 special element {k} is not supported").into()),
        }
    }

    fn vgroup(&self, r: u16) -> Result<Vgroup> {
        let b = self.read(VG, r)?;
        let mut c = Cur { b: &b, p: 0 };
        let n = c.u(2)? as usize;
        let tags = (0..n).map(|_| c.u16()).collect::<Result<Vec<_>>>()?;
        let refs = (0..n).map(|_| c.u16()).collect::<Result<Vec<_>>>()?;
        Ok((tags.into_iter().zip(refs).collect(), c.text()?, c.text()?))
    }

    fn vdata(&self, r: u16) -> Result<Vdata> {
        let b = self.read(VH, r)?;
        let mut c = Cur { b: &b, p: 2 };
        let (n, size, nf) = (c.u(4)? as usize, c.u(2)? as usize, c.u(2)? as usize);
        let mut col = || (0..nf).map(|_| c.u(2)).collect::<Result<Vec<_>>>();
        let (types, _sizes, offs, orders) = (col()?, col()?, col()?, col()?);
        let names = (0..nf).map(|_| c.text()).collect::<Result<Vec<_>>>()?;
        let fields = (0..nf).map(|i| (names[i].clone(), types[i] as u16, offs[i] as usize, orders[i] as usize)).collect();
        Ok(Vdata { name: c.text()?, class: c.text()?, n, size, fields })
    }

    /// The value of an attribute (a vdata of class `Attr0.0` with one field).
    fn attr(&self, r: u16, h: &Vdata) -> Result<Attr> {
        let b = self.read(VS, r)?;
        let t = h.fields.first().ok_or("HDF4 attribute without field")?.1;
        if t & 0xff == 4 {
            return Ok(Attr::Text(String::from_utf8_lossy(&b).trim_end_matches('\0').trim().to_string()));
        }
        let dtype = nt(t as u8).ok_or_else(|| format!("HDF4 attribute {}: number type {t} is not supported", h.name))?;
        Ok(Attr::Num(nums(&b, dtype, t & 0x4000 != 0)))
    }

    /// The attributes in the elements of a vgroup.
    fn attrs(&self, els: &[(u16, u16)]) -> Vec<(String, Attr)> {
        let one = |r: u16| {
            let h = self.vdata(r).ok().filter(|h| h.class == "Attr0.0")?;
            Some((h.name.clone(), self.attr(r, &h).ok()?))
        };
        els.iter().filter(|e| e.0 == VH).filter_map(|e| one(e.1)).collect()
    }

    /// Chunks of a chunked SDS from its special header: the chunk shape, and the location of each chunk in C
    /// order over the grid. The chunk table is a vdata: chunk position (in chunks), tag and reference.
    fn chunked(&self, h: &[u8], shape: &[u64], src: u32) -> Result<(Vec<u64>, Vec<ChunkLoc>, Vec<Codec>)> {
        let mut c = Cur { b: h, p: 2 };
        // Header length, version, flags, length, chunk size, number type size, chunk table tag.
        let _ = (c.u(4)?, c.u(1)?, c.u(4)?, c.u(4)?, c.u(4)?, c.u(4)?, c.u(2)?);
        let table = c.u16()?;
        let _ = c.u(4)?;
        let nd = c.u(4)? as usize;
        let mut chunk = vec![];
        for _ in 0..nd {
            let _ = (c.u(4)?, c.u(4)?);
            chunk.push(c.u(4)?);
        }
        if nd != shape.len() || chunk.contains(&0) {
            return Err("bad HDF4 chunk header".into());
        }
        let count = chunk_count(shape, &chunk, MAX_CHUNKS)?;
        let grid: Vec<u64> = shape.iter().zip(&chunk).map(|(s, c)| s.div_ceil(*c)).collect();
        let mut locs = vec![ChunkLoc { src, off: 0, len: 0 }; count];
        let t = self.vdata(table)?;
        let field = |n: &str| t.fields.iter().find(|f| f.0 == n).map(|f| f.2).ok_or("bad HDF4 chunk table");
        let (origin, tag, rf) = (field("origin")?, field("chk_tag")?, field("chk_ref")?);
        let b = self.read(VS, table)?;
        let mut codecs = None;
        for rec in b.chunks_exact(t.size.max(1)).take(t.n) {
            let mut c = Cur { b: rec, p: origin };
            let pos = (0..nd).map(|_| c.u(4)).collect::<Result<Vec<_>>>()?;
            let (tag, r) = (Cur { b: rec, p: tag }.u16()?, Cur { b: rec, p: rf }.u16()?);
            let Some((off, len, cs)) = self.loc(tag, r)? else { continue };
            if *codecs.get_or_insert_with(|| cs.clone()) != cs {
                return Err("HDF4 chunks with different compressions are not supported".into());
            }
            if pos.iter().zip(&grid).all(|(p, g)| p < g) {
                locs[pos.iter().zip(&grid).fold(0, |i, (p, g)| i * g + p) as usize] = ChunkLoc { src, off, len };
            }
        }
        Ok((chunk, locs, codecs.unwrap_or_default()))
    }

    /// The array of an SDS: its data element is `sd` (None: no data was written).
    fn array(&self, sd: Option<u16>, shape: &[u64], dims: Vec<String>, dtype: DType, le: bool, src: u32) -> Result<Array> {
        let special = sd.filter(|r| !self.dds.contains_key(&(SD, *r))).and_then(|r| self.raw(SD | SPECIAL, r).ok());
        let (chunk, chunks, codecs) = match (special, sd) {
            (Some(h), _) if h.starts_with(&[0, 5]) => self.chunked(&h, shape, src)?,
            (_, Some(r)) => {
                let (off, len, cs) = self.loc(SD, r)?.unwrap_or((0, 0, vec![]));
                (shape.to_vec(), vec![ChunkLoc { src, off, len }], cs)
            }
            _ => (shape.to_vec(), vec![ChunkLoc { src, off: 0, len: 0 }], vec![]),
        };
        let a = Array { dims, shape: shape.to_vec(), chunk, dtype, le, codecs, chunks: chunks.into(), place: None };
        a.validate()?;
        Ok(a)
    }
}

/// Data type of an HDF4 number type (DFNT). None for text (char8) and other types.
fn nt(t: u8) -> Option<DType> {
    Some(match t {
        3 | 21 => DType::U8,
        20 => DType::I8,
        22 => DType::I16,
        23 => DType::U16,
        24 => DType::I32,
        25 => DType::U32,
        26 => DType::I64,
        27 => DType::U64,
        5 => DType::F32,
        6 => DType::F64,
        _ => return None,
    })
}

fn nums(b: &[u8], dtype: DType, le: bool) -> Vec<f64> {
    let a = Array { dims: vec![], shape: vec![], chunk: vec![], dtype, le, codecs: vec![], chunks: Chunks::List(vec![]), place: None };
    codec::to_f64(&a, b)
}

/// The data descriptors of the file. The DD blocks are a list: each block has the offset of the next one.
fn dds(src: &Source) -> Result<HashMap<(u16, u16), (u64, u64)>> {
    let len = src.len()?;
    let mut out = HashMap::new();
    let mut off = 4;
    let mut seen = HashSet::new();
    for _ in 0..1 << 20 {
        // A DD block that comes again is a cycle: its descriptors are already read.
        if off == 0 || off + 6 > len || !seen.insert(off) {
            break;
        }
        let h = src.read(off..off + 6)?;
        let mut c = Cur { b: &h, p: 0 };
        let (n, next) = (c.u(2)?, c.u(4)?);
        let b = src.read(off + 6..(off + 6 + 12 * n).min(len))?;
        for d in b.as_chunks::<12>().0 {
            let mut c = Cur { b: d, p: 0 };
            let (t, r, o, l) = (c.u16()?, c.u16()?, c.u(4)?, c.u(4)?);
            // Tag 1: an empty descriptor.
            if t != 1 {
                out.insert((t, r), (o, l));
            }
        }
        off = next;
    }
    Ok(out)
}

/// Open an HDF4 file: the SDS with 2 or more dimensions are the variables, the others are small variables.
/// `idx` is the index of `src` in the dataset sources.
pub fn open(src: &Source, idx: u32, name: String) -> Result<Product> {
    let f = File { src, dds: dds(src)? };
    let mut refs: Vec<u16> = f.dds.keys().filter(|k| k.0 == VG).map(|k| k.1).collect();
    refs.sort();
    let vgs: HashMap<u16, Vgroup> = refs.iter().filter_map(|&r| Some((r, f.vgroup(r).ok()?))).collect();
    refs.retain(|r| vgs.contains_key(r));
    let global: Vec<(String, Attr)> = refs.iter().filter(|r| vgs[r].2 == "CDF0.0").flat_map(|r| f.attrs(&vgs[r].0)).collect();
    let (mut vars, mut small, mut metas) = (vec![], vec![], vec![]);
    for r in refs.iter().filter(|r| vgs[r].2 == "Var0.0") {
        let (els, vname, _) = &vgs[r];
        // The elements of the variable, and of its numeric data group (NDG).
        let mut els = els.clone();
        for e in els.clone().iter().filter(|e| e.0 == NDG) {
            let b = f.read(NDG, e.1).unwrap_or_default();
            els.extend(b.as_chunks::<4>().0.iter().map(|x| (u16::from_be_bytes([x[0], x[1]]), u16::from_be_bytes([x[2], x[3]]))));
        }
        let get = |t: u16| els.iter().find(|e| e.0 == t).map(|e| e.1);
        let Some(sdd) = get(SDD).and_then(|r| f.read(SDD, r).ok()) else { continue };
        let mut c = Cur { b: &sdd, p: 0 };
        let Ok(shape) = c.u(2).and_then(|n| (0..n).map(|_| c.u(4)).collect::<Result<Vec<u64>>>()) else { continue };
        // Number type: version, type, width in bits, class (4: little-endian).
        let ntb = get(NT).and_then(|r| f.read(NT, r).ok()).unwrap_or_default();
        let (dtype, le) = (ntb.get(1).and_then(|&t| nt(t)), ntb.get(3) == Some(&4));
        let dim = |e: &(u16, u16)| vgs.get(&e.1).filter(|g| e.0 == VG && (g.2 == "Dim0.0" || g.2 == "UDim0.0")).map(|g| g.1.clone());
        let mut names: Vec<String> = els.iter().filter_map(dim).collect();
        if names.len() != shape.len() {
            names = (0..shape.len()).map(|k| format!("dim_{k}")).collect();
        }
        let attrs = f.attrs(&els);
        let num = |k: &str| match &attrs.iter().find(|a| a.0 == k)?.1 {
            Attr::Num(v) => v.first().copied(),
            Attr::Text(t) => t.parse().ok(),
        };
        metas.push(Meta {
            name: vname.clone(),
            dims: names.iter().cloned().zip(shape.iter().copied()).collect(),
            dtype: dtype.map_or("text".into(), |t| format!("{t:?}").to_lowercase()),
            attrs: attrs.iter().map(|(k, v)| (k.clone(), v.text())).collect(),
        });
        let Some(dtype) = dtype else { continue };
        if shape.contains(&0) {
            continue;
        }
        let n = shape.len();
        // The last two dimensions are the proposal of image axes. A file dimension with the name of an axis is not an axis.
        let dims = names.iter().enumerate().map(|(k, d)| match () {
            _ if n < 2 => d.clone(),
            _ if k == n - 2 => "y".into(),
            _ if k == n - 1 => "x".into(),
            _ if ["y", "x", "time", "band"].contains(&d.as_str()) => format!("{d}_"),
            _ => d.clone(),
        });
        let a = match f.array(get(SD), &shape, dims.collect(), dtype, le, idx) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{}: {vname}: {e}", redact(src.name()));
                continue;
            }
        };
        let (scale, offset) = (num("scale_factor").unwrap_or(1.0), num("add_offset").unwrap_or(0.0));
        // SDsetcal writes `calibrated_nt`: the HDF4 convention, value = scale * (stored - offset). Else CF.
        let offset = if num("calibrated_nt").is_some() { -scale * offset } else { offset };
        let units = match attrs.iter().find(|a| a.0 == "units") {
            Some((_, Attr::Text(t))) => t.clone(),
            _ => String::new(),
        };
        let v = Variable {
            name: vname.clone(),
            group: String::new(),
            levels: vec![a],
            bands: vec![vname.clone()],
            fill: num("_FillValue"),
            scale,
            offset,
            units,
            georef: Georef::None,
            times: Default::default(),
            dims: names,
        };
        if n >= 2 { vars.push(v) } else { small.push(v) }
    }
    if vars.is_empty() {
        return Err(format!("{name}: no HDF4 scientific data set with 2 or more dimensions").into());
    }
    // HDF-EOS 2 metadata: StructMetadata.0, StructMetadata.1 and so on, in this order.
    let mut meta: Vec<&(String, Attr)> = global.iter().filter(|a| a.0.starts_with("StructMetadata.")).collect();
    meta.sort_by_key(|a| a.0.trim_start_matches("StructMetadata.").parse::<u32>().unwrap_or(0));
    let grids = eos_grids(&meta.iter().map(|a| a.1.text()).collect::<String>());
    let all = vars.clone();
    for v in &mut vars {
        v.georef = georef(src, v, &small, &all, &grids);
    }
    let mut dims: Vec<(String, u64)> = vec![];
    for d in metas.iter().flat_map(|m| &m.dims) {
        if !dims.iter().any(|x| x.0 == d.0) {
            dims.push(d.clone());
        }
    }
    let desc = format!("{}, {} variables", if meta.is_empty() { "HDF4" } else { "HDF-EOS 2 / HDF4" }, vars.len());
    let info = Info { attrs: global.iter().map(|(k, v)| (k.clone(), v.text())).collect(), dims, vars: metas };
    Ok(Product { name, desc, vars, small, valid: None, info })
}

/// HDF-EOS 2 grids with the geographic projection (GCTP_GEO), from the StructMetadata text: (name, columns,
/// rows, geotransform). The corners are in the packed degrees of GCTP (DDDMMMSSS.SS).
fn eos_grids(meta: &str) -> Vec<(String, u64, u64, [f64; 6])> {
    let val = |s: &str, k: &str| Some(s.split(k).nth(1)?.lines().next()?.trim().trim_matches('"').to_string());
    let pair = |s: &str, k: &str| -> Option<(f64, f64)> {
        let v = val(s, k)?;
        let (a, b) = v.trim_matches(['(', ')']).split_once(',')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    };
    let deg = |v: f64| {
        let a = v.abs();
        let d = (a / 1e6).floor();
        let m = ((a - d * 1e6) / 1e3).floor();
        v.signum() * (d + m / 60.0 + (a - d * 1e6 - m * 1e3) / 3600.0)
    };
    let grid = |s: &str| {
        if val(s, "Projection=")? != "GCTP_GEO" {
            return None;
        }
        let (w, h): (u64, u64) = (val(s, "XDim=")?.parse().ok()?, val(s, "YDim=")?.parse().ok()?);
        let (ul, lr) = (pair(s, "UpperLeftPointMtrs=")?, pair(s, "LowerRightMtrs=")?);
        let (x0, y0) = (deg(ul.0), deg(ul.1));
        let gt = [x0, (deg(lr.0) - x0) / w as f64, 0.0, y0, 0.0, (deg(lr.1) - y0) / h as f64];
        Some((s.lines().next()?.trim().trim_matches('"').to_string(), w, h, gt))
    };
    meta.split("GridName=").skip(1).filter_map(grid).collect()
}

/// Georeferencing: an HDF-EOS 2 grid (the dimensions `YDim:<grid>` and `XDim:<grid>`), 1D latitude and
/// longitude (a regular grid), or 2D latitude and longitude of the same shape (geolocation arrays).
fn georef(src: &Source, v: &Variable, small: &[Variable], all: &[Variable], grids: &[(String, u64, u64, [f64; 6])]) -> Georef {
    let (w, h) = v.size();
    let n = v.dims.len();
    if let Some(g) = grids.iter().find(|g| (g.1, g.2) == (w, h) && v.dims[n - 2] == format!("YDim:{}", g.0) && v.dims[n - 1] == format!("XDim:{}", g.0)) {
        return Georef::Affine { gt: g.3, crs: Crs::wgs84() };
    }
    let is = |n: &str, k: &str| {
        let n = n.to_lowercase();
        n == k || n == k[..3] || n.starts_with(&format!("{k}_"))
    };
    let lat = small.iter().find(|s| is(&s.name, "latitude") && s.levels[0].shape == [h]);
    let lon = small.iter().find(|s| is(&s.name, "longitude") && s.levels[0].shape == [w]);
    if let (Some(la), Some(lo)) = (lat, lon)
        && let (Ok(la), Ok(lo)) = (read_1d(src, la), read_1d(src, lo))
        && la.len() > 1
        && lo.len() > 1
    {
        let (dx, dy) = (lo[1] - lo[0], la[1] - la[0]);
        return Georef::Affine { gt: [lo[0] - dx / 2.0, dx, 0.0, la[0] - dy / 2.0, 0.0, dy], crs: Crs::wgs84() };
    }
    netcdf::georef(src, v, &[], all)
}

/// Values of a small 1D variable, with scale and offset (coordinates).
fn read_1d(src: &Source, v: &Variable) -> Result<Vec<f64>> {
    let a = &v.levels[0];
    // The length comes from the file: a coordinate has less than 50 million values.
    if a.shape[0] > 50_000_000 {
        return Err(format!("{}: {} values, more than the limit of 50 million", v.name, a.shape[0]).into());
    }
    let mut out = vec![];
    for c in a.chunks.iter() {
        if c.len == 0 {
            out.extend(std::iter::repeat_n(f64::NAN, a.chunk[0].min(a.shape[0]) as usize));
        } else {
            out.extend(codec::to_f64(a, &codec::decode(a, &src.read(c.off..c.off + c.len)?)?));
        }
    }
    out.truncate(a.shape[0] as usize);
    Ok(out.into_iter().map(|x| x * v.scale + v.offset).collect())
}
