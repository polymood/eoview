//! CEOS SAR products (ERS, JERS, Radarsat-1, ALOS PALSAR). A product is a set of files: a volume directory
//! (VOL), a leader file (LED, or .L) with the metadata, one image file for each polarization (IMG-HH, ..., or
//! .D) and a trailer file (TRL). Each file is a list of records. A record starts with a header of 12 bytes:
//! the record number (u32), 4 type codes and the record length (u32), big-endian.
//!
//! Specification: CEOS SAR CCT format (CEOS-SAR-CCT, Issue 2), and its versions in the ALOS PALSAR level 1.1 /
//! 1.5 product format description (JAXA, NEB-070062) and the Radarsat-1 CEOS format (RSI-GS-026). The offsets
//! below are offsets in the record (0 is the first byte of the header), checked on the sample products:
//! ALOS PALSAR 1.1 and 1.5, ALOS-2 1.5, Radarsat-1 (GDAL test data).
//!
//! Image file descriptor (the first record of an image file), ASCII fields: number of lines at 236 (I8),
//! pixels per line at 248 (I8), record length at 186 (I6), bits per sample at 216 (I4), bytes per pixel at
//! 224 (I4), prefix bytes of a line record at 276 (I4, with the 12 bytes of the header), sample format at 400
//! (A28: "UNSIGNED INTEGER*1", "COMPLEX*8", ...). Each line is one record after the descriptor.
//!
//! Supported: 8 and 16-bit unsigned, 16-bit signed, 32-bit float, complex 16-bit integer and complex 32-bit
//! float images, the data set summary record of the leader as metadata, the 4 corners of the map projection
//! record (ALOS level 1.5) as georeferencing. Not supported: the georeferencing of slant range products
//! (level 1.1, Radarsat SLC and path image), the signal data (level 1.0), the calibration.
use crate::Source;
use eo_core::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// True for the first bytes of a CEOS file: record 1, type codes (any, 0xC0, 0x12, 0x12).
pub fn is_ceos(head: &[u8]) -> bool {
    head.len() >= 12 && head[..4] == [0, 0, 0, 1] && head[5..8] == [0xc0, 0x12, 0x12]
}

/// The ASCII field at `o` of `n` bytes, without spaces.
fn text(r: &[u8], o: usize, n: usize) -> String {
    r.get(o..o + n).map(|b| String::from_utf8_lossy(b).trim().to_string()).unwrap_or_default()
}

fn int(r: &[u8], o: usize, n: usize) -> Option<u64> {
    text(r, o, n).parse().ok()
}

