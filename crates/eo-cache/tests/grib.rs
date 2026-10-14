//! GRIB tests. scripts/testdata_grib.py writes the files and the values that ecCodes decodes
//! (testdata/expected_grib.tsv). These tests compare the values and the positions that eoview reads.
use eo_cache::{Engine, Event, Pick};
use eo_core::{Product, Variable};
use eo_io::Dataset;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
}

fn lines() -> Vec<Vec<String>> {
    let t = std::fs::read_to_string(dir().join("expected_grib.tsv")).unwrap();
    t.lines().filter(|l| !l.is_empty()).map(|l| l.split('\t').map(String::from).collect()).collect()
}

fn var<'a>(p: &'a Product, n: &str) -> (usize, &'a Variable) {
    p.vars.iter().enumerate().find(|v| v.1.name == n).unwrap_or_else(|| panic!("no variable {n} in {:?}", p.vars.iter().map(|v| &v.name).collect::<Vec<_>>()))
}

/// Index of the time, level and member of a line in the dimensions of `v` (the level and member values
/// are in the attributes of the variable).
fn index(p: &Product, v: &Variable, f: &[String]) -> HashMap<&'static str, u64> {
    let meta = p.info.var(&v.name).unwrap();
    let attr = |k: &str| meta.attrs.iter().find(|a| a.0 == k).map(|a| a.1.split(' ').map(|x| x.parse::<f64>().unwrap()).collect::<Vec<_>>()).unwrap_or_default();
    let pos = |vals: &[f64], x: &str| {
        let x = x.parse::<f64>().unwrap();
        // One value: no dimension. Else the value must be a coordinate.
        if vals.len() == 1 { assert_eq!(vals[0], x, "{f:?}") }
        vals.iter().position(|v| *v == x).map_or_else(|| { assert!(vals.len() <= 1, "{f:?}: {x} not in {vals:?}"); 0 }, |i| i as u64)
    };
    let mut at = HashMap::new();
    at.insert("time", if v.times.is_empty() { 0 } else { pos(&v.times, &f[3]) });
    at.insert("level", if f[4] == "-" { 0 } else { pos(&attr("level"), &f[4]) });
    at.insert("member", if f[5] == "-" { 0 } else { pos(&attr("member"), &f[5]) });
    at
}

fn close(got: f64, exp: f64, tol: f64) -> bool {
    (got.is_nan() && exp.is_nan()) || (got - exp).abs() <= tol
}

/// Variables, dimensions, values of each packing, bitmap, georeferencing and times, at the reader level.
#[test]
fn grib_matches_eccodes() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut open: HashMap<String, (Dataset, HashMap<String, Vec<f64>>)> = HashMap::new();
    let (mut nv, mut nl) = (0, 0);
    let mut packings = std::collections::HashSet::new();
    for f in lines() {
        let (ds, all) = open.entry(f[1].clone()).or_insert_with(|| (eo_io::open(dir().join(&f[1]).to_str().unwrap(), rt.handle()).unwrap_or_else(|e| panic!("{}: {e}", f[1])), HashMap::new()));
        let (_, v) = var(&ds.product, &f[2]);
        let num = |i: usize| f[i].parse::<f64>().unwrap();
        match f[0].as_str() {
            "s" => {
                let a = &v.levels[0];
                assert_eq!((v.dims.join(","), a.shape.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",")), (f[3].clone(), f[4].clone()), "{f:?}");
                assert_eq!(a.dims.iter().filter(|d| *d == "y" || *d == "x").count(), 2);
            }
            "v" => {
                let vals = all.entry(v.name.clone()).or_insert_with(|| eo_io::read_all(ds, v).unwrap());
                let at = index(&ds.product, v, &f);
                let a = &v.levels[0];
                let i = a.dims.iter().zip(&a.shape).fold(0, |i, (d, n)| {
                    let k = match d.as_str() {
                        "x" => num(6) as u64,
                        "y" => num(7) as u64,
                        d => at[d],
                    };
                    i * n + k
                });
                let got = vals[i as usize];
                assert!(close(got, num(8), num(8).abs() * 1e-6 + 1e-9), "{f:?}: got {got}");
                assert!(close(got, num(9), num(10) * 1.001 + num(9).abs() * 1e-7), "{f:?}: got {got}, exact {}", num(9));
                if !v.times.is_empty() {
                    assert_eq!(v.times[at["time"] as usize], num(3), "{f:?}");
                }
                packings.insert(ds.product.info.var(&v.name).unwrap().attrs.iter().find(|a| a.0 == "packing").unwrap().1.clone());
                nv += 1;
            }
            "l" => {
                let (lon, lat) = v.georef.map(num(3) + 0.5, num(4) + 0.5).unwrap_or_else(|| panic!("{f:?}: no georeferencing"));
                assert!(((lon - num(5)).rem_euclid(360.0) + 1e-6) % 360.0 < 2e-6 && (lat - num(6)).abs() < 1e-6, "{f:?}: got {lon} {lat}");
                nl += 1;
            }
            _ => {}
        }
    }
    assert!(nv > 100 && nl > 100, "{nv} values, {nl} positions");
    // GRIB 2: simple, complex, complex with spatial differencing, JPEG 2000, CCSDS, IEEE and PNG. GRIB 1: simple.
    assert_eq!(packings.len(), 8, "{packings:?}");
}

/// The engine reads a time step and a level of the cube (a pick), and a bitmap point is no data.
#[test]
fn grib_through_the_engine() {
    let (e, rx) = Engine::new(1 << 28, || {});
    let opened = |req: Option<u64>| loop {
        if let Event::Opened { req: r, res } = rx.recv_timeout(Duration::from_secs(20)).unwrap()
            && req.is_none_or(|q| q == r)
        {
            break res.unwrap();
        }
    };
    let probe = |l: &std::sync::Arc<eo_cache::Layer>, x: u64, y: u64| {
        e.probe(l.clone(), x, y);
        loop {
            if let Event::Probe { values, .. } = rx.recv_timeout(Duration::from_secs(20)).unwrap() {
                break values[0];
            }
        }
    };
    let all = lines();
    let mut n = 0;
    for file in ["grib2_cube.grib2", "grib2_ens.grib2", "grib2_ccsds.grib2", "grib1.grib"] {
        e.open(dir().join(file).to_str().unwrap().into());
        let first = opened(None);
        for f in all.iter().filter(|f| f[0] == "v" && f[1] == file) {
            let (vi, v) = var(&first.ds.product, &f[2]);
            let at = index(&first.ds.product, v, f);
            let mut pick = Pick::default();
            for d in ["level", "member"] {
                if v.dims.iter().any(|x| x == d) {
                    pick.at.push((d.into(), at[d]));
                }
            }
            let req = e.select(&first, vi, 0, at["time"], pick);
            let l = opened(Some(req));
            let got = probe(&l, f[6].parse().unwrap(), f[7].parse().unwrap());
            let exp: f64 = f[8].parse().unwrap();
            match got {
                None => assert!(exp.is_nan(), "{f:?}: no data"),
                Some(g) => assert!((g - exp).abs() <= exp.abs() * 1e-6, "{f:?}: got {g}"),
            }
            n += 1;
        }
    }
    assert!(n > 50);
}
