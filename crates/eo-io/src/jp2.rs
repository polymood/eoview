//! JPEG 2000 (JP2 file or raw codestream). Each tile is a chunk. Each resolution level of the
//! codestream is an overview: level r decodes the tiles with `reduce` r.
//!
//! Specification: ITU-T T.800 | ISO/IEC 15444-1, <https://www.itu.int/rec/T-REC-T.800>.
//! Decoder: OpenJPEG 2.5 (C library, through the jpeg2k crate). It is exact (same values as GDAL)
//! and 1.3 times faster than hayro-jpeg2000 0.4 at full resolution. hayro-jpeg2000 fails at reduced resolution.
use crate::source::Source;
use eo_core::*;
use std::sync::Arc;

pub fn is_jp2(head: &[u8]) -> bool {
    head.starts_with(&[0, 0, 0, 12, b'j', b'P', b' ', b' ']) || head.starts_with(&[0xFF, 0x4F, 0xFF, 0x51])
}

fn be16(b: &[u8], p: usize) -> Result<usize> {
    Ok(u16::from_be_bytes(b.get(p..p + 2).ok_or("truncated JPEG 2000 header")?.try_into().unwrap()) as usize)
}

fn be32(b: &[u8], p: usize) -> Result<u64> {
    Ok(u32::from_be_bytes(b.get(p..p + 4).ok_or("truncated JPEG 2000 header")?.try_into().unwrap()) as u64)
}

/// Main header values that the reader uses.
struct Siz {
    xs: u64,
    ys: u64,
    xo: u64,
    yo: u64,
    tw: u64,
    th: u64,
    xto: u64,
    yto: u64,
    comps: usize,
    ssiz: u8,
}

fn siz(h: &[u8]) -> Result<(usize, Siz)> {
    let mut p = 2;
    while p + 4 <= h.len() {
        let (m, l) = (be16(h, p)?, be16(h, p + 2)?);
        if m == 0xFF51 {
            let v = |o| be32(h, p + o);
            let s = Siz {
                xs: v(6)?,
                ys: v(10)?,
                xo: v(14)?,
                yo: v(18)?,
                tw: v(22)?,
                th: v(26)?,
                xto: v(30)?,
                yto: v(34)?,
                comps: be16(h, p + 38)?,
                ssiz: *h.get(p + 40).ok_or("truncated SIZ")?,
            };
            return Ok((p, s));
        }
        p += 2 + l;
    }
    Err("JPEG 2000 without SIZ marker".into())
}

/// Open a JPEG 2000 source: the levels of one variable. `idx` is the index of `src` in the dataset sources.
pub fn arrays(src: &Source, idx: u32) -> Result<Vec<Array>> {
    arrays_at(src, idx, None)
}

