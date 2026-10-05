//! Reference tests. scripts/make_testdata.py writes the files and the expected values with
//! tifffile, numpy, GDAL, netCDF4 and zarr-python. These tests compare the values that eoview reads.
//!
//! Real products: set EOVIEW_TEST_PRODUCTS to a directory of products, and run
//! scripts/reference_products.py first (it writes expected_products.tsv in that directory).
use eo_cache::pixels::{Part, PlaneAt, value_f64};
use eo_cache::{Engine, Event, LevelSrc, Pixels, TILE, TileKey, chunk_at};
use eo_core::{Georef, Variable};
use eo_io::Dataset;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

fn lines_of(p: &Path) -> Vec<Vec<String>> {
    let t = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    t.lines().filter(|l| !l.is_empty()).map(|l| l.split('\t').map(String::from).collect()).collect()
}

fn lines() -> Vec<Vec<String>> {
    lines_of(&dir().join("expected.tsv"))
}

fn part(s: &str) -> (u64, Part) {
    let (b, p) = s.split_once(':').unwrap_or((s, ""));
    let p = match p {
        "I" => Part::I,
        "Q" => Part::Q,
        "Amp" => Part::Amp,
        _ => Part::Real,
    };
    (b.parse().unwrap(), p)
}

/// Variable with the name `n` (or a name that ends with "/n").
fn find<'a>(p: &'a eo_core::Product, n: &str) -> &'a Variable {
    p.vars.iter().find(|v| v.name == n || v.name.ends_with(&format!("/{n}"))).unwrap_or_else(|| panic!("no variable {n}"))
}

/// Stored value at pixel (x, y) of a file level, read through the chunk table and the codecs.
fn read(ds: &Dataset, v: &Variable, level: usize, band: u64, p: Part, x: u64, y: u64) -> f64 {
    let a = &v.levels[level];
    let (ch, cw) = (a.chunk[a.axis("y").unwrap()], a.chunk[a.axis("x").unwrap()]);
    let c = a.chunks[chunk_at(a, band, y / ch, x / cw)];
    let s = &ds.sources[c.src as usize];
    let raw = s.read_chunk(c).unwrap();
    // A chunk that was not written: all values are the fill value.
    if raw.is_empty() {
        return v.fill.unwrap_or(0.0);
    }
    let d = eo_io::codec::decode(a, &raw).unwrap();
    let pa = PlaneAt::new(a, band);
    value_f64(a, &d, pa.base + (y % ch) as usize * pa.sy + (x % cw) as usize * pa.sx, p)
}

fn close(got: f64, exp: f64, rel: f64) -> bool {
    (got.is_nan() && exp.is_nan()) || (got - exp).abs() <= exp.abs() * rel + 1e-9
}

/// Check the lines s, v, g and p (reader level: chunk table, codecs, attributes). Return the number of checks.
fn check_values(dir: &Path, lines: &[Vec<String>]) -> usize {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut open: HashMap<String, Dataset> = HashMap::new();
    let mut n = 0;
    for f in lines {
        let (file, var) = f[1].split_once('#').unwrap_or((&f[1], ""));
        let ds = open.entry(file.to_string()).or_insert_with(|| {
            eo_io::open(dir.join(file).to_str().unwrap(), rt.handle()).unwrap_or_else(|e| panic!("{file}: {e}"))
        });
        let v = if var.is_empty() { &ds.product.vars[0] } else { find(&ds.product, var) };
        let num = |i: usize| f[i].parse::<f64>().unwrap();
        match f[0].as_str() {
            "s" => {
                let a = &v.levels[0];
                let got = (a.len_of("x"), a.len_of("y"), v.bands.len(), v.levels.len(), format!("{:?}", a.dtype));
                let exp = (num(2) as u64, num(3) as u64, num(4) as usize, num(5) as usize, f[6].clone());
                assert_eq!(got, exp, "{}", f[1]);
            }
            "v" => {
                let (band, p) = part(&f[3]);
                let got = read(ds, v, num(2) as usize, band, p, num(4) as u64, num(5) as u64);
                let ok = match f.get(7) {
                    Some(t) => (got - num(6)).abs() <= t.parse::<f64>().unwrap(),
                    None => close(got, num(6), 1e-6),
                };
                assert!(ok, "{:?}: got {got}", f);
            }
            "g" => {
                let Georef::Affine { gt, crs } = &v.georef else { panic!("{}: no georeferencing", f[1]) };
                let exp: Vec<f64> = f[2].split(' ').map(|s| s.parse().unwrap()).collect();
                assert!(gt.iter().zip(&exp).all(|(g, e)| close(*g, *e, 1e-12)), "{}: {gt:?} {exp:?}", f[1]);
                assert_eq!(crs.epsg, Some(num(3) as u32), "{}", f[1]);
            }
            "p" => assert_eq!((v.scale, v.offset, v.fill), (num(2), num(3), Some(num(4))), "{}", f[1]),
            _ => continue,
        }
        n += 1;
    }
    n
}

#[test]
fn readers_match_reference() {
    let n = check_values(&dir(), &lines());
    assert!(n > 150, "only {n} checks");
}

