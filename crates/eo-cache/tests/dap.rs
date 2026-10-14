//! OPeNDAP (DAP2) through the engine, with a small local server: it serves a fixed DDS and DAS and makes
//! the `.dods` answers for the known values of each variable.
use eo_cache::{Engine, Event, Pick};
use eo_core::Georef;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

const DDS: &str = "Dataset {
    Float64 time[time = 3];
    Float64 lat[lat = 300];
    Float64 lon[lon = 600];
    Grid {
      Array:
        Float32 temp[time = 3][lat = 300][lon = 600];
      Maps:
        Float64 time[time = 3];
        Float64 lat[lat = 300];
        Float64 lon[lon = 600];
    } temp;
    Int16 elev[lat = 300][lon = 600];
    Byte mask[lat = 300][lon = 600];
    Sequence {
        Int32 id;
        Float64 depth;
    } casts;
} test.nc;
";

const DAS: &str = r#"Attributes {
    time {
        String units "days since 2000-01-01";
    }
    lat {
        String units "degrees_north";
    }
    lon {
        String units "degrees_east";
    }
    temp {
        String units "K";
        String long_name "temperature; test";
    }
    elev {
        Int16 _FillValue -32768;
        Float32 scale_factor 0.5;
        Float32 add_offset 100;
    }
    NC_GLOBAL {
        String title "DAP test";
    }
    DODS_EXTRA {
        String Unlimited_Dimension "time";
    }
}
"#;

/// Type, shape and value at an index of each variable of the server.
fn var(name: &str) -> (&'static str, Vec<u64>) {
    match name {
        "time" => ("f64", vec![3]),
        "lat" => ("f64", vec![300]),
        "lon" => ("f64", vec![600]),
        "temp" => ("f32", vec![3, 300, 600]),
        "elev" => ("i16", vec![300, 600]),
        "mask" => ("u8", vec![300, 600]),
        _ => panic!("no variable {name}"),
    }
}

fn value(name: &str, i: &[u64]) -> f64 {
    match name {
        "time" => i[0] as f64,
        "lat" => 89.875 - 0.25 * i[0] as f64,
        "lon" => -179.875 + 0.25 * i[0] as f64,
        "temp" => (i[0] * 1_000_000 + i[1] * 1000 + i[2]) as f64,
        "elev" if i[0] == 0 && i[1] < 10 => -32768.0,
        "elev" => ((i[0] * 600 + i[1]) % 20000) as f64 - 10000.0,
        _ => ((i[0] + i[1]) % 256) as f64,
    }
}

/// The `.dods` answer for a constraint `name[a:1:b]...` (or `name`): the DDS, then the XDR values.
fn dods(q: &str) -> Vec<u8> {
    let q = q.replace("%5B", "[").replace("%5D", "]");
    let name = q.split('[').next().unwrap();
    let (ty, shape) = var(name);
    let ranges: Vec<(u64, u64)> = match q.contains('[') {
        true => q.split('[').skip(1).map(|r| {
            let p: Vec<u64> = r.trim_end_matches(']').split(':').map(|x| x.parse().unwrap()).collect();
            (p[0], p[2])
        }).collect(),
        false => shape.iter().map(|&n| (0, n - 1)).collect(),
    };
    let n: u64 = ranges.iter().map(|r| r.1 - r.0 + 1).product();
    let mut out = format!("{DDS}Data:\n").into_bytes();
    out.extend((n as u32).to_be_bytes());
    out.extend((n as u32).to_be_bytes());
    for j in 0..n {
        let (mut r, mut idx) = (j, vec![0; ranges.len()]);
        for k in (0..ranges.len()).rev() {
            let len = ranges[k].1 - ranges[k].0 + 1;
            idx[k] = ranges[k].0 + r % len;
            r /= len;
        }
        let v = value(name, &idx);
        match ty {
            "f64" => out.extend(v.to_be_bytes()),
            "f32" => out.extend((v as f32).to_be_bytes()),
            "i16" => out.extend((v as i32).to_be_bytes()),
            _ => out.push(v as u8),
        }
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

/// A DAP2 server on a free local port, for all the tests of the process. One request for each connection.
fn server() -> u16 {
    static PORT: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
    *PORT.get_or_init(|| {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut s = s;
                    let (mut req, mut buf) = (vec![], [0u8; 4096]);
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend(&buf[..n]),
                        }
                    }
                    let line = String::from_utf8_lossy(&req).lines().next().unwrap_or("").to_string();
                    let path = line.split(' ').nth(1).unwrap_or("");
                    let (status, body) = match path.strip_prefix("/opendap/test.nc") {
                        Some(".dds") => ("200 OK", DDS.as_bytes().to_vec()),
                        Some(".das") => ("200 OK", DAS.as_bytes().to_vec()),
                        Some(p) if p.starts_with(".dods?") => ("200 OK", dods(&p[6..])),
                        _ => ("404 Not Found", b"Error { code = 404; message = \"no such file\"; };".to_vec()),
                    };
                    let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                    let _ = s.write_all(head.as_bytes()).and_then(|_| s.write_all(&body));
                });
            }
        });
        port
    })
}

