//! NITF 2.1 and NSIF 1.0 (SAR products: SICD complex images, SIDD detected images). The image blocks are the
//! chunks. The segment parsing comes from radview.
//!
//! Specifications:
//! - NITF 2.1: MIL-STD-2500C, <https://nsgreg.nga.mil/doc/view?i=4122>
//! - SICD and SIDD: NGA.STND.0024 and NGA.STND.0025, <https://nsgreg.nga.mil/NSGDOC/>
//!
//! Georeferencing: the plane projection of the SIDD XML, else the image corners of the SICD or SIDD XML,
//! else the IGEOLO corners of the image subheader (as GDAL: at the centers of the corner pixels).
use crate::source::Source;
use crate::xml::{elems, text};
use eo_core::*;

pub fn is_nitf(head: &[u8]) -> bool {
    head.starts_with(b"NITF02.10") || head.starts_with(b"NSIF01.00")
}

fn trim(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap_or("").trim()
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl Cur<'_> {
    fn str(&mut self, n: usize) -> Result<&str> {
        let s = self.b.get(self.p..self.p + n).ok_or("truncated NITF header")?;
        self.p += n;
        Ok(trim(s))
    }
    fn num(&mut self, n: usize) -> Result<u64> {
        let at = self.p;
        let s = self.str(n)?;
        s.parse().map_err(|_| Error(format!("bad NITF number {s:?} at byte {at}")))
    }
    fn skip(&mut self, n: usize) {
        self.p += n;
    }
}

/// One image segment subheader.
struct Seg {
    rows: u64,
    cols: u64,
    pvtype: String,
    /// Image representation: MONO, RGB, MULTI, NODISPLY, ...
    irep: String,
    icat: String,
    icords: String,
    igeolo: String,
    nbpp: u64,
    nbands: u64,
    iq: bool,
    imode: String,
    ic: String,
    bpr: u64,
    bpc: u64,
    bw: u64,
    bh: u64,
    idlvl: u64,
    ialvl: u64,
    iloc_row: u64,
    data: u64,
}

fn segment(src: &Source, sub: u64, lish: u64) -> Result<Seg> {
    let b = src.read(sub..sub + lish)?;
    let mut c = Cur { b: &b, p: 333 };
    let (rows, cols) = (c.num(8)?, c.num(8)?);
    let pvtype = c.str(3)?.to_string();
    let irep = c.str(8)?.trim().to_string();
    let icat = c.str(8)?.to_string();
    c.skip(2 + 1); // ABPP PJUST
    let icords = c.str(1)?.to_string();
    let igeolo = if icords.is_empty() { String::new() } else { c.str(60)?.to_string() };
    let nicom = c.num(1)?;
    c.skip(80 * nicom as usize);
    let ic = c.str(2)?.to_string();
    if ic != "NC" && ic != "NM" {
        c.skip(4); // COMRAT
    }
    let mut nbands = c.num(1)?;
    if nbands == 0 {
        nbands = c.num(5)?;
    }
    let mut subcat = Vec::new();
    for _ in 0..nbands {
        c.skip(2);
        subcat.push(c.str(6)?.to_string());
        c.skip(4);
        let nluts = c.num(1)?;
        if nluts > 0 {
            let nelut = c.num(5)?;
            c.skip((nluts * nelut) as usize);
        }
    }
    c.skip(1); // ISYNC
    let imode = c.str(1)?.to_string();
    let (bpr, bpc, bw, bh, nbpp) = (c.num(4)?, c.num(4)?, c.num(4)?, c.num(4)?, c.num(2)?);
    let (idlvl, ialvl) = (c.num(3)?, c.num(3)?);
    let iloc_row = c.num(5)?;
    let iq = nbands == 2 && subcat[0] == "I" && subcat[1] == "Q";
    let (bw, bh) = (if bw == 0 { cols } else { bw }, if bh == 0 { rows } else { bh });
    Ok(Seg { rows, cols, pvtype, irep, icat, icords, igeolo, nbpp, nbands, iq, imode, ic, bpr, bpc, bw, bh, idlvl, ialvl, iloc_row, data: sub + lish })
}