/// As `arrays`, with the position of a raw codestream in the source (for example in a NITF file).
/// None: a JP2 file (the codestream is in the jp2c box) or a raw codestream at the start.
pub fn arrays_at(src: &Source, idx: u32, at: Option<u64>) -> Result<Vec<Array>> {
    let len = src.len()?;
    // Codestream position: the contents of the jp2c box, or the whole source.
    let mut cs = at.unwrap_or(0);
    if at.is_none() && src.read(0..12.min(len))?.starts_with(&[0, 0, 0, 12]) {
        let mut p = 0;
        loop {
            let b = src.read(p..(p + 16).min(len))?;
            let (mut bl, ty, mut hl) = (be32(&b, 0)?, &b[4..8], 8);
            if bl == 1 {
                bl = u64::from_be_bytes(b.get(8..16).ok_or("truncated JP2 box")?.try_into().unwrap());
                hl = 16;
            } else if bl == 0 {
                bl = len - p;
            }
            if ty == b"jp2c" {
                cs = p + hl;
                break;
            }
            if bl < 8 || p + bl >= len {
                return Err("JP2 file without codestream".into());
            }
            p += bl;
        }
    }
    // Main header: from SOC to the first SOT.
    let mut head = src.read(cs..(cs + 65536).min(len))?;
    let mut p = 2;
    let mut tlm = vec![];
    loop {
        if p + 4 > head.len() {
            head = src.read(cs..(cs + 2 * head.len() as u64).min(len))?;
            if p + 4 > head.len() {
                return Err("JPEG 2000 main header is after end of file".into());
            }
            continue;
        }
        let (m, l) = (be16(&head, p)?, be16(&head, p + 2)?);
        if m == 0xFF90 {
            break;
        }
        if m == 0xFF60 {
            return Err("JPEG 2000 with packed packet headers (PPM) is not supported".into());
        }
        if p + 2 + l > head.len() {
            head = src.read(cs..(cs + 2 * head.len() as u64).min(len))?;
            continue;
        }
        if m == 0xFF55 {
            tlm.push(head.slice(p + 4..p + 2 + l));
        }
        p += 2 + l;
    }
    let header: Arc<[u8]> = Arc::from(&head[..p]);
    let (sp, s) = siz(&header)?;
    let _ = sp;
    if s.xto != 0 || s.yto != 0 || s.xo != 0 || s.yo != 0 {
        return Err("JPEG 2000 with an image or tile offset is not supported".into());
    }
    let levels = cod_levels(&header)?;
    let (nx, ny) = (s.xs.div_ceil(s.tw), s.ys.div_ceil(s.th));
    let ntiles = (nx * ny) as usize;

    // Tile parts: the lengths come from the TLM markers, or from the SOT markers.
    let mut parts: Vec<(usize, u64, u64)> = vec![];
    let mut off = cs + p as u64;
    if !tlm.is_empty() {
        let mut k = 0;
        for t in &tlm {
            let stlm = *t.get(1).ok_or("truncated TLM")?;
            let (st, sp4) = (((stlm >> 4) & 3) as usize, (stlm >> 6) & 1 == 1);
            let psz = if sp4 { 4 } else { 2 };
            let mut q = 2;
            while q + st + psz <= t.len() {
                let tile = match st {
                    0 => k,
                    1 => t[q] as usize,
                    _ => be16(t, q)?,
                };
                let plen = if sp4 { be32(t, q + st)? } else { be16(t, q + st)? as u64 };
                parts.push((tile, off, plen));
                off += plen;
                q += st + psz;
                k += 1;
            }
        }
    } else {
        while off + 12 <= len {
            let b = src.read(off..off + 12)?;
            if be16(&b, 0)? != 0xFF90 {
                break;
            }
            let plen = be32(&b, 6)?;
            if plen == 0 {
                return Err("JPEG 2000 tile part without length is not supported".into());
            }
            parts.push((be16(&b, 4)?, off, plen));
            off += plen;
        }
    }
    let mut chunks = vec![ChunkLoc { src: idx, off: 0, len: 0 }; ntiles];
    for (t, o, l) in parts {
        let c = chunks.get_mut(t).ok_or("JPEG 2000 tile index out of range")?;
        if c.len == 0 {
            (c.off, c.len) = (o, l);
        } else if c.off + c.len == o {
            c.len += l;
        } else {
            return Err("JPEG 2000 tile parts that are not adjacent are not supported".into());
        }
    }
    let dtype = match (s.ssiz & 0x80 != 0, (s.ssiz & 0x7F) + 1) {
        (false, 1..=8) => DType::U8,
        (true, 1..=8) => DType::I8,
        (false, 9..=16) => DType::U16,
        (true, 9..=16) => DType::I16,
        (false, _) => DType::U32,
        (true, _) => DType::I32,
    };
    Ok((0..=levels)
        .map(|r| {
            let d = 1u64 << r;
            let (h, w, ch, cw) = (s.ys.div_ceil(d), s.xs.div_ceil(d), s.th.div_ceil(d), s.tw.div_ceil(d));
            let (dims, shape, chunk) = if s.comps > 1 {
                (vec!["y", "x", "band"], vec![h, w, s.comps as u64], vec![ch, cw, s.comps as u64])
            } else {
                (vec!["y", "x"], vec![h, w], vec![ch, cw])
            };
            Array {
                dims: dims.into_iter().map(String::from).collect(),
                shape,
                chunk,
                dtype,
                le: true,
                codecs: vec![Codec::Jpeg2000 { reduce: r as u8, header: header.clone() }],
                chunks: chunks.clone(),
                // Resolution level r has exactly 2^r level-0 pixels for each pixel.
                place: Some([d as f64, d as f64, 0.0, 0.0]),
            }
        })
        .collect())
}

