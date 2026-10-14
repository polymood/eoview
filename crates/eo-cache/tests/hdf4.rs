//! HDF4 reader tests. scripts/testdata_hdf4.py writes the files with pyhdf. The value of most variables is
//! their flat index in C order: the tests compute the expected value at each index.
use eo_cache::{Engine, Event, Layer, Pick};
use eo_core::Georef;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

fn path(f: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(f).to_str().unwrap().into()
}

fn opened(rx: &Receiver<Event>) -> Arc<Layer> {
    loop {
        match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Event::Opened { res, .. } => return res.unwrap(),
            Event::Error(m) => panic!("{m}"),
            _ => {}
        }
    }
}

/// The layer of variable `name` of the file of `l`, with the index `at` of the other dimensions.
fn select(e: &Engine, rx: &Receiver<Event>, l: &Layer, name: &str, at: &[(&str, u64)]) -> Arc<Layer> {
    let var = l.ds.product.vars.iter().position(|v| v.name == name).unwrap_or_else(|| panic!("no variable {name}"));
    let pick = Pick { axes: None, at: at.iter().map(|(n, i)| (n.to_string(), *i)).collect() };
    e.select(l, var, 0, 0, pick);
    opened(rx)
}

fn probe(e: &Engine, rx: &Receiver<Event>, l: &Arc<Layer>, x: u64, y: u64) -> Option<f64> {
    e.probe(l.clone(), x, y);
    loop {
        match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Event::Probe { values, .. } => return values[0],
            Event::Error(m) => panic!("{m}"),
            _ => {}
        }
    }
}

#[test]
fn hdf4_variables_dimensions_and_attributes() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let ds = eo_io::open(&path("hdf4_sds.hdf"), rt.handle()).unwrap();
    let p = &ds.product;
    let names: Vec<&str> = p.vars.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["temperature", "Latitude", "Longitude", "counts", "calibrated", "deflated", "tiles", "plain", "growing"]);
    let v = &p.vars[3];
    assert_eq!(v.dims, ["level", "row", "col"]);
    assert_eq!(v.levels[0].dims, ["level", "y", "x"]);
    assert_eq!(v.levels[0].shape, [3, 6, 8]);
    assert!(!v.levels[0].le);
    assert_eq!((v.fill, v.scale, v.offset, v.units.as_str()), (Some(0.0), 0.5, 10.0, "W m-2"));
    // SDsetcal: value = 2 * (stored - 5).
    assert_eq!((p.vars[4].scale, p.vars[4].offset), (2.0, -10.0));
    let tiles = &p.vars[6];
    assert_eq!((tiles.levels[0].chunk.clone(), tiles.levels[0].chunks.len()), (vec![4, 5], 9));
    assert_eq!(tiles.levels[0].codecs, [eo_core::Codec::Deflate]);
    // Georeferencing: 2D latitude and longitude, and 1D latitude and longitude (a regular grid).
    assert!(matches!(&p.vars[0].georef, Georef::Arrays { lon, lat, .. } if lon == "Longitude" && lat == "Latitude"));
    assert_eq!(tiles.georef, Georef::Affine { gt: [1.75, 0.5, 0.0, 50.25, 0.0, -0.5], crs: eo_core::Crs::wgs84() });
    // The 1D variables.
    let lat = p.small.iter().find(|v| v.name == "lat").unwrap();
    assert_eq!(lat.dims, ["lat"]);
    assert_eq!(eo_io::read_all(&ds, lat).unwrap(), (0..10).map(|i| 50.0 - 0.5 * i as f64).collect::<Vec<_>>());
    // Information: global attributes, dimensions, all variables (also the one with skipping Huffman).
    let i = &p.info;
    assert!(i.attrs.contains(&("title".into(), "eoview HDF4 test".into())));
    assert!(i.attrs.contains(&("version".into(), "3".into())));
    for d in [("row", 6), ("col", 8), ("level", 3), ("step", 4), ("lat", 10)] {
        assert!(i.dims.contains(&(d.0.into(), d.1)), "dimension {d:?}");
    }
    let t = i.var("temperature").unwrap();
    assert_eq!((t.dims.clone(), t.dtype.as_str()), (vec![("row".into(), 6), ("col".into(), 8)], "f32"));
    assert!(t.attrs.contains(&("units".into(), "K".into())));
    assert!(t.attrs.contains(&("valid_range".into(), "0, 100".into())));
    assert!(i.var("huffman").is_some());
    assert!(!names.contains(&"huffman"));
}

#[test]
fn hdf4_values_through_the_engine() {
    let (e, rx) = Engine::new(1 << 28, || {});
    e.open(path("hdf4_sds.hdf"));
    let first = opened(&rx);
    assert_eq!(first.ds.product.vars[first.var].name, "temperature");
    for (x, y) in [(0, 0), (7, 0), (3, 5)] {
        assert_eq!(probe(&e, &rx, &first, x, y), Some((y * 8 + x) as f64), "temperature ({x}, {y})");
    }
    // 3D int16 with scale, offset and fill (value 0), at index 2 of `level`, then index 0.
    let flat = |k: u64, y: u64, x: u64| ((k * 6 + y) * 8 + x) as f64;
    let l = select(&e, &rx, &first, "counts", &[("level", 2)]);
    for (x, y) in [(0, 0), (5, 4)] {
        assert_eq!(probe(&e, &rx, &l, x, y), Some(flat(2, y, x) * 0.5 + 10.0), "counts ({x}, {y})");
    }
    let l = select(&e, &rx, &first, "counts", &[]);
    assert_eq!(probe(&e, &rx, &l, 0, 0), None);
    assert_eq!(probe(&e, &rx, &l, 1, 0), Some(10.5));
    let l = select(&e, &rx, &first, "calibrated", &[]);
    assert_eq!(probe(&e, &rx, &l, 2, 3), Some(2.0 * (flat(0, 3, 2) - 5.0)));
    // Deflate, chunks with deflate, chunks without compression, linked blocks.
    for name in ["deflated", "growing"] {
        let l = select(&e, &rx, &first, name, &[]);
        for (x, y) in [(0, 0), (6, 1), (7, 3)] {
            assert_eq!(probe(&e, &rx, &l, x, y), Some(flat(0, y, x)), "{name} ({x}, {y})");
        }
    }
    for name in ["tiles", "plain"] {
        let l = select(&e, &rx, &first, name, &[]);
        // Pixels in the first chunk, in a middle chunk and in the last (partial) chunk.
        for (x, y) in [(0, 0), (4, 3), (7, 5), (11, 9), (10, 8)] {
            assert_eq!(probe(&e, &rx, &l, x, y), Some((y * 12 + x) as f64), "{name} ({x}, {y})");
        }
    }
}

#[test]
fn hdf_eos_geographic_grid() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let ds = eo_io::open(&path("hdf4_eos_geo.hdf"), rt.handle()).unwrap();
    let v = &ds.product.vars[0];
    assert_eq!(v.dims, ["YDim:Geo", "XDim:Geo"]);
    assert_eq!(v.georef, Georef::Affine { gt: [10.0, 1.0, 0.0, 46.0, 0.0, -1.0], crs: eo_core::Crs::wgs84() });
    assert!(ds.product.info.attrs.iter().any(|a| a.0 == "StructMetadata.0" && a.1.contains("GCTP_GEO")));
}
