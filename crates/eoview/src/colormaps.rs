//! The color maps: the library of eoview (`assets/colormaps.json`, made by `scripts/make_colormaps.py` from
//! matplotlib, ColorBrewer, cmocean and the Scientific colour maps of Crameri), then the maps of the user.
//! A map of the user is a JSON file in the `colormaps` directory of the configuration: an imported file
//! (GMT .cpt, Adobe .act, NCL .rgb, GIMP .gpl and .ggr, JASC .pal, a text file of RGB rows) or a map of the
//! editor. A layer keeps the name of its map, and the index in this list while eoview runs.
use std::sync::{Arc, RwLock};

#[derive(Clone, Debug, PartialEq)]
pub struct Cmap {
    pub name: String,
    /// The group of the menu: Perceptual, Sequential, Diverging, Cyclic, Qualitative, ..., User.
    pub group: String,
    pub source: String,
    pub stops: Vec<[u8; 3]>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CmapFile {
    name: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    source: String,
    /// Colors as hexadecimal RGB, separated by commas.
    stops: String,
}

static LIB: RwLock<Option<Arc<Vec<Cmap>>>> = RwLock::new(None);

fn hexes(s: &str) -> Vec<[u8; 3]> {
    s.split(',').filter_map(|h| u32::from_str_radix(h.trim().trim_start_matches('#'), 16).ok()).map(crate::layer::hex).collect()
}

fn from_file(f: CmapFile, user: bool) -> Option<Cmap> {
    let stops = hexes(&f.stops);
    (stops.len() >= 2).then(|| Cmap { name: f.name, group: if user { "User".into() } else { f.group }, source: f.source, stops })
}

/// The directory of the maps of the user.
pub fn user_dir() -> Option<std::path::PathBuf> {
    crate::prefs::config_dir().map(|d| d.join("colormaps"))
}

/// Read the library again: the maps of eoview, then the maps of the user (a map of the user with the
/// name of a map of eoview gets the suffix " (user)").
pub fn reload() {
    let built: Vec<CmapFile> = serde_json::from_str(include_str!("../assets/colormaps.json")).unwrap_or_default();
    let mut all: Vec<Cmap> = built.into_iter().filter_map(|f| from_file(f, false)).collect();
    let mut files: Vec<_> = user_dir().and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    for p in files {
        let Some(mut m) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<CmapFile>(&s).ok()).and_then(|f| from_file(f, true)) else { continue };
        if all.iter().any(|x| x.name == m.name) {
            m.name += " (user)";
        }
        all.push(m);
    }
    *LIB.write().unwrap() = Some(Arc::new(all));
}

/// All the color maps.
pub fn all() -> Arc<Vec<Cmap>> {
    if let Some(l) = LIB.read().unwrap().as_ref() {
        return l.clone();
    }
    reload();
    LIB.read().unwrap().clone().unwrap()
}

/// The index of the map of this name (letter case does not matter).
pub fn find(name: &str) -> Option<usize> {
    all().iter().position(|c| c.name.eq_ignore_ascii_case(name))
}

/// The colors of map `i` (the first map if `i` is not a map).
pub fn stops(i: usize) -> Vec<[u8; 3]> {
    let a = all();
    a.get(i).unwrap_or(&a[0]).stops.clone()
}

/// The name of map `i`.
pub fn name(i: usize) -> String {
    all().get(i).map_or(String::new(), |c| c.name.clone())
}