/// Chunk locations of an uncompressed (NC) or masked (NM) segment. `step`: bytes of one block (all
/// bands of the block for IMODE B and P). `first`: index of the first block of the selected band (IMODE S).
fn blocks(src: &Source, idx: u32, g: &Seg, step: u64, nblocks: u64, out: &mut Vec<ChunkLoc>) -> Result<()> {
    if g.ic == "NC" {
        out.extend((0..nblocks).map(|i| ChunkLoc { src: idx, off: g.data + i * step, len: step }));
        return Ok(());
    }
    // NM: block mask table. Offset 0xFFFFFFFF: the block is not in the file (fill value).
    let h = src.read(g.data..g.data + 10)?;
    let be32 = |b: &[u8], p: usize| u32::from_be_bytes(b[p..p + 4].try_into().unwrap()) as u64;
    let imdatoff = be32(&h, 0);
    let bmrlnth = u16::from_be_bytes([h[4], h[5]]) as u64;
    let tpxcdlnth = u16::from_be_bytes([h[8], h[9]]) as u64;
    let bmr = g.data + 10 + tpxcdlnth.div_ceil(8);
    let table = if bmrlnth == 4 { src.read(bmr..bmr + 4 * nblocks)? } else { bytes::Bytes::new() };
    for i in 0..nblocks {
        let o = if bmrlnth == 4 { be32(&table, 4 * i as usize) } else { i * step };
        out.push(if o == 0xFFFF_FFFF { ChunkLoc { src: idx, off: 0, len: 0 } } else { ChunkLoc { src: idx, off: g.data + imdatoff + o, len: step } });
    }
    Ok(())
}

