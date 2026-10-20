//! Envisat N1 products (ASAR, MERIS and AATSR level 1). The file has a main product header (MPH, 1247
//! bytes of ASCII `KEY=value` lines), a specific product header (SPH, ASCII) and the data set descriptors
//! (DSD, 280 bytes each, at the end of the SPH). A DSD gives the offset, the size, the number of records
//! and the record size of one data set: a measurement data set (MDS), an annotation data set (ADS) or a
//! global annotation data set (GADS). The values are big-endian.
//!
//! Specifications: Envisat Products Specifications Volume 5 (ASAR), Volume 11 (MERIS), Volume 12 (AATSR),
//! PO-RS-MDA-GS-2009 and the ESA Envisat product format ("N1") description, <https://earth.esa.int/eogateway/>.
//! The record layouts below come from these documents and from the sample products: ASAR records have 17
//! bytes before the pixels (time 12, attachment flag 1, line number 4), MERIS records have 13 (time 12,
//! flag 1), AATSR records have 20 and 512 pixels (the pixels are the last 1024 bytes of the record: this
//! offset comes from the sample, not from the specification).
//!
//! Supported: all headers as metadata, the image data sets of ASAR products with one array of pixels for each
//! record (IMM, IMP, IMS, APM, APP, APS, WSM, 8-bit, 16-bit, complex 16-bit), the radiance and flag bands of
//! MERIS level 1 (the scaling is in the GADS), the 14 bands, the confidence and the cloud flags of AATSR
//! level 1, the geolocation grid of ASAR and the tie points of MERIS (variables and georeferencing).
//! Not supported: the other products (wave mode, level 2, auxiliary products), the AATSR geolocation, the
//! other annotation data sets, the AATSR scaling (the values are the stored 16-bit values).
use crate::Source;
use eo_core::*;

const MPH_SIZE: u64 = 1247;

/// True for the first bytes of an MPH.
pub fn is_envisat(head: &[u8]) -> bool {
    head.starts_with(b"PRODUCT=\"")
}

/// The `KEY=value` lines of an ASCII header. A quoted value loses its quotes and its trailing spaces.
fn fields(text: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    for l in text.split('\n') {
        if let Some((k, v)) = l.split_once('=') {
            let v = v.trim().trim_matches('"').trim_end();
            out.push((k.trim().to_string(), v.to_string()));
        }
    }
    out
}

fn get<'a>(f: &'a [(String, String)], k: &str) -> Option<&'a str> {
    f.iter().find(|x| x.0 == k).map(|x| x.1.as_str())
}

/// The integer at the start of a value like `+0000006099<bytes>`.
fn int(f: &[(String, String)], k: &str) -> Option<u64> {
    get(f, k)?.split('<').next()?.trim().trim_start_matches('+').parse().ok()
}

/// A data set descriptor.
struct Dsd {
    name: String,
    kind: char,
    off: u64,
    n: u64,
    size: u64,
}

/// One array of `n` records. Row `i` has `w` values at `off + i * stride + skip`. One chunk for each row:
/// a record has bytes before and after its values. Used for the records of Envisat and CEOS.
/// `len` is the length of the file: the records must be inside it.
// ponytail: one chunk for each row (a 20 000 row image has 20 000 chunk reads), merge the rows of a file without prefix if the speed matters.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rows(src: u32, len: u64, off: u64, stride: u64, skip: u64, n: u64, w: u64, dtype: DType, le: bool) -> Result<Array> {
    let end = n.checked_mul(stride).and_then(|b| b.checked_add(off));
    let row = w.checked_mul(dtype.size() as u64);
    match (end, row) {
        (Some(end), Some(row)) if n > 0 && w > 0 && n <= MAX_CHUNKS as u64 && end <= len && skip + row <= stride => {
            let chunks = (0..n).map(|i| ChunkLoc { src, off: off + i * stride + skip, len: row }).collect();
            Ok(Array { dims: vec!["y".into(), "x".into()], shape: vec![n, w], chunk: vec![1, w], dtype, le, codecs: vec![], chunks, place: None })
        }
        _ => Err(format!("{n} records of {stride} bytes at {off}: not inside the file of {len} bytes").into()),
    }
}

