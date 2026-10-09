//! SAFE products: a directory with a manifest and metadata files, and the measurement files.
//!
//! Sentinel-2 L1C and L2A: Sentinel-2 Products Specification Document (S2-PDGS-TAS-DI-PSD),
//! <https://sentinels.copernicus.eu/documents/247904/685211/S2-PDGS-TAS-DI-PSD-V14.9.pdf>.
//! Sentinel-3 OLCI, SLSTR, SYN Level 1 and 2: Sentinel-3 product data format specifications,
//! <https://sentinels.copernicus.eu/web/sentinel/technical-guides/sentinel-3-olci/level-1/products>.
//! Sentinel-1 Level 1: Sentinel-1 Product Specification (S1-RS-MDA-52-7441),
//! <https://sentinels.copernicus.eu/documents/247904/1877131/S1-RS-MDA-52-7441-3-15_Sentinel-1_ProductSpecification.pdf>.
use crate::xml::{attr, elems, text};
use crate::{Dataset, Source, jp2, netcdf, tiff};
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
    } else if dir.join("manifest.safe").exists() && dir.join("measurement").is_dir() && dir.join("annotation").is_dir() {
        Some("S1")
    } else if dir.join("xfdumanifest.xml").exists() {
        Some("S3")
    } else {
        None
    }
}

pub fn open(dir: &Path, rt: &Handle) -> Result<Dataset> {
    match kind(dir) {
        Some("S2") => s2(dir, rt),
        Some("S1") => s1(dir, rt),
        Some("S3") => s3(dir, rt),
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
            times: Default::default(),
            name: format!("{name} ({res} m)"),
            // Product tree: the spectral bands in one group, then AOT, SCL, TCI and WVP.
            group: if name.starts_with('B') { "Reflectance".into() } else { String::new() },
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
    Ok(Dataset { product: Product { name: pname, desc, vars, valid: None, info: Default::default() }, sources })
}

/// Sentinel-1 Level 1 (GRD, SLC): one variable for each measurement file (polarisation, and swath for SLC).
/// The geolocation grid of the annotation file gives the georeferencing. As GDAL, the grid uses the
/// pixel and line values of the annotation as pixel positions.
fn s1(dir: &Path, rt: &Handle) -> Result<Dataset> {
    let mut files: Vec<_> = std::fs::read_dir(dir.join("measurement"))?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "tiff" || e == "tif")).collect();
    files.sort();
    let mut sources = vec![];
    let mut vars = vec![];
    for f in files {
        let stem = f.file_stem().map_or(String::new(), |s| s.to_string_lossy().into());
        // s1a-iw1-slc-vv-<start>-<stop>-<orbit>-<datatake>-<index>
        let parts: Vec<&str> = stem.split('-').collect();
        let (swath, pol) = (parts.get(1).copied().unwrap_or(""), parts.get(3).copied().unwrap_or(""));
        let src = Source::new(&f.to_string_lossy(), rt);
        let p = tiff::open(&src, sources.len() as u32).map_err(|e| Error(format!("{}: {e}", f.display())))?;
        sources.push(Arc::new(src));
        let ann = read(&dir.join("annotation").join(format!("{stem}.xml")))?;
        let pts: Vec<(f64, f64, f64, f64)> = elems(&ann, "geolocationGridPoint")
            .iter()
            .filter_map(|(_, t)| {
                let n = |k| text(t, k)?.parse::<f64>().ok();
                Some((n("pixel")?, n("line")?, n("longitude")?, n("latitude")?))
            })
            .collect();
        let mut cols: Vec<f64> = pts.iter().map(|p| p.0).collect();
        let mut rows: Vec<f64> = pts.iter().map(|p| p.1).collect();
        for v in [&mut cols, &mut rows] {
            v.sort_by(f64::total_cmp);
            v.dedup();
        }
        let georef = if cols.len() * rows.len() == pts.len() {
            let (mut lon, mut lat) = (vec![f64::NAN; pts.len()], vec![f64::NAN; pts.len()]);
            for p in &pts {
                let (i, j) = (cols.partition_point(|&c| c < p.0), rows.partition_point(|&r| r < p.1));
                lon[j * cols.len() + i] = p.2;
                lat[j * cols.len() + i] = p.3;
            }
            Georef::Grid { cols, rows, lon, lat }
        } else {
            eprintln!("{}: geolocation grid is not regular", f.display());
            Georef::None
        };
        let swath_up = swath.to_uppercase();
        let name = if swath_up.len() > 2 && swath_up[2..].chars().all(|c| c.is_ascii_digit()) { format!("{swath_up} {}", pol.to_uppercase()) } else { pol.to_uppercase() };
        let mut v = p.vars.into_iter().next().ok_or("empty measurement file")?;
        v.bands = if v.bands.len() == 1 { vec![name.clone()] } else { v.bands };
        v.name = name;
        v.georef = georef;
        v.fill = Some(0.0);
        vars.push(v);
    }
    if vars.is_empty() {
        return Err("Sentinel-1 product without measurement files".into());
    }
    let pname = dir.file_name().map_or(String::new(), |n| n.to_string_lossy().into());
    let kind = pname.split('_').filter(|s| !s.is_empty()).skip(1).take(2).collect::<Vec<_>>().join(" ");
    let desc = format!("Sentinel-1 {kind} SAFE, {} measurement(s), {:?}", vars.len(), vars[0].levels[0].dtype);
    Ok(Dataset { product: Product { name: pname, desc, vars, valid: None, info: Default::default() }, sources })
}