/// Open a NITF source. `idx` is the index of `src` in the dataset sources.
pub fn open(src: &Source, idx: u32) -> Result<Product> {
    let len = src.len()?;
    let head = src.read(0..len.min(4096))?;
    let mut c = Cur { b: &head, p: 354 };
    let (hl, numi) = (c.num(6)?, c.num(3)?);
    if numi == 0 {
        return Err("NITF without image segment".into());
    }
    let mut imgs = vec![];
    for _ in 0..numi {
        imgs.push((c.num(6)?, c.num(10)?));
    }
    let skip = |c: &mut Cur, a: usize, b: usize| -> Result<u64> {
        let n = c.num(3)?;
        let mut t = 0;
        for _ in 0..n {
            t += c.num(a)? + c.num(b)?;
        }
        Ok(t)
    };
    let graphics = skip(&mut c, 4, 6)?;
    c.skip(3); // NUMX
    let texts = skip(&mut c, 4, 5)?;
    let numdes = c.num(3)?;
    let mut des = vec![];
    for _ in 0..numdes {
        des.push((c.num(4)?, c.num(9)?));
    }
    let mut segs = vec![];
    let mut off = hl;
    for &(lish, li) in &imgs {
        segs.push(segment(src, off, lish));
        off += lish + li;
    }
    // XML of the SICD or SIDD metadata (data extension segments).
    let mut off = off + graphics + texts;
    let mut xml = String::new();
    for (ldsh, ld) in des {
        let d = src.read(off + ldsh..(off + ldsh + ld).min(len))?;
        let s = String::from_utf8_lossy(&d);
        if s.contains("<SIDD") || s.contains("<SICD") {
            xml = s.into_owned();
            break;
        }
        off += ldsh + ld;
    }

    let s0 = segs.remove(0)?;
    // Large images (SICD, SIDD > 10 GB) continue in the next segments. They join the first one if they are
    // below it, with the same layout, and if the chunk grid stays regular (all full blocks but the last).
    let mut used = vec![s0];
    for s in segs {
        let Ok(s) = s else { break };
        let (p, f) = (used.last().unwrap(), &used[0]);
        let same = s.cols == f.cols && s.pvtype == f.pvtype && s.nbpp == f.nbpp && s.nbands == f.nbands && s.ic == f.ic;
        let same = same && s.imode == f.imode && s.bw == f.bw && s.bh == f.bh && s.bpr == f.bpr;
        let rows: u64 = used.iter().map(|u| u.rows).sum();
        let below = (s.ialvl == p.idlvl && s.iloc_row == p.rows) || (s.ialvl == f.idlvl && s.iloc_row == rows);
        if !(same && below && p.rows == p.bpc * p.bh) {
            break;
        }
        used.push(s);
    }
    let f = &used[0];
    let (pv, nb) = (f.pvtype.as_str(), f.nbands);
    let comps = if pv == "C" { 2 } else { 1 };
    let complex = pv == "C" || (f.iq && f.imode == "P");
    let part_bits = f.nbpp / comps;
    let part = match (pv, part_bits) {
        ("INT", 8) => DType::U8,
        ("INT", 16) => DType::U16,
        ("INT", 32) => DType::U32,
        ("SI", 8) => DType::I8,
        ("SI", 16) => DType::I16,
        ("SI", 32) => DType::I32,
        ("R" | "C", 32) => DType::F32,
        ("R", 64) => DType::F64,
        _ => return Err(format!("NITF pixel type {pv} with {} bits is not supported", f.nbpp).into()),
    };
    let dtype = match (complex, part) {
        (false, t) => t,
        (true, DType::I16) => DType::CI16,
        (true, DType::I32) => DType::CI32,
        (true, DType::F32) => DType::CF32,
        (true, DType::F64) => DType::CF64,
        (true, t) => return Err(format!("NITF complex {t:?} is not supported").into()),
    };
    let rows: u64 = used.iter().map(|u| u.rows).sum();
    let (bw, bh) = (f.bw, f.bh);
    let es = dtype.size() as u64;

    let levels = if f.ic == "C8" || f.ic == "M8" {
        if used.len() > 1 || f.ic == "M8" {
            return Err("NITF JPEG 2000 with more than one segment or with a block mask is not supported".into());
        }
        crate::jp2::arrays_at(src, idx, Some(f.data))?
    } else if f.ic == "NC" || f.ic == "NM" {
        // Chunk layout of a block by IMODE. Complex data (C, or I and Q in IMODE P) is one band of complex values.
        let nbands = if complex { 1 } else { nb };
        let (dims, shape, chunk) = match f.imode.as_str() {
            _ if nbands == 1 => (vec!["y", "x"], vec![rows, f.cols], vec![bh, bw]),
            "P" => (vec!["y", "x", "band"], vec![rows, f.cols, nbands], vec![bh, bw, nbands]),
            "B" => (vec!["band", "y", "x"], vec![nbands, rows, f.cols], vec![nbands, bh, bw]),
            "S" => (vec!["band", "y", "x"], vec![nbands, rows, f.cols], vec![1, bh, bw]),
            m => return Err(format!("NITF IMODE {m} with {nb} bands is not supported").into()),
        };
        let mut chunks = vec![];
        if f.imode == "S" && nbands > 1 {
            if f.ic != "NC" {
                return Err("NITF IMODE S with a block mask is not supported".into());
            }
            // All blocks of band 0, then of band 1, ...: band-major, as the chunk grid [band, y, x].
            let step = bw * bh * es;
            for b in 0..nbands {
                for g in &used {
                    let nblk = g.bpr * g.bpc;
                    chunks.extend((0..nblk).map(|i| ChunkLoc { src: idx, off: g.data + (b * nblk + i) * step, len: step }));
                }
            }
        } else {
            let step = bw * bh * es * if complex { 1 } else { nb };
            for g in &used {
                blocks(src, idx, g, step, g.bpr * g.bpc, &mut chunks)?;
            }
        }
        for c in &chunks {
            if c.len > 0 && c.off + c.len > len {
                return Err(format!("NITF block at {} (+{}) is after end of file", c.off, c.len).into());
            }
        }
        let a = Array { dims: dims.into_iter().map(String::from).collect(), shape, chunk, dtype, le: false, codecs: vec![], chunks: chunks.into(), place: None };
        a.validate()?;
        vec![a]
    } else {
        return Err(format!("NITF compression {} is not supported (NC, NM, C8)", f.ic).into());
    };

    let georef = georef(&xml, f, levels[0].len_of("x"), rows);
    let kind = if xml.contains("<SIDD") { "SIDD" } else if xml.contains("<SICD") { "SICD" } else { "NITF" };
    let nbv = levels[0].len_of("band") as usize;
    let name = src.name().rsplit(['/', '\\']).next().unwrap_or("").to_string();
    let desc = format!(
        "{kind} {:?}, {} x {rows}, {nbv} band(s), {}{}",
        dtype,
        f.cols,
        f.ic,
        if used.len() > 1 { format!(", {} segments", used.len()) } else { String::new() }
    );
    let var = Variable {
        times: Default::default(),
        name: kind.into(),
        group: String::new(),
        bands: match nbv {
            1 => vec![kind.into()],
            // IREP RGB: the three bands are colors.
            3 if f.irep == "RGB" => ["red", "green", "blue"].map(String::from).to_vec(),
            _ => (1..=nbv).map(|b| format!("Band {b}")).collect(),
        },
        levels,
        // SAR products: value 0 is outside the image.
        fill: (f.icat == "SAR").then_some(0.0),
        scale: 1.0,
        offset: 0.0,
        units: String::new(),
        georef,
    };
    Ok(Product { name, desc, vars: vec![var] })
}