pub fn open(src: &Source, idx: u32, name: String) -> Result<Product> {
    let len = src.len()?;
    let mph = fields(&String::from_utf8_lossy(&src.read(0..MPH_SIZE.min(len))?));
    let (sph_size, nd, ds) = (int(&mph, "SPH_SIZE").ok_or("Envisat: no SPH_SIZE")?, int(&mph, "NUM_DSD").ok_or("Envisat: no NUM_DSD")?, int(&mph, "DSD_SIZE").ok_or("Envisat: no DSD_SIZE")?);
    // The sizes come from the file: the headers must be in the file and the DSD in the SPH.
    if sph_size > 1 << 24 || nd > 1000 || ds == 0 || ds > 4096 || nd * ds > sph_size || MPH_SIZE + sph_size > len {
        return Err(format!("{name}: Envisat headers with sizes that are not valid (SPH {sph_size}, {nd} DSD of {ds} bytes)").into());
    }
    let sph = src.read(MPH_SIZE..MPH_SIZE + sph_size)?;
    let first = (sph_size - nd * ds) as usize;
    let sph_f = fields(&String::from_utf8_lossy(&sph[..first]));
    let mut dsds = vec![];
    let mut attrs: Attrs = mph.iter().chain(&sph_f).cloned().collect();
    for i in 0..nd as usize {
        let f = fields(&String::from_utf8_lossy(&sph[first + i * ds as usize..first + (i + 1) * ds as usize]));
        let Some(n) = get(&f, "DS_NAME") else { continue };
        let d = Dsd { name: n.to_string(), kind: get(&f, "DS_TYPE").and_then(|t| t.chars().next()).unwrap_or('?'), off: int(&f, "DS_OFFSET").unwrap_or(0), n: int(&f, "NUM_DSR").unwrap_or(0), size: int(&f, "DSR_SIZE").unwrap_or(0) };
        attrs.push((format!("DSD {}", d.name), format!("type {}, offset {}, {} records of {} bytes", d.kind, d.off, d.n, d.size)));
        dsds.push(d);
    }
    let kind = get(&mph, "PRODUCT").unwrap_or("").get(..9).unwrap_or("").to_string();
    // (bytes before the pixels, pixels in a record) for the measurement data sets, by instrument.
    let width = int(&sph_f, "LINE_LENGTH");
    let (prefix, w) = match kind.get(..3) {
        Some("ASA") => (17, width),
        Some("MER") => (13, width),
        Some("ATS") => (20, Some(512)),
        _ => (0, None),
    };
    let (sample, data) = (get(&sph_f, "SAMPLE_TYPE").unwrap_or(""), get(&sph_f, "DATA_TYPE").unwrap_or(""));
    let mut vars = vec![];
    let mut skipped = vec![];
    for d in dsds.iter().filter(|d| d.kind == 'M' && d.n > 0) {
        let Some(w) = w.filter(|&w| w > 0 && d.size > prefix) else { skipped.push(d.name.clone()); continue };
        let bytes = (d.size - prefix) / w;
        let dtype = match (kind.get(..3), bytes, (d.size - prefix) % w) {
            (_, _, r) if r != 0 => None,
            (Some("ASA"), 1, _) if data == "UBYTE" => Some(DType::U8),
            (Some("ASA"), 2, _) if data == "UWORD" && sample != "COMPLEX" => Some(DType::U16),
            (Some("ASA"), 4, _) if sample == "COMPLEX" => Some(DType::CI16),
            (Some("ASA"), ..) => None,
            (_, 2, _) => Some(DType::U16),
            _ => None,
        };
        // AATSR values are signed (the sample has negative fill values).
        let dtype = if kind.starts_with("ATS") && dtype == Some(DType::U16) { Some(DType::I16) } else { dtype };
        let Some(dtype) = dtype else { skipped.push(d.name.clone()); continue };
        match rows(idx, len, d.off, d.size, prefix, d.n, w, dtype, false) {
            Ok(a) => vars.push(Variable { name: d.name.clone(), group: String::new(), levels: vec![a], bands: vec![d.name.clone()], fill: None, scale: 1.0, offset: 0.0, units: String::new(), georef: Georef::None, times: Default::default(), dims: vec![] }),
            Err(e) => eprintln!("{}: {}: {e}", redact(src.name()), d.name),
        }
    }
    // MERIS: the radiance of band k is the stored value times the factor k of the scaling GADS (floats 7 to 21, checked on the sample: the solar fluxes follow at 41 to 55).
    if let Some(g) = dsds.iter().find(|d| d.name == "Scaling Factor GADS" && d.size >= 88 && d.off + 88 <= len) {
        let b = src.read(g.off..g.off + 88)?;
        for v in &mut vars {
            let k = v.name.strip_prefix("Radiance MDS(").and_then(|s| s.trim_end_matches(')').parse::<usize>().ok());
            if let Some(k) = k.filter(|k| (1..=15).contains(k)) {
                v.scale = f32::from_be_bytes(b[4 * (6 + k)..4 * (7 + k)].try_into().unwrap()) as f64;
                v.units = "mW/m2/sr/nm".into();
            }
        }
    }
    let geo = grid(src, idx, len, &dsds, &sph_f);
    let image = vars.len();
    if let Some((cols, rows, lon, lat, tie)) = geo {
        for v in &mut vars {
            v.georef = Georef::Grid { cols: cols.clone(), rows: rows.clone(), lon: lon.clone(), lat: lat.clone() };
        }
        vars.extend(tie);
    }
    if vars.is_empty() {
        return Err(format!("{name}: Envisat product {kind}: this reader shows the image data sets of ASAR, MERIS level 1 and AATSR level 1 only. The headers are read, but the product has no such data set").into());
    }
    if !skipped.is_empty() {
        attrs.push(("note".into(), format!("Not shown (no reader for the layout): {}", skipped.join(", "))));
    }
    let metas = vars.iter().map(|v| Meta { name: v.name.clone(), dims: vec![("y".into(), v.levels[0].shape[0]), ("x".into(), v.levels[0].shape[1])], dtype: format!("{:?}", v.levels[0].dtype), attrs: vec![] }).collect();
    let desc = format!("Envisat {kind}, {image} image data sets, {} data sets in the file", dsds.len());
    Ok(Product { name, desc, vars, small: vec![], valid: None, info: Info { attrs, dims: vec![], vars: metas } })
}

