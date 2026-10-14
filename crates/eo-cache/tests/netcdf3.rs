//! NetCDF-3 files in the three formats (classic, 64-bit offset, 64-bit data). scripts/testdata_nc3.py writes
//! them. The value of each data variable is its flat index in C order.
use eo_cache::{Engine, Event, Layer, Pick};
use eo_core::{Crs, Georef};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

fn path(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name).to_str().unwrap().into()
}

fn opened(rx: &Receiver<Event>) -> Arc<Layer> {
    loop {
        if let Event::Opened { res, .. } = rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            return res.unwrap();
        }
    }
}

/// The physical value of variable `var` at pixel (x, y), time step `time` and pick `pick`.
fn value(e: &Engine, rx: &Receiver<Event>, l: &Layer, var: &str, time: u64, pick: &Pick, x: u64, y: u64) -> Option<f64> {
    let var = l.ds.product.vars.iter().position(|v| v.name == var).unwrap();
    e.select(l, var, 0, time, pick.clone());
    let l = opened(rx);
    e.probe(l, x, y);
    loop {
        if let Event::Probe { values, .. } = rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            assert_eq!(values.len(), 1);
            return values[0];
        }
    }
}

fn pick(axes: Option<[&str; 2]>, at: &[(&str, u64)]) -> Pick {
    Pick { axes: axes.map(|a| a.map(String::from)), at: at.iter().map(|(n, i)| (n.to_string(), *i)).collect() }
}

#[test]
fn netcdf3_formats() {
    for (file, kind) in [("nc3_classic.nc", "classic"), ("nc3_64bit_offset.nc", "64-bit offset"), ("nc3_64bit_data.nc", "64-bit data")] {
        let cdf5 = kind == "64-bit data";
        let (e, rx) = Engine::new(1 << 28, || {});
        e.open(path(file));
        let l = opened(&rx);
        let p = &l.ds.product;
        assert!(p.desc.contains(kind), "{file}: {}", p.desc);
        let names: Vec<&str> = p.vars.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, [&["grid", "temp", "packed"][..], if cdf5 { &["ucount"] } else { &[] }].concat(), "{file}");
        let small: Vec<&str> = p.small.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(small, [&["time", "level", "lat", "lon", "height"][..], if cdf5 { &["big"] } else { &[] }].concat(), "{file}");
        let var = |n: &str| p.vars.iter().chain(&p.small).find(|v| v.name == n).unwrap();

        // Dimensions: the file names, and the names of the engine.
        let temp = var("temp");
        assert_eq!(temp.dims, ["time", "level", "lat", "lon"]);
        assert_eq!(temp.levels[0].dims, ["time", "level", "y", "x"]);
        assert_eq!(temp.levels[0].shape, [3, 2, 3, 5]);
        assert_eq!(var("packed").levels[0].dims, ["time", "y", "x"]);
        assert_eq!(var("time").levels[0].dims, ["time"]);
        assert!(var("height").levels[0].dims.is_empty());

        // Metadata, also of the char variable (it is not data).
        assert!(p.info.attrs.contains(&("title".into(), "NetCDF-3 test".into())));
        assert!(p.info.attrs.contains(&("version".into(), "3".into())));
        assert_eq!(p.info.dims, [("time".into(), 3), ("level".into(), 2), ("lat".into(), 3), ("lon".into(), 5), ("strlen".into(), 8)]);
        let st = p.info.var("station").unwrap();
        assert_eq!((st.dtype.as_str(), st.dims.clone()), ("char", vec![("strlen".into(), 8)]));
        assert_eq!(p.info.var("packed").unwrap().dtype, "i16");

        // CF attributes, time and georeferencing.
        let packed = var("packed");
        assert_eq!((packed.scale, packed.offset, packed.fill), (0.5, 100.0, Some(-1.0)));
        assert_eq!(temp.units, "K");
        let (day, t0) = eo_core::time::cf("days since 2020-01-01").unwrap();
        assert_eq!(*temp.times, [t0, t0 + 10.0 * day, t0 + 20.0 * day]);
        assert_eq!(var("grid").georef, Georef::Affine { gt: [-5.125, 0.25, 0.0, 41.25, 0.0, -0.5], crs: Crs::wgs84() });

        // The small variables.
        let all = |n: &str| eo_io::read_all(&l.ds, var(n)).unwrap();
        assert_eq!(all("time"), [0.0, 10.0, 20.0]);
        assert_eq!(all("level"), [850.0, 500.0]);
        assert_eq!(all("lat"), [41.0, 40.5, 40.0]);
        assert_eq!(all("lon"), [-5.0, -4.75, -4.5, -4.25, -4.0]);
        assert_eq!(all("height"), [2.5]);
        if cdf5 {
            assert_eq!(all("big"), (0..5).map(|j| (1u64 << 40) as f64 + j as f64).collect::<Vec<_>>());
        }

        // Values through the engine.
        let flat = |pos: &[u64], shape: &[u64]| pos.iter().zip(shape).fold(0, |i, (p, n)| i * n + p) as f64;
        let none = Pick::default();
        for (x, y) in [(0, 0), (4, 1), (2, 2)] {
            assert_eq!(value(&e, &rx, &l, "grid", 0, &none, x, y), Some(flat(&[y, x], &[3, 5])), "{file} grid ({x}, {y})");
            // Records 0 and 2, at level 0 and level 1.
            for (t, lev) in [(0, 0), (2, 0), (2, 1)] {
                let got = value(&e, &rx, &l, "temp", t, &pick(None, &[("level", lev)]), x, y);
                assert_eq!(got, Some(flat(&[t, lev, y, x], &[3, 2, 3, 5])), "{file} temp t {t} level {lev} ({x}, {y})");
            }
            // The records of a short variable have a padding to 4 bytes.
            for t in [0, 2] {
                let got = value(&e, &rx, &l, "packed", t, &none, x, y);
                assert_eq!(got, Some(flat(&[t, y, x], &[3, 3, 5]) * 0.5 + 100.0), "{file} packed t {t} ({x}, {y})");
            }
            if cdf5 {
                assert_eq!(value(&e, &rx, &l, "ucount", 0, &none, x, y), Some(60000.0 + flat(&[y, x], &[3, 5])));
            }
        }
        assert_eq!(value(&e, &rx, &l, "packed", 1, &none, 0, 0), None, "{file}: fill value");
        // Other axes: time by longitude, at level 1 and latitude 2 (one record chunk for each row).
        let tl = pick(Some(["time", "lon"]), &[("level", 1), ("lat", 2)]);
        for (x, y) in [(0, 0), (3, 1), (4, 2)] {
            assert_eq!(value(&e, &rx, &l, "temp", 0, &tl, x, y), Some(flat(&[y, 1, 2, x], &[3, 2, 3, 5])), "{file} time x lon ({x}, {y})");
        }
    }
}

/// With only one record variable, the records have no padding.
#[test]
fn netcdf3_one_record_variable() {
    let (e, rx) = Engine::new(1 << 26, || {});
    e.open(path("nc3_onerec.nc"));
    let l = opened(&rx);
    let x = &l.ds.product.vars[0];
    assert_eq!((x.levels[0].dims.clone(), x.levels[0].shape.clone()), (vec!["y".to_string(), "x".into()], vec![3, 3]));
    assert_eq!(eo_io::read_all(&l.ds, x).unwrap(), (0..9).map(f64::from).collect::<Vec<_>>());
}