/// The image files of the product of `path` with their names ("HH", ...), and its leader file.
fn files(path: &Path) -> (Vec<(String, PathBuf)>, Option<PathBuf>) {
    let (dir, name) = (path.parent().unwrap_or(Path::new("")), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    let names: Vec<String> = std::fs::read_dir(dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    // ALOS: VOL-<id>, LED-<id>, IMG-<pol>-<id>, TRL-<id>. The image files can have an extension (".head" for a part).
    let id = match name.get(..4) {
        Some("VOL-" | "LED-" | "TRL-") => Some(name[4..].to_string()),
        Some("IMG-") => name[4..].split_once('-').map(|x| x.1.to_string()),
        _ => None,
    };
    if let Some(id) = id {
        let id = id.trim_end_matches(".head").to_string();
        let mut imgs: Vec<(String, PathBuf)> = names
            .iter()
            .filter_map(|n| n.strip_prefix("IMG-")?.split_once('-').filter(|x| x.1 == id || x.1 == format!("{id}.head")).map(|x| (x.0.to_string(), dir.join(n))))
            .collect();
        imgs.sort();
        let led = names.iter().find(|n| **n == format!("LED-{id}")).map(|n| dir.join(n));
        return (imgs, led);
    }
    // ERS (ESA): DAT_01.001 (image) and LEA_01.001 (leader).
    if let Some(rest) = name.strip_prefix("DAT_").or(name.strip_prefix("LEA_")) {
        let (img, led) = (dir.join(format!("DAT_{rest}")), dir.join(format!("LEA_{rest}")));
        return (img.exists().then(|| (String::new(), img)).into_iter().collect(), led.exists().then_some(led));
    }
    // Radarsat and others: <name>.D (image) and <name>.L (leader), in any case.
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_uppercase());
    let find = |e: &str| names.iter().find(|n| n.len() == stem.len() + 2 && n.starts_with(&stem) && n[stem.len()..].eq_ignore_ascii_case(e)).map(|n| dir.join(n));
    match ext.as_deref() {
        Some("L") => (find(".D").map(|p| (String::new(), p)).into_iter().collect(), Some(path.into())),
        _ => (vec![(String::new(), path.into())], find(".L")),
    }
}

/// The records of a file. At most `max` records, and only the records of the first
/// 64 KB of each (the leader records with metadata are small; the large ones are tables).
fn records(src: &Source, max: usize) -> Result<Vec<Vec<u8>>> {
    let len = src.len()?;
    let (mut out, mut o) = (vec![], 0);
    while o + 12 <= len && out.len() < max {
        let h = src.read(o..o + 12)?;
        let n = u32::from_be_bytes(h[8..12].try_into().unwrap()) as u64;
        if n < 12 || o + n > len {
            break;
        }
        out.push(src.read(o..o + n.min(1 << 16))?.to_vec());
        o += n;
    }
    Ok(out)
}

/// The variable of image file `src` (index `idx` of the sources). Second result: the number of lines in the
/// descriptor (more than in the array if the file is a part).
fn image(src: &Source, idx: u32, band: &str) -> Result<(Variable, u64)> {
    let len = src.len()?;
    let d = src.read(0..720.min(len))?;
    let dl = u32::from_be_bytes(d.get(8..12).ok_or("CEOS: short image file")?.try_into().unwrap()) as u64;
    let field = |o, n, what: &str| int(&d, o, n).ok_or_else(|| Error::from(format!("CEOS image file: no {what}")));
    let (lines, w, reclen, bits, bpp, prefix) = (field(236, 8, "number of lines")?, field(248, 8, "number of pixels")?, field(186, 6, "record length")?, field(216, 4, "bits per sample")?, field(224, 4, "bytes per pixel")?, field(276, 4, "prefix length")?);
    let fmt = text(&d, 400, 28);
    let dtype = match (fmt.split('*').next().unwrap_or(""), bits, bpp) {
        ("COMPLEX" | "COMPLEX INTEGER", 16, 4) | ("COMPLEX INTEGER", _, 4) => DType::CI16,
        ("COMPLEX" | "COMPLEX REAL", _, 8) => DType::CF32,
        ("UNSIGNED INTEGER", 8, 1) => DType::U8,
        ("UNSIGNED INTEGER", 16, 2) => DType::U16,
        ("SIGNED INTEGER" | "INTEGER", 16, 2) => DType::I16,
        ("REAL" | "IEEE REAL", 32, 4) => DType::F32,
        _ => return Err(format!("CEOS image file: sample format \"{fmt}\" of {bits} bits, {bpp} bytes is not supported").into()),
    };
    // A part of a file (or a file that a transfer cut): the lines that are in it.
    let n = lines.min(len.saturating_sub(dl) / reclen.max(1));
    let a = crate::envisat::rows(idx, len, dl, reclen, prefix, n, w, dtype, false)?;
    let name = if band.is_empty() { "Amplitude".to_string() } else { band.to_string() };
    Ok((Variable { name: name.clone(), group: String::new(), levels: vec![a], bands: vec![name], fill: None, scale: 1.0, offset: 0.0, units: String::new(), georef: Georef::None, times: Default::default(), dims: vec![] }, lines))
}

/// Metadata of the leader records, and the 4 corners of the map projection record (latitude, longitude of the
/// first line first pixel, first line last pixel, last line last pixel, last line first pixel).
fn leader(src: &Source, attrs: &mut Attrs) -> Result<Option<[[f64; 2]; 4]>> {
    let mut corners = None;
    for r in records(src, 64)? {
        let t = r[5];
        // Data set summary.
        if t == 0x0a && r.len() >= 1142 {
            for (o, n, k) in [(20, 32, "Scene ID"), (68, 32, "Scene center time"), (116, 16, "Scene center latitude"), (132, 16, "Scene center longitude"), (148, 16, "Platform heading"), (164, 16, "Ellipsoid"), (396, 16, "Mission"), (412, 32, "Sensor"), (444, 8, "Orbit"), (500, 16, "Wavelength (m)"), (1110, 32, "Product type")] {
                let v = text(&r, o, n);
                if !v.is_empty() {
                    attrs.push((k.into(), v));
                }
            }
        }
        // Map projection record (geocoded products).
        if t == 0x14 && r.len() >= 1200 {
            let f = |o: usize| text(&r, o, 16).parse::<f64>().ok().filter(|v| v.is_finite());
            let c: Option<Vec<[f64; 2]>> = (0..4).map(|k| Some([f(1072 + 32 * k)?, f(1088 + 32 * k)?])).collect();
            corners = c.filter(|c| c.iter().all(|p| p[0].abs() <= 90.0 && p[1].abs() <= 360.0 && (p[0], p[1]) != (0.0, 0.0))).map(|c| [c[0], c[1], c[2], c[3]]);
        }
    }
    Ok(corners)
}

pub fn open(path: &str, first: Source, rt: &tokio::runtime::Handle, name: String) -> Result<crate::Dataset> {
    let (imgs, led) = files(Path::new(path));
    if imgs.is_empty() {
        return Err(format!("{name}: CEOS product without an image file (IMG-* or .D) in its directory").into());
    }
    let mut attrs: Attrs = vec![];
    let corners = match &led {
        Some(p) => leader(&Source::open(&p.to_string_lossy(), rt)?, &mut attrs)?,
        None => None,
    };
    let (mut vars, mut sources) = (vec![], vec![]);
    let mut first = Some(first);
    for (band, p) in &imgs {
        let src = match first.take().filter(|_| p == Path::new(path)) {
            Some(s) => s,
            None => Source::open(&p.to_string_lossy(), rt)?,
        };
        src.sparse(true);
        let (mut v, lines) = image(&src, sources.len() as u32, band)?;
        let (w, n) = (v.levels[0].shape[1], v.levels[0].shape[0]);
        if n < lines {
            attrs.push(("note".into(), format!("{}: {n} of the {lines} lines are in the file", p.file_name().unwrap_or_default().to_string_lossy())));
        }
        if let Some(c) = corners {
            // The corners are the centers of the corner pixels of the whole image.
            // A geocoded image has 0 outside the scene.
            v.fill = Some(0.0);
            v.georef = Georef::Grid { cols: vec![0.5, w as f64 - 0.5], rows: vec![0.5, lines as f64 - 0.5], lon: vec![c[0][1], c[1][1], c[3][1], c[2][1]], lat: vec![c[0][0], c[1][0], c[3][0], c[2][0]] };
        }
        vars.push(v);
        sources.push(Arc::new(src));
    }
    let metas = vars.iter().map(|v| Meta { name: v.name.clone(), dims: vec![("y".into(), v.levels[0].shape[0]), ("x".into(), v.levels[0].shape[1])], dtype: format!("{:?}", v.levels[0].dtype), attrs: vec![] }).collect();
    let mission = attrs.iter().find(|a| a.0 == "Mission").map_or(String::new(), |a| format!("{} ", a.1));
    let desc = format!("{mission}CEOS SAR, {} image file(s){}", vars.len(), if led.is_none() { ", no leader file" } else { "" });
    let product = Product { name, desc, vars, small: vec![], valid: None, info: Info { attrs, dims: vec![], vars: metas } };
    Ok(crate::Dataset { product, sources })
}

#[cfg(test)]
mod tests {
    /// An image file of 2 lines of 3 pixels (16-bit, 4 prefix bytes after the header), and a part of it.
    #[test]
    fn image_file() {
        let mut d = vec![b' '; 720];
        d[..12].copy_from_slice(&[0, 0, 0, 1, 0x3f, 0xc0, 0x12, 0x12, 0, 0, 2, 208]);
        for (o, s) in [(186, "    22"), (216, "  16"), (224, "   2"), (236, "       2"), (248, "       3"), (276, "  16"), (400, "UNSIGNED INTEGER*2")] {
            d[o..o + s.len()].copy_from_slice(s.as_bytes());
        }
        for r in 0..2u8 {
            d.extend([0u8; 16]);
            d.extend([0, r, 0, 2, 1, 0]);
        }
        let dir = std::env::temp_dir().join(format!("eoview-ceos-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("T.D");
        std::fs::write(&p, &d).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ds = crate::open(p.to_str().unwrap(), rt.handle()).unwrap();
        let a = &ds.product.vars[0].levels[0];
        assert_eq!((a.shape.clone(), a.dtype, a.chunks.at(1).off), (vec![2, 3], eo_core::DType::U16, 720 + 22 + 16));
        // A part: the line that is in the file.
        std::fs::write(&p, &d[..760]).unwrap();
        assert_eq!(crate::open(p.to_str().unwrap(), rt.handle()).unwrap().product.vars[0].levels[0].shape, vec![1, 3]);
        std::fs::write(&p, &d[..700]).unwrap();
        assert!(crate::open(p.to_str().unwrap(), rt.handle()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