/// Georeferencing: SIDD plane projection, else SICD/SIDD image corners, else IGEOLO. Grid positions are
/// pixel centers.
fn georef(xml: &str, s: &Seg, w: u64, h: u64) -> Georef {
    let num = |x: &str, k: &str| text(x, k).and_then(|t| t.parse::<f64>().ok());
    let xyz = |x: &str| Some([num(x, "X")?, num(x, "Y")?, num(x, "Z")?]);
    let (cw, ch) = (w as f64, h as f64);
    // SIDD: pixel (row, col) = reference point + (row - r0) * dr * row vector + (col - c0) * dc * col vector (ECEF).
    if let Some(pp) = elems(xml, "PlaneProjection").first().map(|e| e.1) {
        let rp = elems(pp, "ReferencePoint").first().map(|e| e.1).unwrap_or("");
        let ss = elems(pp, "SampleSpacing").first().map(|e| e.1).unwrap_or("");
        let v = (
            elems(rp, "ECEF").first().and_then(|e| xyz(e.1)),
            num(rp, "Row").zip(num(rp, "Col")),
            num(ss, "Row").zip(num(ss, "Col")),
            elems(pp, "RowUnitVector").first().and_then(|e| xyz(e.1)),
            elems(pp, "ColUnitVector").first().and_then(|e| xyz(e.1)),
        );
        if let (Some(p0), Some((r0, c0)), Some((dr, dc)), Some(ru), Some(cu)) = v {
            let n = 17;
            let cols: Vec<f64> = (0..n).map(|i| 0.5 + (cw - 1.0) * i as f64 / (n - 1) as f64).collect();
            let rows: Vec<f64> = (0..n).map(|j| 0.5 + (ch - 1.0) * j as f64 / (n - 1) as f64).collect();
            let (mut lon, mut lat) = (vec![], vec![]);
            for &r in &rows {
                for &c in &cols {
                    let (a, b) = ((r - 0.5 - r0) * dr, (c - 0.5 - c0) * dc);
                    let e: [f64; 3] = std::array::from_fn(|k| p0[k] + a * ru[k] + b * cu[k]);
                    let (lo, la) = geo::ecef_to_lonlat(e);
                    lon.push(lo);
                    lat.push(la);
                }
            }
            return Georef::Grid { cols, rows, lon, lat };
        }
    }
    // Image corners: first row first column, first row last column, last row last column, last row first column.
    let icp: Vec<(f64, f64)> = elems(xml, "ICP").iter().filter_map(|e| Some((num(e.1, "Lon")?, num(e.1, "Lat")?))).collect();
    let corners = if icp.len() == 4 { Some(icp) } else { igeolo(&s.icords, &s.igeolo) };
    match corners {
        Some(c) => Georef::Grid {
            cols: vec![0.5, cw - 0.5],
            rows: vec![0.5, ch - 0.5],
            lon: vec![c[0].0, c[1].0, c[3].0, c[2].0],
            lat: vec![c[0].1, c[1].1, c[3].1, c[2].1],
        },
        None => Georef::None,
    }
}

/// IGEOLO corners (lon, lat) in the order UL, UR, LR, LL. ICORDS G (ddmmssXdddmmssY) and D (+-dd.ddd+-ddd.ddd).
fn igeolo(icords: &str, g: &str) -> Option<Vec<(f64, f64)>> {
    if g.len() < 60 {
        return None;
    }
    (0..4)
        .map(|k| {
            let s = &g[15 * k..15 * (k + 1)];
            match icords {
                "G" => {
                    let dms = |t: &str, n: usize| -> Option<f64> {
                        let d: f64 = t[..n].parse().ok()?;
                        let m: f64 = t[n..n + 2].parse().ok()?;
                        let s: f64 = t[n + 2..n + 4].parse().ok()?;
                        let v = d + m / 60.0 + s / 3600.0;
                        Some(if matches!(&t[n + 4..n + 5], "S" | "W") { -v } else { v })
                    };
                    Some((dms(&s[7..], 3)?, dms(&s[..7], 2)?))
                }
                "D" => Some((s[7..].trim().parse().ok()?, s[..7].trim().parse().ok()?)),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn igeolo_dms() {
        let c = super::igeolo("G", "511407N0041916E511647N0041840E511709N0042256E511429N0042331E").unwrap();
        assert!((c[0].1 - (51.0 + 14.0 / 60.0 + 7.0 / 3600.0)).abs() < 1e-12);
        assert!((c[0].0 - (4.0 + 19.0 / 60.0 + 16.0 / 3600.0)).abs() < 1e-12);
        assert_eq!(c.len(), 4);
    }
}