/// Number of decomposition levels (COD marker).
fn cod_levels(h: &[u8]) -> Result<usize> {
    let mut p = 2;
    while p + 4 <= h.len() {
        let (m, l) = (be16(h, p)?, be16(h, p + 2)?);
        if m == 0xFF52 {
            return Ok(*h.get(p + 9).ok_or("truncated COD")? as usize);
        }
        p += 2 + l;
    }
    Err("JPEG 2000 without COD marker".into())
}

/// Decode one tile (its tile parts in `src`) into a chunk of `a`.
pub fn decode_tile(a: &Array, src: &[u8], reduce: u8, header: &[u8]) -> Result<Vec<u8>> {
    let (sp, s) = siz(header)?;
    if be16(src, 0)? != 0xFF90 {
        return Err("JPEG 2000 chunk does not start with a tile part".into());
    }
    let tile = be16(src, 4)? as u64;
    let nx = s.xs.div_ceil(s.tw);
    let (tx, ty) = (tile % nx, tile / nx);
    let (x0, y0) = (tx * s.tw, ty * s.th);
    let (x1, y1) = (((tx + 1) * s.tw).min(s.xs), ((ty + 1) * s.th).min(s.ys));
    // A codestream with this tile only: SIZ of the tile (same absolute positions), no TLM or PLM, tile index 0.
    let mut cs = Vec::with_capacity(header.len() + src.len() + 2);
    let mut p = 2;
    cs.extend_from_slice(&header[..2]);
    while p + 4 <= header.len() {
        let (m, l) = (be16(header, p)?, be16(header, p + 2)?);
        if p == sp {
            let mut z = header[p..p + 2 + l].to_vec();
            for (o, v) in [(6, x1), (10, y1), (14, x0), (18, y0), (30, x0), (34, y0)] {
                z[o..o + 4].copy_from_slice(&(v as u32).to_be_bytes());
            }
            cs.extend(z);
        } else if m != 0xFF55 && m != 0xFF57 {
            cs.extend_from_slice(&header[p..p + 2 + l]);
        }
        p += 2 + l;
    }
    let mut q = 0;
    while q + 12 <= src.len() && be16(src, q)? == 0xFF90 {
        let plen = be32(src, q + 6)? as usize;
        let end = (q + plen).min(src.len());
        let t0 = cs.len();
        cs.extend_from_slice(&src[q..end]);
        cs[t0 + 4..t0 + 6].copy_from_slice(&[0, 0]);
        q = end;
    }
    cs.extend([0xFF, 0xD9]);
    let img = jpeg2k::Image::from_bytes_with(&cs, jpeg2k::DecodeParameters::new().reduce(reduce as u32))
        .map_err(|e| Error(format!("JPEG 2000 tile {tile}: {e}")))?;
    let comps = img.components();
    let (y, x) = (a.axis("y").unwrap(), a.axis("x").unwrap());
    let (ch, cw, nc) = (a.chunk[y] as usize, a.chunk[x] as usize, comps.len());
    let es = a.dtype.size();
    let mut out = vec![0u8; a.chunk_bytes()];
    for (k, c) in comps.iter().enumerate() {
        let (w, h) = ((c.width() as usize).min(cw), (c.height() as usize).min(ch));
        let data = c.data();
        for r in 0..h {
            for col in 0..w {
                let v = data[r * c.width() as usize + col];
                let o = ((r * cw + col) * nc + k) * es;
                match es {
                    1 => out[o] = v as u8,
                    2 => out[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes()),
                    _ => out[o..o + 4].copy_from_slice(&(v as u32).to_le_bytes()),
                }
            }
        }
    }
    Ok(out)
}