/// Sentinel-3: the variables of all NetCDF files. A name that is in two files gets the file name first.
/// Geolocation arrays of the same shape give the georeferencing (geo_coordinates.nc for OLCI, geodetic_*.nc
/// for SLSTR; the tie-point files have their own latitude and longitude).
fn s3(dir: &Path, rt: &Handle) -> Result<Dataset> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "nc")).collect();
    // Full-resolution geolocation files first: their variables keep the short names.
    files.sort_by_key(|p| {
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        (!(n.starts_with("geo_coordinates") || n.starts_with("geodetic")), n)
    });
    let mut sources = vec![];
    let mut vars: Vec<Variable> = vec![];
    let mut per_file = vec![];
    let mut info = Info::default();
    for f in files {
        let src = Source::new(&f.to_string_lossy(), rt);
        let idx = sources.len() as u32;
        match netcdf::variables(&src, idx) {
            Ok((mut v, one_d, mut inf)) => {
                let stem = f.file_stem().map_or(String::new(), |s| s.to_string_lossy().into());
                let orig: Vec<String> = v.iter().map(|x| x.name.clone()).collect();
                // Product tree: one group for each file. The files of the bands (Oa01_radiance,
                // S7_BT_in, F1_BT_fn) go in one group for each measurement (radiance, BT_in, BT_fn).
                let band = stem.split_once('_').filter(|(b, _)| {
                    let d = b.trim_start_matches(|c: char| c.is_ascii_alphabetic());
                    b.len() <= 4 && d.len() < b.len() && !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())
                });
                let group = band.map_or(stem.clone(), |(_, rest)| rest.to_string());
                for x in &mut v {
                    x.group = group.clone();
                    if vars.iter().any(|o| o.name == x.name) {
                        x.name = format!("{stem}/{}", x.name);
                    }
                }
                // Metadata: a variable of the viewer has its name in the product; the others get the file name.
                for m in &mut inf.vars {
                    m.name = match orig.iter().position(|o| *o == m.name) {
                        Some(k) => v[k].name.clone(),
                        None => format!("{stem}/{}", m.name),
                    };
                }
                if info.attrs.is_empty() {
                    info.attrs = inf.attrs;
                }
                for d in inf.dims {
                    if !info.dims.contains(&d) {
                        info.dims.push(d);
                    }
                }
                info.vars.extend(inf.vars);
                per_file.push((idx, vars.len(), v.len(), one_d));
                vars.extend(v);
                sources.push(std::sync::Arc::new(src));
            }
            Err(e) => eprintln!("{}: {e}", f.display()),
        }
    }
    // Georeferencing: the geolocation variables of the same file first, then of all files.
    for (idx, first, n, one_d) in &per_file {
        for k in *first..first + n {
            let file_vars: Vec<Variable> = vars[*first..first + n].to_vec();
            let mut g = netcdf::georef(&sources[*idx as usize], &vars[k], one_d, &file_vars);
            if g == Georef::None {
                g = netcdf::georef(&sources[*idx as usize], &vars[k], &[], &vars);
            }
            vars[k].georef = g;
        }
    }
    if vars.is_empty() {
        return Err("Sentinel-3 product without variables".into());
    }
    // Radiances and reflectances first (the default layer is the first variable).
    let rank = |v: &Variable| if v.name.contains("radiance") && !v.name.contains("unc") { 0 } else if v.name.contains("reflectance") { 1 } else { 2 };
    let order: Vec<usize> = {
        let mut o: Vec<usize> = (0..vars.len()).collect();
        o.sort_by_key(|&i| (rank(&vars[i]), vars[i].name.clone()));
        o
    };
    let vars: Vec<Variable> = order.into_iter().map(|i| vars[i].clone()).collect();
    let pname = dir.file_name().map_or(String::new(), |n| n.to_string_lossy().into());
    let kind = pname.split('_').filter(|s| !s.is_empty()).skip(1).take(3).collect::<Vec<_>>().join(" ");
    let desc = format!("Sentinel-3 {kind} SAFE, {} variables", vars.len());
    Ok(Dataset { product: Product { name: pname, desc, vars, valid: None, info }, sources })
}
