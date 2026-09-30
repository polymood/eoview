//! TIFF, BigTIFF, GeoTIFF and COG reader. It reads the IFDs, the overviews and the georeferencing.
//! It does not read pixel data: the chunk engine reads the chunks.
//!
//! Specifications:
//! - TIFF 6.0: <https://www.itu.int/itudoc/itu-t/com16/tiff-fx/docs/tiff6.pdf>
//! - BigTIFF: <https://www.awaresystems.be/imaging/tiff/bigtiff.html>
//! - OGC GeoTIFF 1.1: <https://docs.ogc.org/is/19-008r4/19-008r4.html>
//! - OGC Cloud Optimized GeoTIFF: <https://docs.ogc.org/is/21-026/21-026.html>
//! - GDAL private tags (GDAL_METADATA, GDAL_NODATA): <https://gdal.org/en/stable/drivers/raster/gtiff.html>
use crate::source::Source;
use bytes::Bytes;
use eo_core::*;
use std::collections::HashMap;
use std::sync::Mutex;

/// Size of the blocks that the reader gets for the header and the tag values.
const BLOCK: u64 = 64 << 10;
/// Tags that the viewer uses. The reader ignores all other tags.
const USED: &[u16] = &[
    254, 256, 257, 258, 259, 273, 277, 278, 279, 284, 317, 322, 323, 324, 325, 339, 33550, 33922, 34264, 34735, 34736,
    34737, 42112, 42113,
];

pub fn is_tiff(head: &[u8]) -> bool {
    matches!(head.get(..4), Some(b"II*\0" | b"MM\0*" | b"II+\0" | b"MM\0+"))
}

/// Reader for the header. For a remote source, it gets aligned blocks and keeps them.
struct Rd<'a> {
    src: &'a Source,
    le: bool,
    blocks: Mutex<HashMap<u64, Bytes>>,
}

impl Rd<'_> {
    fn bytes(&self, off: u64, n: u64) -> Result<Bytes> {
        let end = off.checked_add(n).filter(|&e| e <= self.src.len()).ok_or("TIFF value is after end of file")?;
        if self.src.is_local() {
            return self.src.read(off..end);
        }
        let (b0, b1) = (off / BLOCK, (end.max(1) - 1) / BLOCK);
        if b0 == b1 {
            let mut m = self.blocks.lock().unwrap();
            if let Some(b) = m.get(&b0) {
                return Ok(b.slice((off - b0 * BLOCK) as usize..(end - b0 * BLOCK) as usize));
            }
            let b = self.src.read(b0 * BLOCK..((b0 + 1) * BLOCK).min(self.src.len()))?;
            let s = b.slice((off - b0 * BLOCK) as usize..(end - b0 * BLOCK) as usize);
            m.insert(b0, b);
            return Ok(s);
        }
        self.src.read(off..end)
    }

    fn uint(&self, b: &[u8]) -> u64 {
        let n = b.len();
        (0..n).fold(0u64, |v, i| v | (b[if self.le { i } else { n - 1 - i }] as u64) << (8 * i))
    }

    fn at(&self, off: u64, n: u64) -> Result<u64> {
        Ok(self.uint(&self.bytes(off, n)?))
    }
}

struct Tag {
    typ: u16,
    raw: Bytes,
}

fn type_size(typ: u16) -> u64 {
    match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 | 16 | 17 | 18 => 8,
        _ => 0,
    }
}

impl Tag {
    fn uints(&self, rd: &Rd) -> Vec<u64> {
        let s = type_size(self.typ) as usize;
        match self.typ {
            1 | 3 | 4 | 16 | 13 | 18 => self.raw.chunks_exact(s).map(|b| rd.uint(b)).collect(),
            _ => vec![],
        }
    }

    fn f64s(&self, rd: &Rd) -> Vec<f64> {
        match self.typ {
            12 => self.raw.chunks_exact(8).map(|b| f64::from_bits(rd.uint(b))).collect(),
            11 => self.raw.chunks_exact(4).map(|b| f32::from_bits(rd.uint(b) as u32) as f64).collect(),
            _ => self.uints(rd).into_iter().map(|v| v as f64).collect(),
        }
    }