/// Georeferencing from the tie points: nodes (columns, rows, longitude, latitude) and the latitude and
/// longitude arrays as variables. ASAR: the geolocation grid ADS, one record every few lines with 11 tie points
/// (line number u32 at 13, sample numbers u32 x 11 at 25, latitude i32 x 11 at 157, longitude i32 x 11 at
/// 201, in 1e-6 degrees; the numbers start at 1). MERIS: the tie points ADS, 1 record every `LINES_PER_TIE_PT`
/// lines, with the latitude i32 and then the longitude i32 of the tie points at 13 (in 1e-6 degrees).
#[allow(clippy::type_complexity)]
fn grid(src: &Source, idx: u32, len: u64, dsds: &[Dsd], sph: &[(String, String)]) -> Option<(Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>, Vec<Variable>)> {
    let var = |name: &str, a: Array| Variable { name: name.into(), group: String::new(), levels: vec![a], bands: vec![name.into()], fill: None, scale: 1e-6, offset: 0.0, units: "degrees".into(), georef: Georef::None, times: Default::default(), dims: vec![] };
    let i32s = |b: &[u8], p: usize, n: usize| (0..n).map(|k| i32::from_be_bytes(b[p + 4 * k..p + 4 * k + 4].try_into().unwrap()) as f64).collect::<Vec<f64>>();
    // The ADS is read whole: at most 16 MB.
    let find = |name: &str| dsds.iter().find(|d| d.name == name && d.n > 0 && d.size > 0 && d.n * d.size <= 1 << 24 && d.off + d.n * d.size <= len);
    if let Some(d) = find("GEOLOCATION GRID ADS").filter(|d| d.size >= 245) {
        let b = src.read(d.off..d.off + d.n * d.size).ok()?;
        let rec = |i: u64| &b[(i * d.size) as usize..((i + 1) * d.size) as usize];
        let cols: Vec<f64> = (0..11).map(|k| u32::from_be_bytes(rec(0)[25 + 4 * k..29 + 4 * k].try_into().unwrap()) as f64 - 0.5).collect();
        let rows: Vec<f64> = (0..d.n).map(|i| u32::from_be_bytes(rec(i)[13..17].try_into().unwrap()) as f64 - 0.5).collect();
        let (mut lat, mut lon) = (vec![], vec![]);
        for i in 0..d.n {
            lat.extend(i32s(rec(i), 157, 11).iter().map(|x| x * 1e-6));
            lon.extend(i32s(rec(i), 201, 11).iter().map(|x| x * 1e-6));
        }
        let mk = |p| rows_of(idx, len, d, p, 11);
        return Some((cols, rows, lon, lat, vec![var("Tie point latitude", mk(157).ok()?), var("Tie point longitude", mk(201).ok()?)]));
    }
    let (ls, ss) = (int(sph, "LINES_PER_TIE_PT")?, int(sph, "SAMPLES_PER_TIE_PT")?);
    let w = int(sph, "LINE_LENGTH")?;
    let d = find("Tie points ADS")?;
    let k = (w.checked_sub(1)?).div_ceil(ss.max(1)) + 1;
    if ss == 0 || ls == 0 || d.size < 13 + 8 * k {
        return None;
    }
    let b = src.read(d.off..d.off + d.n * d.size).ok()?;
    let cols: Vec<f64> = (0..k).map(|i| (i * ss) as f64 + 0.5).collect();
    let rows: Vec<f64> = (0..d.n).map(|j| (j * ls) as f64 + 0.5).collect();
    let (mut lat, mut lon) = (vec![], vec![]);
    for j in 0..d.n as usize {
        let r = &b[j * d.size as usize..];
        lat.extend(i32s(r, 13, k as usize).iter().map(|x| x * 1e-6));
        lon.extend(i32s(r, 13 + 4 * k as usize, k as usize).iter().map(|x| x * 1e-6));
    }
    let mk = |p| rows_of(idx, len, d, p, k);
    Some((cols, rows, lon, lat, vec![var("Tie point latitude", mk(13).ok()?), var("Tie point longitude", mk(13 + 4 * k).ok()?)]))
}

