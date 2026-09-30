//! SAFE products: a directory with a manifest and metadata files, and the measurement files.
//!
//! Sentinel-2 L1C and L2A: Sentinel-2 Products Specification Document (S2-PDGS-TAS-DI-PSD),
//! <https://sentinels.copernicus.eu/documents/247904/685211/S2-PDGS-TAS-DI-PSD-V14.9.pdf>.
use crate::xml::{attr, elems, text};
use crate::{Dataset, Source, jp2};
use eo_core::*;
use std::path::Path;
use std::sync::Arc;
use tokio::runtime::Handle;

const S2_BANDS: [&str; 13] = ["B01", "B02", "B03", "B04", "B05", "B06", "B07", "B08", "B8A", "B09", "B10", "B11", "B12"];

fn read(p: &Path) -> Result<String> {
    std::fs::read_to_string(p).map_err(|e| Error(format!("{}: {e}", p.display())))
}

/// First file in `dir` whose name starts with `prefix` and ends with `suffix`.
fn find(dir: &Path, prefix: &str, suffix: &str) -> Option<std::path::PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix)))
        .collect();
    v.sort();
    v.into_iter().next()
}

/// Kind of SAFE product in `dir`, if it is one.
pub fn kind(dir: &Path) -> Option<&'static str> {
    if find(dir, "MTD_MSIL", ".xml").is_some() {
        Some("S2")
    } else {
        None
    }
}

pub fn open(dir: &Path, rt: &Handle) -> Result<Dataset> {
    match kind(dir) {
        Some("S2") => s2(dir, rt),
        _ => Err(format!("{}: unknown SAFE product", dir.display()).into()),
    }
}

/// Sentinel-2 L1C or L2A: one variable for each band, at the finest resolution of the product.
/// The JPEG 2000 resolution levels are the overviews.
fn s2(dir: &Path, rt: &Handle) -> Result<Dataset> {
    let mtd_path = find(dir, "MTD_MSIL", ".xml").ok_or("no MTD_MSIL*.xml")?;
    let mtd = read(&mtd_path)?;
    let l2a = mtd_path.to_string_lossy().contains("MSIL2A");
    // Physical value = (DN + offset) / quantification.
    let quant: f64 = text(&mtd, if l2a { "BOA_QUANTIFICATION_VALUE" } else { "QUANTIFICATION_VALUE" })
        .and_then(|v| v.parse().ok())
        .unwrap_or(10000.0);
    let offsets: Vec<(usize, f64)> = elems(&mtd, if l2a { "BOA_ADD_OFFSET" } else { "RADIO_ADD_OFFSET" })
        .iter()
        .filter_map(|(a, t)| Some((attr(a, "band_id")?.parse().ok()?, t.trim().parse().ok()?)))
        .collect();
    let granule = std::fs::read_dir(dir.join("GRANULE"))?.flatten().map(|e| e.path()).find(|p| p.is_dir()).ok_or("no granule")?;
    let tl = read(&granule.join("MTD_TL.xml"))?;
    let epsg = text(&tl, "HORIZONTAL_CS_CODE").and_then(|c| c.rsplit(':').next()?.parse().ok());
    let crs = Crs { epsg, name: text(&tl, "HORIZONTAL_CS_NAME").unwrap_or_default() };
    let geo: Vec<(u32, [f64; 4])> = elems(&tl, "Geoposition")
        .iter()
        .filter_map(|(a, t)| {
            let n = |k| text(t, k)?.parse::<f64>().ok();
            Some((attr(a, "resolution")?.parse().ok()?, [n("ULX")?, n("ULY")?, n("XDIM")?, n("YDIM")?]))
        })
        .collect();

    // Image files: keep the finest resolution of each band.
    let mut files: Vec<(String, u32, String)> = vec![];
    for f in elems(&mtd, "IMAGE_FILE") {
        let rel = f.1.trim();
        let stem = rel.rsplit('/').next().unwrap_or(rel);
        let parts: Vec<&str> = stem.split('_').collect();
        let (name, res) = match parts.last() {
            Some(r) if r.ends_with('m') && r[..r.len() - 1].parse::<u32>().is_ok() => {
                (parts[parts.len() - 2].to_string(), r[..r.len() - 1].parse().unwrap())
            }
            Some(b) => (b.to_string(), match *b {
                "B02" | "B03" | "B04" | "B08" | "TCI" => 10,
                "B01" | "B09" | "B10" => 60,
                _ => 20,
            }),
            None => continue,
        };
        match files.iter_mut().find(|f| f.0 == name) {
            Some(e) if e.1 <= res => {}
            Some(e) => *e = (name, res, rel.into()),
            None => files.push((name, res, rel.into())),
        }
    }
    let order = |n: &str| S2_BANDS.iter().position(|b| *b == n).unwrap_or(99);
    files.sort_by_key(|f| (order(&f.0), f.0.clone()));

    let mut sources = vec![];
    let mut vars = vec![];
    for (name, res, rel) in files {
        let path = dir.join(format!("{rel}.jp2"));
        let src = Source::new(&path.to_string_lossy(), rt);
        let levels = jp2::arrays(&src, sources.len() as u32).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        sources.push(Arc::new(src));
        let band = order(&name);
        let (scale, offset, units) = match name.as_str() {
            _ if band < 99 => (1.0 / quant, offsets.iter().find(|o| o.0 == band).map_or(0.0, |o| o.1) / quant, ""),
            "AOT" => (1.0 / 1000.0, 0.0, ""),
            "WVP" => (1.0 / 1000.0, 0.0, "cm"),
            _ => (1.0, 0.0, ""),
        };
        let georef = geo.iter().find(|g| g.0 == res).map_or(Georef::None, |(_, g)| Georef::Affine {
            gt: [g[0], g[2], 0.0, g[1], 0.0, g[3]],
            crs: crs.clone(),
        });
        let nb = levels[0].len_of("band") as usize;
        let bands = if name == "TCI" { vec!["red".into(), "green".into(), "blue".into()] } else { (1..=nb).map(|b| format!("{name} {b}")).collect() };
        vars.push(Variable {
            name: format!("{name} ({res} m)"),
            levels,
            bands: if nb == 1 { vec![name.clone()] } else { bands },
            fill: Some(0.0),
            scale,
            offset,
            units: units.into(),
            georef,
        });
    }
    if vars.is_empty() {
        return Err("Sentinel-2 product without image files".into());
    }
    let pname = dir.file_name().map_or(String::new(), |n| n.to_string_lossy().into());
    let desc = format!("Sentinel-2 {} SAFE, {} bands, {}", if l2a { "L2A" } else { "L1C" }, vars.len(), crs.name);
    Ok(Dataset { product: Product { name: pname, desc, vars }, sources })
}