    fn ascii(&self) -> String {
        String::from_utf8_lossy(&self.raw).trim_end_matches('\0').trim().to_string()
    }
}

type Ifd = HashMap<u16, Tag>;

fn read_ifd(rd: &Rd, big: bool, at: u64) -> Result<(Ifd, u64)> {
    let (cn, esz, inline) = if big { (8, 20, 8) } else { (2, 12, 4) };
    let n = rd.at(at, cn)?;
    if n > 4096 {
        return Err(format!("TIFF IFD at {at} has {n} entries").into());
    }
    let ents = rd.bytes(at + cn, n * esz + inline)?;
    let mut tags = HashMap::new();
    for e in 0..n as usize {
        let p = &ents[e * esz as usize..][..esz as usize];
        let tag = rd.uint(&p[..2]) as u16;
        if !USED.contains(&tag) {
            continue;
        }
        let typ = rd.uint(&p[2..4]) as u16;
        let (count, vp) = if big { (rd.uint(&p[4..12]), &p[12..20]) } else { (rd.uint(&p[4..8]), &p[8..12]) };
        let len = type_size(typ).checked_mul(count).ok_or("TIFF tag too large")?;
        let raw = if len <= inline {
            Bytes::copy_from_slice(&vp[..len as usize])
        } else {
            rd.bytes(rd.uint(vp), len)?
        };
        tags.insert(tag, Tag { typ, raw });
    }
    let next = rd.uint(&ents[(n * esz) as usize..]);
    Ok((tags, next))
}

/// Open a TIFF source as a product with one variable.
pub fn open(src: &Source) -> Result<Product> {
    let head = src.read(0..16.min(src.len()))?;
    if !is_tiff(&head) {
        return Err("not a TIFF file".into());
    }
    let le = head[0] == b'I';
    let rd = Rd { src, le, blocks: Default::default() };
    let big = rd.uint(&head[2..4]) == 43;
    let mut next = if big { rd.uint(&head[8..16]) } else { rd.uint(&head[4..8]) };
    let mut ifds = Vec::new();
    while next != 0 && ifds.len() < 64 {
        let (t, n) = read_ifd(&rd, big, next)?;
        ifds.push(t);
        next = n;
    }
    let first = ifds.first().ok_or("TIFF without IFD")?;
    let one = |ifd: &Ifd, t: u16, d: u64| ifd.get(&t).and_then(|v| v.uints(&rd).first().copied()).unwrap_or(d);
    // Overviews: reduced-resolution images (bit 0) that are not masks (bit 2), with the same pixel layout.
    let same = |a: &Ifd| [258, 277, 339, 284].iter().all(|&t| one(a, t, 1) == one(first, t, 1));
    let mut levels = vec![array(&rd, first)?];
    for ifd in &ifds[1..] {
        let kind = one(ifd, 254, 0);
        if kind & 1 == 1 && kind & 4 == 0 && same(ifd) {
            match array(&rd, ifd) {
                Ok(a) => levels.push(a),
                Err(e) => eprintln!("{}: overview ignored: {e}", src.name()),
            }
        }
    }
    levels.sort_by_key(|a| std::cmp::Reverse(a.len_of("x")));
    for a in &levels {
        a.validate()?;
        if let Some(c) = a.chunks.iter().find(|c| c.off + c.len > src.len()) {
            return Err(format!("chunk at {} (+{}) is after end of file", c.off, c.len).into());
        }
    }

    let a = &levels[0];
    let nb = a.len_of("band") as usize;
    let md = first.get(&42112).map(|t| gdal_metadata(&t.ascii())).unwrap_or_default();
    let item = |role: &str, b: usize| md.iter().find(|m| m.0 == role && m.1 == b).map(|m| m.2.clone());
    let bands = (0..nb).map(|b| item("description", b).unwrap_or_else(|| format!("Band {}", b + 1))).collect();
    let num = |role| item(role, 0).and_then(|s| s.parse::<f64>().ok());
    let fill = first.get(&42113).and_then(|t| t.ascii().parse::<f64>().ok());
    let name = src.name().rsplit(['/', '\\']).next().unwrap_or("").to_string();
    let (w, h) = (a.len_of("x"), a.len_of("y"));
    let desc = format!(
        "{}TIFF {:?}, {w} x {h}, {nb} band(s), {} level(s), {:?}",
        if big { "Big" } else { "" },
        a.dtype,
        levels.len(),
        a.codecs.first().map_or("no compression".into(), |c| format!("{c:?}"))
    );
    let var = Variable {
        name: name.clone(),
        bands,
        fill,
        scale: num("scale").unwrap_or(1.0),
        offset: num("offset").unwrap_or(0.0),
        units: item("unittype", 0).unwrap_or_default(),
        georef: georef(&rd, first),
        levels,
    };
    Ok(Product { name, desc, vars: vec![var] })
}