/// Save a map of the user (a new file, or the file of a map of the same name), and read the library again.
pub fn save_user(name: &str, stops: &[[u8; 3]], source: &str) -> Result<(), String> {
    let dir = user_dir().ok_or("no configuration directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let f = CmapFile { name: name.trim().into(), group: "User".into(), source: source.into(), stops: stops.iter().map(|c| format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2])).collect::<Vec<_>>().join(",") };
    std::fs::write(dir.join(format!("{file}.json")), serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    reload();
    Ok(())
}

/// Delete a map of the user, and read the library again.
pub fn delete_user(name: &str) {
    if let Some(dir) = user_dir() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<CmapFile>(&s).ok()).is_some_and(|f| f.name == name.trim_end_matches(" (user)")) {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    reload();
}

/// The colors of a color map file. The format comes from the extension: GMT `.cpt` (the colors of the
/// slices; RGB, R/G/B or HSV-free files), Adobe `.act` (binary, 256 colors), GIMP `.gpl` (palette) and `.ggr`
/// (gradient: the colors at the ends of the segments), JASC `.pal`, NCL `.rgb` and the other text files:
/// rows of 3 numbers (0 to 1, or 0 to 255), lines with `#` or `;` are comments.
pub fn parse(path: &str, data: &[u8]) -> Result<Vec<[u8; 3]>, String> {
    let ext = std::path::Path::new(path).extension().map_or(String::new(), |e| e.to_string_lossy().to_lowercase());
    if ext == "act" {
        let n = if data.len() >= 772 { u16::from_be_bytes([data[768], data[769]]).clamp(2, 256) as usize } else { data.len() / 3 };
        let c: Vec<[u8; 3]> = data.chunks_exact(3).take(n.min(256)).map(|c| [c[0], c[1], c[2]]).collect();
        return if c.len() >= 2 { Ok(c) } else { Err("an .act file has 768 bytes or more".into()) };
    }
    let text = String::from_utf8_lossy(data);
    let nums = |l: &str| -> Vec<f64> { l.split(|c: char| c.is_whitespace() || c == ',' || c == '/' || c == ';').filter_map(|x| x.parse().ok()).collect() };
    let to8 = |v: &[f64], unit: bool| -> [u8; 3] { [0, 1, 2].map(|k| (if unit { v[k] * 255.0 } else { v[k] }).round().clamp(0.0, 255.0) as u8) };
    let mut out: Vec<[u8; 3]> = vec![];
    match ext.as_str() {
        "cpt" => {
            // Slice lines: z0 r g b z1 r g b (or z0 r/g/b z1 r/g/b). B, F and N lines are not slices.
            for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(['B', 'F', 'N'])) {
                let v = nums(l);
                if v.len() >= 8 {
                    out.push(to8(&v[1..4], false));
                    out.push(to8(&v[5..8], false));
                }
            }
            out.dedup();
        }
        "ggr" => {
            // GIMP gradient: a segment has left, middle, right, then the RGBA of the left and of the right end.
            for l in text.lines().skip(3) {
                let v = nums(l);
                if v.len() >= 11 {
                    out.push(to8(&v[3..6], true));
                    out.push(to8(&v[7..10], true));
                }
            }
            out.dedup();
        }
        _ => {
            let rows: Vec<Vec<f64>> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with(['#', ';']) && !l.starts_with("GIMP") && !l.starts_with("Name:") && !l.starts_with("Columns:") && !l.starts_with("JASC") && !l.starts_with("ncolors"))
                .map(nums)
                .filter(|v| v.len() >= 3)
                .collect();
            // JASC .pal: a version line (0100) and a count line before the colors: rows of 1 number are not
            // colors. A file of values from 0 to 1: unit values.
            let unit = rows.iter().all(|v| v[..3].iter().all(|x| (0.0..=1.0).contains(x)));
            out = rows.iter().map(|v| to8(&v[..3], unit)).collect();
        }
    }
    if out.len() < 2 { Err(format!("{path}: no colors (at least 2)")) } else { Ok(out) }
}

/// At most `n` colors of `c`, evenly spaced (a large imported table, for the stops of a layer).
pub fn thin(c: Vec<[u8; 3]>, n: usize) -> Vec<[u8; 3]> {
    if c.len() <= n {
        return c;
    }
    (0..n).map(|k| c[k * (c.len() - 1) / (n - 1)]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_and_formats() {
        let a = all();
        assert!(a.len() >= 100, "{}", a.len());
        // The maps of the old fixed list keep their names, and Viridis keeps index 1 (the wind layers).
        assert_eq!([&a[0].name, &a[1].name, &a[11].name], ["Gray", "Viridis", "RdYlGn"]);
        assert!(find("batlow").is_some() && find("thermal").is_some() && find("tab10").is_some());
        let cpt = b"# a GMT table\n0 0 0 0 10 255 0 0\n10 255 0 0 20 255 255 255\nB 0 0 0\nF 255 255 255\n";
        assert_eq!(parse("x.cpt", cpt).unwrap(), [[0, 0, 0], [255, 0, 0], [255, 255, 255]]);
        assert_eq!(parse("x.rgb", b"ncolors=2\n# r g b\n0.0 0.0 1.0\n1.0 0.5 0.0\n").unwrap(), [[0, 0, 255], [255, 128, 0]]);
        assert_eq!(parse("x.txt", b"0 0 255\n255 128 0\n").unwrap(), [[0, 0, 255], [255, 128, 0]]);
        assert_eq!(parse("x.gpl", b"GIMP Palette\nName: x\nColumns: 0\n#\n  0   0 255 blue\n255 128   0 orange\n").unwrap(), [[0, 0, 255], [255, 128, 0]]);
        let mut act = vec![0u8; 772];
        act[3..6].copy_from_slice(&[255, 255, 255]);
        act[769] = 2;
        assert_eq!(parse("x.act", &act).unwrap(), [[0, 0, 0], [255, 255, 255]]);
        assert_eq!(thin((0..=255u8).map(|v| [v, v, v]).collect(), 3), [[0, 0, 0], [127, 127, 127], [255, 255, 255]]);
    }
}