/// The i32 values at `skip` of the records of an ADS, as an array of one row for each record.
fn rows_of(idx: u32, len: u64, d: &Dsd, skip: u64, w: u64) -> Result<Array> {
    rows(idx, len, d.off, d.size, skip, d.n, w, DType::I32, false)
}

#[cfg(test)]
mod tests {
    /// A product with an MPH, an SPH with one DSD of a measurement data set and 2 records of 3 pixels (ASAR).
    #[test]
    fn asar_like_product() {
        let pad = |s: &str, n: usize| format!("{s:<n$}");
        let mph = pad("PRODUCT=\"ASA_IMM_1PXPDE20020730_095830_000001002008_00108_02166_0066.N1\"\nSPH_SIZE=+0000000380<bytes>\nNUM_DSD=+0000000001\nDSD_SIZE=+0000000280<bytes>\n", 1247);
        let sph = pad("SAMPLE_TYPE=\"DETECTED\"\nLINE_LENGTH=+00003<samples>\nDATA_TYPE=\"UWORD\"\n", 100);
        let dsd = pad(&format!("DS_NAME=\"MDS1                        \"\nDS_TYPE=M\nDS_OFFSET=+{:020}<bytes>\nDS_SIZE=+{:020}<bytes>\nNUM_DSR=+0000000002\nDSR_SIZE=+0000000023<bytes>\n", 1247 + 380, 46), 280);
        let mut f = (mph + &sph + &dsd).into_bytes();
        for r in 0..2u8 {
            f.extend([0u8; 17]);
            f.extend([0, r, 0, 2, 0, 3]);
        }
        let dir = std::env::temp_dir().join(format!("eoview-n1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.N1");
        std::fs::write(&p, &f).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let d = crate::open(p.to_str().unwrap(), rt.handle()).unwrap();
        let v = &d.product.vars[0];
        assert_eq!((v.name.as_str(), v.levels[0].shape.clone()), ("MDS1", vec![2, 3]));
        assert_eq!(v.levels[0].chunks.at(1).off, 1627 + 23 + 17);
        assert!(d.product.info.attrs.iter().any(|a| a.0 == "LINE_LENGTH" && a.1 == "+00003<samples>"));
        // A truncated file is an error, not a panic.
        std::fs::write(&p, &f[..1650]).unwrap();
        assert!(crate::open(p.to_str().unwrap(), rt.handle()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