fn array(rd: &Rd, ifd: &Ifd) -> Result<Array> {
    let one = |t: u16, d: u64| ifd.get(&t).and_then(|v| v.uints(rd).first().copied()).unwrap_or(d);
    let (w, h) = (one(256, 0), one(257, 0));
    let (bps, spp, sfmt, planar) = (one(258, 1), one(277, 1), one(339, 1), one(284, 1));
    let codec = match one(259, 1) {
        1 => None,
        5 => Some(Codec::Lzw),
        8 | 32946 => Some(Codec::Deflate),
        50000 => Some(Codec::Zstd),
        32773 => Some(Codec::PackBits),
        7 => return Err("TIFF JPEG compression is not supported yet".into()),
        34712 => return Err("TIFF JPEG 2000 compression is not supported yet".into()),
        c => return Err(format!("TIFF compression {c} is not supported (none, LZW, Deflate, Zstd, PackBits)").into()),
    };
    let dtype = match (sfmt, bps) {
        (1, 8) => DType::U8,
        (2, 8) => DType::I8,
        (1, 16) => DType::U16,
        (2, 16) => DType::I16,
        (1, 32) => DType::U32,
        (2, 32) => DType::I32,
        (1, 64) => DType::U64,
        (2, 64) => DType::I64,
        (3, 32) => DType::F32,
        (3, 64) => DType::F64,
        (5, 32) => DType::CI16,
        (5, 64) => DType::CI32,
        (6, 64) => DType::CF32,
        (6, 128) => DType::CF64,
        _ => return Err(format!("TIFF sample format {sfmt} with {bps} bits is not supported").into()),
    };
    let (cw, ch, ot, lt) = if ifd.contains_key(&322) {
        (one(322, 0), one(323, 0), 324, 325)
    } else {
        (w, one(278, h).min(h), 273, 279)
    };
    let get = |t| ifd.get(&t).map(|v| v.uints(rd)).ok_or("TIFF without chunk offsets or sizes");
    let chunks = get(ot)?.into_iter().zip(get(lt)?).map(|(off, len)| ChunkLoc { src: 0, off, len }).collect();
    let chunky = planar == 1 && spp > 1;
    let (dims, shape, chunk) = match (spp, chunky) {
        (1, _) => (vec!["y", "x"], vec![h, w], vec![ch, cw]),
        (_, true) => (vec!["y", "x", "band"], vec![h, w, spp], vec![ch, cw, spp]),
        (_, false) => (vec!["band", "y", "x"], vec![spp, h, w], vec![1, ch, cw]),
    };
    let mut codecs: Vec<Codec> = codec.into_iter().collect();
    let pred = one(317, 1) as u16;
    if pred > 1 {
        let stride = if chunky { spp as u32 } else { 1 } * if dtype.is_complex() { 2 } else { 1 };
        codecs.push(Codec::Predictor { kind: pred, stride, row: cw as u32 * stride });
    }
    Ok(Array {
        dims: dims.into_iter().map(String::from).collect(),
        shape,
        chunk,
        dtype,
        le: rd.le,
        codecs,
        chunks,
    })
}