fn opened(rx: &std::sync::mpsc::Receiver<Event>) -> Arc<eo_cache::Layer> {
    loop {
        match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Event::Opened { res, .. } => return res.unwrap(),
            Event::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
}

fn probe(e: &Engine, rx: &std::sync::mpsc::Receiver<Event>, l: &Arc<eo_cache::Layer>, x: u64, y: u64) -> Option<f64> {
    e.probe(l.clone(), x, y);
    loop {
        match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
            Event::Probe { values, .. } => return values[0],
            Event::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
}

#[test]
fn dap2_variables_values_and_axes() {
    let port = server();
    let (e, rx) = Engine::new(1 << 28, || {});
    e.open(format!("dap://127.0.0.1:{port}/opendap/test.nc"));
    let l = opened(&rx);
    let p = &l.ds.product;
    let names: Vec<&str> = p.vars.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["temp", "elev", "mask"], "the sequence is not a variable");
    let small: Vec<&str> = p.small.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(small, ["time", "lat", "lon"]);

    // The grid: CF time, georeferencing from the 1D coordinates, attributes.
    let temp = &p.vars[0];
    assert_eq!(temp.dims, ["time", "lat", "lon"]);
    assert_eq!(temp.levels[0].dims, ["time", "y", "x"]);
    assert_eq!(temp.units, "K");
    assert_eq!(temp.times.len(), 3);
    assert_eq!(temp.times[2] - temp.times[0], 2.0 * 86400.0);
    assert_eq!(eo_core::time::text(temp.times[0]), "2000-01-01 00:00:00");
    assert_eq!(temp.georef, Georef::Affine { gt: [-180.0, 0.25, 0.0, 90.0, 0.0, -0.25], crs: eo_core::Crs::wgs84() });
    assert!(p.info.attrs.contains(&("title".into(), "DAP test".into())));
    assert!(p.info.attrs.contains(&("DODS_EXTRA.Unlimited_Dimension".into(), "time".into())));
    assert!(p.info.var("temp").unwrap().attrs.contains(&("long_name".into(), "temperature; test".into())));
    assert_eq!(p.info.dims, [("time".to_string(), 3), ("lat".to_string(), 300), ("lon".to_string(), 600)]);

    // Exact values: a pixel in an edge chunk, at time step 0, then at time step 2.
    assert_eq!(probe(&e, &rx, &l, 550, 280), Some(280_550.0));
    e.select(&l, 0, 0, 2, Pick::default());
    let l2 = opened(&rx);
    assert_eq!(probe(&e, &rx, &l2, 3, 1), Some(2_001_003.0));

    // Other axes: time as the rows and lon as the columns, at lat 123.
    let pick = Pick { axes: Some(["time".into(), "lon".into()]), at: vec![("lat".into(), 123)] };
    e.select(&l, 0, 0, 0, pick.clone());
    let l3 = opened(&rx);
    assert_eq!(l3.size(), (600, 3));
    assert_eq!(l3.pick(), pick);
    assert_eq!(probe(&e, &rx, &l3, 570, 2), Some(2_123_570.0));

    // Int16 (4 bytes in XDR) with scale, offset and fill value.
    e.select(&l, 1, 0, 0, Pick::default());
    let l4 = opened(&rx);
    assert_eq!(probe(&e, &rx, &l4, 599, 299), Some(((299 * 600 + 599) % 20000 - 10000) as f64 * 0.5 + 100.0));
    assert_eq!(probe(&e, &rx, &l4, 5, 0), None);
}

#[test]
fn dap2_coordinates_and_bytes_with_read_all() {
    let port = server();
    let rt = tokio::runtime::Runtime::new().unwrap();
    // A server path with "/opendap/" that answers ".dds" is OPeNDAP without a mark.
    let ds = eo_io::open(&format!("http://127.0.0.1:{port}/opendap/test.nc.html"), rt.handle()).unwrap();
    let find = |n: &str| ds.product.small.iter().chain(&ds.product.vars).find(|v| v.name == n).unwrap();
    let lat = eo_io::read_all(&ds, find("lat")).unwrap();
    assert_eq!(lat.len(), 300);
    assert_eq!((lat[0], lat[299]), (89.875, 89.875 - 0.25 * 299.0));
    // Byte arrays have their own XDR encoding (1 byte for each value). The edge chunk is padded.
    let mask = eo_io::read_all(&ds, find("mask")).unwrap();
    for (y, x) in [(0, 0), (17, 511), (299, 599), (100, 512)] {
        assert_eq!(mask[y * 600 + x], ((x + y) % 256) as f64, "mask ({x}, {y})");
    }
    // A marked URL that does not answer is an error.
    assert!(eo_io::open(&format!("dap://127.0.0.1:{port}/opendap/none.nc"), rt.handle()).is_err());
}