/// Real products (EOVIEW_TEST_PRODUCTS). No check if the variable is not set.
#[test]
fn products_match_reference() {
    let Some(d) = std::env::var_os("EOVIEW_TEST_PRODUCTS").map(PathBuf::from) else { return };
    let all = lines_of(&d.join("expected_products.tsv"));
    let n = check_values(&d, &all);
    let g = check_geo(&d, &all);
    println!("{n} value checks, {g} position checks");
    assert!(n > 50);
}

/// Check the lines l (geolocation): the engine reads the geolocation arrays or grids. Return the number of checks.
fn check_geo(dir: &Path, lines: &[Vec<String>]) -> usize {
    let (e, rx) = Engine::new(1 << 30, || {});
    let mut n = 0;
    for f in lines.iter().filter(|f| f[0] == "l") {
        let (file, var) = f[1].split_once('#').unwrap();
        let l = layer(&e, &rx, &dir.join(file));
        let vi = l.ds.product.vars.iter().position(|v| v.name == var).unwrap_or_else(|| panic!("no variable {var}"));
        e.select(&l, vi, 0);
        let lv = loop {
            if let Event::Opened { res, .. } = rx.recv_timeout(Duration::from_secs(60)).unwrap() {
                break res.unwrap();
            }
        };
        let g = e.georef(&lv).unwrap();
        let num = |i: usize| f[i].parse::<f64>().unwrap();
        let (lon, lat) = g.map(num(2), num(3)).unwrap();
        assert!((lon - num(4)).abs() <= num(6) && (lat - num(5)).abs() <= num(6), "{f:?}: got {lon} {lat}");
        n += 1;
    }
    n
}

/// Wait for the complete tiles `keys` of `layer`.
fn tiles(e: &Engine, rx: &std::sync::mpsc::Receiver<Event>, l: &Arc<eo_cache::Layer>, keys: &[TileKey]) -> HashMap<TileKey, (u32, Arc<Pixels>)> {
    e.want(0, keys.iter().map(|k| (l.clone(), *k)).collect());
    let mut got = HashMap::new();
    while got.len() < keys.len() {
        match rx.recv_timeout(Duration::from_secs(20)).expect("tile timeout") {
            Event::Tile { key, w, px, done: true, .. } => drop(got.insert(key, (w, px))),
            Event::Error(m) => panic!("{m}"),
            _ => {}
        }
    }
    got
}

fn layer(e: &Engine, rx: &std::sync::mpsc::Receiver<Event>, path: &Path) -> Arc<eo_cache::Layer> {
    e.open(path.to_str().unwrap().into());
    loop {
        if let Event::Opened { res, .. } = rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            return res.unwrap();
        }
    }
}

#[test]
fn engine_tiles_match_reference() {
    let (e, rx) = Engine::new(1 << 30, || {});
    let all = lines();

    // Generated display levels: mean of the level-0 pixels.
    let l = layer(&e, &rx, &dir().join("virt_u8.tif"));
    assert!(l.enc.u8);
    let m: Vec<&Vec<String>> = all.iter().filter(|f| f[0] == "m").collect();
    let key = |f: &Vec<String>| {
        let (d, x, y) = (f[2].parse::<u8>().unwrap(), f[3].parse::<u64>().unwrap(), f[4].parse::<u64>().unwrap());
        assert!(matches!(l.levels[d as usize].src, LevelSrc::Virtual { .. }));
        (TileKey { layer: l.id, lv: d, tx: (x / TILE) as u32, ty: (y / TILE) as u32 }, x % TILE, y % TILE)
    };
    let mut keys: Vec<TileKey> = m.iter().map(|f| key(f).0).collect();
    let mut seen = std::collections::HashSet::new();
    keys.retain(|k| seen.insert(*k));
    let got = tiles(&e, &rx, &l, &keys);
    for f in &m {
        let (k, x, y) = key(f);
        let (w, px) = &got[&k];
        let Pixels::U8(v) = &**px else { panic!("u8 tile expected") };
        let exp: f64 = f[5].parse().unwrap();
        let g = v[(y * *w as u64 + x) as usize] as f64;
        assert!((g - exp).abs() <= 0.5 + 1e-9, "{f:?}: got {g}");
    }

    // File overview through the f16 display path of the engine.
    let l = layer(&e, &rx, &dir().join("cog_u16.tif"));
    let top = l.levels.len() - 1;
    let LevelSrc::File(lvl) = l.levels[top].src else { panic!("top level of a COG is a file level") };
    let k = TileKey { layer: l.id, lv: top as u8, tx: 0, ty: 0 };
    let got = tiles(&e, &rx, &l, &[k]);
    let (w, px) = &got[&k];
    let Pixels::F16(v) = &**px else { panic!("f16 tile expected") };
    let mut n = 0;
    for f in all.iter().filter(|f| f[0] == "v" && f[1] == "cog_u16.tif" && f[2] == lvl.to_string()) {
        let (x, y, exp) = (f[4].parse::<usize>().unwrap(), f[5].parse::<usize>().unwrap(), f[6].parse::<f64>().unwrap());
        let t = v[y * *w as usize + x].to_f32();
        if exp == 0.0 {
            assert!(t.is_nan(), "{f:?}: fill value must be NaN, got {t}");
        } else {
            let g = (t / l.enc.k + l.enc.off) as f64;
            assert!(close(g, exp, 2e-3), "{f:?}: got {g}");
        }
        n += 1;
    }
    assert!(n >= 6);

    assert!(check_geo(&dir(), &all) >= 6);
}