/// Items of the GDAL_METADATA XML: (role or name, sample, value).
fn gdal_metadata(xml: &str) -> Vec<(String, usize, String)> {
    let attr = |s: &str, a: &str| {
        let k = format!("{a}=\"");
        s.find(&k).map(|i| s[i + k.len()..].split('"').next().unwrap_or("").to_string())
    };
    let unescape = |s: &str| s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&");
    xml.split("<Item").skip(1).filter_map(|it| {
        let (tag, rest) = it.split_once('>')?;
        let val = rest.split("</Item>").next()?;
        let key = attr(tag, "role").or_else(|| attr(tag, "name"))?.to_lowercase();
        let sample = attr(tag, "sample").and_then(|s| s.parse().ok()).unwrap_or(0);
        Some((key, sample, unescape(val.trim())))
    })
    .collect()
}

fn georef(rd: &Rd, ifd: &Ifd) -> Georef {
    let f = |t| ifd.get(&t).map(|v| v.f64s(rd)).unwrap_or_default();
    let (scale, tie, mt) = (f(33550), f(33922), f(34264));
    let mut gt = if mt.len() >= 16 {
        [mt[3], mt[0], mt[1], mt[7], mt[4], mt[5]]
    } else if scale.len() >= 2 && tie.len() >= 6 {
        let (sx, sy) = (scale[0], scale[1]);
        [tie[3] - tie[0] * sx, sx, 0.0, tie[4] + tie[1] * sy, 0.0, -sy]
    } else {
        return Georef::None;
    };
    let keys = ifd.get(&34735).map(|t| t.uints(rd)).unwrap_or_default();
    let dbl = f(34736);
    let asc = ifd.get(&34737).map(|t| String::from_utf8_lossy(&t.raw).into_owned()).unwrap_or_default();
    let (mut ks, mut text) = (HashMap::new(), HashMap::new());
    for k in keys.get(4..).unwrap_or(&[]).chunks_exact(4) {
        let (id, loc, n, v) = (k[0] as u16, k[1], k[2] as usize, k[3] as usize);
        match loc {
            0 => {
                ks.insert(id, v as f64);
            }
            34736 => {
                if let Some(&d) = dbl.get(v) {
                    ks.insert(id, d);
                }
            }
            34737 => {
                if let Some(s) = asc.get(v..v + n) {
                    text.insert(id, s.trim_end_matches(['|', '\0']).to_string());
                }
            }
            _ => {}
        }
    }
    // PixelIsPoint: GDAL moves the origin by half a pixel to the pixel corner.
    if ks.get(&1025) == Some(&2.0) {
        gt[0] -= 0.5 * (gt[1] + gt[2]);
        gt[3] -= 0.5 * (gt[4] + gt[5]);
    }
    let code = |k| ks.get(&k).map(|&v| v as u32).filter(|&v| v > 0 && v != 32767);
    let epsg = if ks.get(&1024) == Some(&2.0) { code(2048) } else { code(3072).or(code(2048)) };
    let name = [3073, 1026, 2049]
        .iter()
        .find_map(|k| text.get(k).filter(|s| !s.is_empty()).cloned())
        .or(epsg.map(|e| format!("EPSG:{e}")))
        .unwrap_or_else(|| "unknown CRS".into());
    Georef::Affine { gt, crs: Crs { epsg, name } }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gdal_metadata_items() {
        let x = r#"<GDALMetadata><Item name="SCALE" sample="0" role="scale">0.0001</Item>
            <Item name="DESCRIPTION" sample="1" role="description">B&amp;04</Item></GDALMetadata>"#;
        let m = super::gdal_metadata(x);
        assert_eq!(m, [("scale".into(), 0, "0.0001".into()), ("description".into(), 1, "B&04".into())]);
    }
}
