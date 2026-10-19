//! OPeNDAP servers, protocol DAP2 (Hyrax, THREDDS, ERDDAP, pydap). The DDS gives the variables and the DAS
//! gives their attributes. Each array or grid with 2 or more dimensions is a variable. A chunk of a variable
//! is one `.dods` request with a constraint expression (`var[a:1:b][c:1:d]`): the server sends only that
//! hyperslab. The chunks are 1 along the leading dimensions and blocks along the last two.
//!
//! Not read: sequences (tables), strings, and DAP4. The members of a structure are variables with the name
//! of the structure and a dot first.
//!
//! Specification: DAP 2.0, <https://www.opendap.org/pdf/ESE-RFC-004v1.2.pdf>.
use crate::{Dataset, Source};
use bytes::Bytes;
use eo_core::*;
use object_store::client::{HttpClient, HttpConnector, HttpRequest, HttpRequestBody, SpawnedReqwestConnector};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::runtime::Handle;

/// The dataset URL of an OPeNDAP URL, and true if the user marks it as OPeNDAP: a `dap://` or `dap2://` URL
/// (HTTP), or `opendap:` before the URL. An HTTP(S) URL with `/dodsC/`, `/opendap/` or `/dap/` in its path
/// can be OPeNDAP (false: the caller tries it). The suffix of a DAP response (`.html`, `.dds`, ...) goes away.
pub fn url(u: &str) -> Option<(String, bool)> {
    let marked = match u.strip_prefix("dap://").or(u.strip_prefix("dap2://")) {
        Some(r) => Some(format!("http://{r}")),
        None => u.strip_prefix("opendap:").map(String::from),
    };
    let path = u.split(['?', '#']).next().unwrap_or(u);
    let (mut u, marked) = match marked {
        Some(m) => (m, true),
        None if u.starts_with("http") && ["/dodsC/", "/opendap/", "/dap/"].iter().any(|p| path.contains(p)) => (path.to_string(), false),
        None => return None,
    };
    for s in [".html", ".dmr", ".dds", ".das", ".dods", ".info"] {
        if let Some(b) = u.strip_suffix(s) {
            u = b.to_string();
        }
    }
    Some((u, marked))
}

/// An array of the DDS. `name` is the path for a constraint (`grid`, `structure.member`, `grid.map`).
struct Var {
    name: String,
    ty: String,
    dims: Vec<(String, u64)>,
    /// A map of a grid.
    map: bool,
}

/// Words of a DDS or a DAS: the signs `{ } [ ] ; = ,` alone, the other runs of characters, and the quoted
/// strings with a '"' first (and without their escapes), so that a quoted ";" is not a sign.
fn tokens(s: &str) -> Vec<String> {
    let (mut out, mut c) = (vec![], s.chars().peekable());
    while let Some(ch) = c.next() {
        match ch {
            _ if ch.is_whitespace() => {}
            '{' | '}' | '[' | ']' | ';' | '=' | ',' => out.push(ch.to_string()),
            '"' => {
                let mut t = String::from('"');
                while let Some(ch) = c.next() {
                    match ch {
                        '"' => break,
                        '\\' => t.extend(c.next()),
                        _ => t.push(ch),
                    }
                }
                out.push(t);
            }
            _ => {
                let mut t = ch.to_string();
                while let Some(&ch) = c.peek().filter(|ch| !ch.is_whitespace() && !"{}[];=,\"".contains(**ch)) {
                    t.push(ch);
                    c.next();
                }
                out.push(t);
            }
        }
    }
    out
}

type Words<'a> = std::slice::Iter<'a, String>;

fn next<'a>(t: &mut Words<'a>) -> Result<&'a str> {
    t.next().map(String::as_str).ok_or_else(|| "DDS or DAS: unexpected end".into())
}

/// The declarations of a DDS up to the closing brace. The members of a grid come back as the array and
/// then its maps. A sequence is not data: it is skipped. `depth`: the level of nesting (limited: the server
/// sends the text).
fn decls(t: &mut Words, out: &mut Vec<Var>, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err("DDS: too many nested structures".into());
    }
    loop {
        let ty = next(t)?;
        match ty.to_lowercase().as_str() {
            "}" => return Ok(()),
            // The "Array:" and "Maps:" labels of a grid.
            l if l.ends_with(':') => {}
            kind @ ("grid" | "structure" | "sequence") => {
                let kind = kind.to_string();
                next(t)?; // {
                let mut inner = vec![];
                decls(t, &mut inner, depth + 1)?;
                let name = next(t)?;
                next(t)?; // ;
                match kind.as_str() {
                    "sequence" => eprintln!("OPeNDAP: sequence {name} is not read (not an array)"),
                    "grid" if !inner.is_empty() => {
                        let a = inner.remove(0);
                        out.push(Var { name: name.into(), ..a });
                        out.extend(inner.into_iter().map(|m| Var { name: format!("{name}.{}", m.name), map: true, ..m }));
                    }
                    _ => out.extend(inner.into_iter().map(|m| Var { name: format!("{name}.{}", m.name), ..m })),
                }
            }
            _ => {
                let name = next(t)?.to_string();
                let mut dims = vec![];
                while next(t)? == "[" {
                    let w: Vec<&str> = std::iter::from_fn(|| t.next().map(String::as_str)).take_while(|w| *w != "]").collect();
                    let n = w.last().and_then(|n| n.parse().ok()).ok_or_else(|| Error(format!("DDS: bad dimension of {name}")))?;
                    dims.push((if w.len() == 3 { w[0].to_string() } else { format!("dim_{}", dims.len()) }, n));
                }
                out.push(Var { name, ty: ty.into(), dims, map: false });
            }
        }
    }
}

/// The arrays of a DDS. A map of a grid comes only if the dataset does not have it as a variable (a
/// coordinate), and one time for all grids.
fn dds(text: &str) -> Result<Vec<Var>> {
    let w = tokens(text);
    let mut t = w.iter();
    if !next(&mut t)?.eq_ignore_ascii_case("dataset") || next(&mut t)? != "{" {
        return Err("not a DDS (no Dataset)".into());
    }
    let mut all = vec![];
    decls(&mut t, &mut all, 0)?;
    let top: Vec<String> = all.iter().filter(|v| !v.map).map(|v| v.name.clone()).collect();
    let mut seen = std::collections::HashSet::new();
    all.retain(|v| !v.map || (!top.iter().any(|n| n == leaf(&v.name)) && seen.insert(leaf(&v.name).to_string())));
    Ok(all)
}

fn leaf(n: &str) -> &str {
    n.rsplit('.').next().unwrap_or(n)
}

/// The attributes of each container of a DAS, by path (`var`, `struct.var`, `NC_GLOBAL`). Values as text,
/// with ", " between values. The attributes outside a container have the path "".
fn das(text: &str) -> HashMap<String, Attrs> {
    fn body(t: &mut Words, path: &str, out: &mut HashMap<String, Attrs>, depth: usize) -> Result<()> {
        if depth > 64 {
            return Err("DAS: too many nested containers".into());
        }
        loop {
            let a = next(t)?;
            if a == "}" {
                return Ok(());
            }
            let b = next(t)?;
            if b == "{" {
                body(t, &if path.is_empty() { a.to_string() } else { format!("{path}.{a}") }, out, depth + 1)?;
                continue;
            }
            let mut vals = vec![];
            loop {
                match next(t)? {
                    ";" => break,
                    "," => {}
                    v => vals.push(v.strip_prefix('"').unwrap_or(v)),
                }
            }
            out.entry(path.to_string()).or_default().push((b.to_string(), vals.join(", ")));
        }
    }
    let w = tokens(text);
    let mut t = w.iter();
    let mut out = HashMap::new();
    if next(&mut t).is_ok_and(|a| a.eq_ignore_ascii_case("attributes")) && next(&mut t).is_ok_and(|b| b == "{") {
        let _ = body(&mut t, "", &mut out, 0);
    }
    out
}

fn dtype(ty: &str) -> Option<DType> {
    Some(match ty.to_lowercase().as_str() {
        "byte" => DType::U8,
        "int16" => DType::I16,
        "uint16" => DType::U16,
        "int32" => DType::I32,
        "uint32" => DType::U32,
        "float32" => DType::F32,
        "float64" => DType::F64,
        _ => return None,
    })
}

/// GET `url`. The text of the server is the error of a failed request.
async fn get(c: &HttpClient, url: &str) -> Result<Bytes> {
    let err = |e: &dyn std::fmt::Display| Error(format!("{url}: {e}"));
    let mut req = HttpRequest::new(HttpRequestBody::empty());
    *req.uri_mut() = url.parse().map_err(|e| err(&e))?;
    let r = c.execute(req).await.map_err(|e| err(&e))?;
    let status = r.status();
    let b = r.into_body().bytes().await.map_err(|e| err(&e))?;
    if !status.is_success() {
        return Err(err(&format!("{status}: {}", String::from_utf8_lossy(&b[..b.len().min(300)]).trim())));
    }
    Ok(b)
}

/// The values of the first array (or scalar) of a `.dods` response: `n` values of type `dt`, big-endian.
/// XDR: an array has its count two times first. A Byte array has 1 byte for each value (padded to 4
/// bytes), the other types 4 bytes (Int16 and UInt16 too) or 8 bytes.
fn values(b: &[u8], dt: DType, n: usize, scalar: bool) -> Result<Vec<u8>> {
    let at = b.windows(7).position(|w| w == b"\nData:\n").ok_or("OPeNDAP: no data in the response")? + 7;
    let mut d = &b[at..];
    if !scalar {
        let count = d.get(..4).map_or(0, |c| u32::from_be_bytes(c.try_into().unwrap()) as usize);
        if count != n {
            return Err(format!("OPeNDAP: {count} values, {n} expected").into());
        }
        d = &d[8.min(d.len())..];
    }
    let s = dt.size();
    let w = if dt == DType::U8 && !scalar { 1 } else { s.max(4) };
    if d.len() < n * w {
        return Err("OPeNDAP: response too short".into());
    }
    Ok(d.chunks_exact(w).take(n).flat_map(|c| &c[w - s..]).copied().collect())
}

/// Values of hyperslab shape `h` (C order) in a chunk of shape `c`: an edge chunk has its full size.
fn pad(v: Vec<u8>, h: &[u64], c: &[u64], s: usize) -> Vec<u8> {
    if h == c {
        return v;
    }
    let n = c.len();
    let row = h[n - 1] as usize * s;
    let mut out = vec![0u8; c.iter().product::<u64>() as usize * s];
    for (r, src) in v.chunks(row).enumerate() {
        let (mut q, mut at, mut stride) = (r as u64, 0, 1);
        for k in (0..n - 1).rev() {
            at += (q % h[k]) * stride;
            q /= h[k];
            stride *= c[k];
        }
        let dst = at as usize * c[n - 1] as usize * s;
        out[dst..dst + row].copy_from_slice(src);
    }
    out
}

/// Keyed source of array `a` of variable `name`: object `i` is chunk `i`, one `.dods` request.
fn source(url: &str, name: &str, a: &Array, c: &HttpClient, rt: &Handle) -> Source {
    let (shape, chunk, dt, c) = (a.shape.clone(), a.chunk.clone(), a.dtype, c.clone());
    let base = format!("{url}.dods?{}", name.replace('[', "%5B").replace(']', "%5D"));
    // The name is the key of the disk cache: the chunk shape is in it.
    Source::fetched(&format!("{base}#{chunk:?}"), rt, move |i| {
        let (mut p, mut q, mut h) = (i, String::new(), vec![0; shape.len()]);
        for k in (0..shape.len()).rev() {
            let g = shape[k].div_ceil(chunk[k]);
            let s = p % g * chunk[k];
            p /= g;
            h[k] = chunk[k].min(shape[k] - s);
            q = format!("%5B{s}:1:{}%5D{q}", s + h[k] - 1);
        }
        let (c, u, chunk) = (c.clone(), format!("{base}{q}"), chunk.clone());
        Box::pin(async move {
            let v = values(&get(&c, &u).await?, dt, h.iter().product::<u64>() as usize, h.is_empty())?;
            Ok(Some(Bytes::from(pad(v, &h, &chunk, dt.size()))))
        })
    })
}

/// Open the OPeNDAP dataset at `url` (no suffix): read its DDS and its DAS, then the time and the latitude and
/// longitude coordinates of the variables. This function blocks.
pub fn open(url: &str, rt: &Handle) -> Result<Dataset> {
    let opts = object_store::ClientOptions::new().with_allow_http(true);
    let client = SpawnedReqwestConnector::new(rt.clone()).connect(&opts).map_err(|e| Error(format!("{url}: {e}")))?;
    let (dds_u, das_u) = (format!("{url}.dds"), format!("{url}.das"));
    let (dds_b, das_b) = rt.block_on(async { tokio::join!(get(&client, &dds_u), get(&client, &das_u)) });
    let all = dds(&String::from_utf8_lossy(&dds_b?)).map_err(|e| Error(format!("{url}: {e}")))?;
    let das = das(&das_b.map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default());
    // Attributes of a variable: its container, and the container of the array in a grid (Hyrax).
    let attrs = |n: &str| -> Attrs { [n.to_string(), format!("{n}.{}", leaf(n))].iter().flat_map(|p| das.get(p).cloned().unwrap_or_default()).collect() };
    let text = |a: &Attrs, k: &str| a.iter().find(|x| x.0 == k).map(|x| x.1.clone());
    let num = |a: &Attrs, k: &str| text(a, k).and_then(|v| v.split(',').next()?.trim().parse::<f64>().ok());
    let coord = |d: &str| all.iter().find(|v| v.dims.len() == 1 && leaf(&v.name) == d);
    let is_time = |d: &str| d.eq_ignore_ascii_case("time") || coord(d).and_then(|c| text(&attrs(&c.name), "units")).is_some_and(|u| time::cf(&u).is_some());

    let mut info = Info::default();
    let (mut vars, mut small, mut sources) = (vec![], vec![], vec![]);
    for v in &all {
        let a = attrs(&v.name);
        info.vars.push(Meta { name: v.name.clone(), dims: v.dims.clone(), dtype: v.ty.to_lowercase(), attrs: a.clone() });
        for d in &v.dims {
            if !info.dims.contains(d) {
                info.dims.push(d.clone());
            }
        }
        let Some(dt) = dtype(&v.ty) else { continue };
        let n = v.dims.len();
        let names: Vec<String> = v.dims.iter().map(|d| d.0.clone()).collect();
        let shape: Vec<u64> = v.dims.iter().map(|d| d.1).collect();
        // Chunks: 1 along the leading dimensions. Smaller blocks for a cube: a view of two other axes reads
        // whole chunks along the rows and the columns.
        let b = if n > 2 { 256 } else { 512 };
        let chunk = shape.iter().enumerate().map(|(k, &s)| if n < 2 { s.min(1 << 20) } else if k + 2 >= n { s.min(b) } else { 1 }).collect();
        let dims = (0..n)
            .map(|k| match names[k].as_str() {
                _ if n < 2 => names[k].clone(),
                _ if k + 1 == n => "x".into(),
                _ if k + 2 == n => "y".into(),
                d if is_time(d) => "time".into(),
                d if ["y", "x", "time", "band"].contains(&d) => format!("{d}_"),
                d => d.into(),
            })
            .collect();
        let mut arr = Array { dims, shape, chunk, dtype: dt, le: false, codecs: vec![], chunks: Chunks::Keyed { src: sources.len() as u32, n: 0 }, place: None };
        arr.chunks = Chunks::Keyed { src: sources.len() as u32, n: arr.grid().iter().product::<u64>() as usize };
        if let Err(e) = arr.validate() {
            eprintln!("{}: {}: {e}", redact(url), v.name);
            continue;
        }
        sources.push(Arc::new(source(url, &v.name, &arr, &client, rt)));
        let fill = num(&a, "_FillValue").or(num(&a, "missing_value")).map(|f| if dt == DType::F32 { f as f32 as f64 } else { f });
        let var = Variable {
            name: v.name.clone(),
            group: String::new(),
            levels: vec![arr],
            bands: vec![leaf(&v.name).to_string()],
            fill,
            scale: num(&a, "scale_factor").unwrap_or(1.0),
            offset: num(&a, "add_offset").unwrap_or(0.0),
            units: text(&a, "units").unwrap_or_default(),
            georef: Georef::None,
            times: Default::default(),
            dims: names,
        };
        if n < 2 { small.push(var) } else { vars.push(var) }
    }
    if vars.is_empty() {
        return Err(format!("{url}: no array with 2 or more dimensions").into());
    }
    // Global attributes: the containers that are not variables (NC_GLOBAL, DODS_EXTRA, ...).
    let mut global: Vec<(&String, &Attrs)> = das.iter().filter(|(p, _)| !all.iter().any(|v| *p == &v.name || p.starts_with(&format!("{}.", v.name)))).collect();
    global.sort_by_key(|g| (g.0 != "NC_GLOBAL", g.0.clone()));
    info.attrs = global.into_iter().flat_map(|(p, a)| a.iter().map(move |(k, v)| (if p.is_empty() || p == "NC_GLOBAL" { k.clone() } else { format!("{p}.{k}") }, v.clone()))).collect();
    let name = url.rsplit('/').next().unwrap_or("").to_string();
    let desc = format!("OPeNDAP (DAP2), {} variables", vars.len());
    let mut ds = Dataset { product: Product { name, desc, vars, small, valid: None, info }, sources };

    // The values of the coordinates: one request for each coordinate.
    let mut cache: HashMap<String, Option<Arc<Vec<f64>>>> = HashMap::new();
    let mut vals = |d: &str| -> Option<Arc<Vec<f64>>> {
        cache.entry(d.to_string()).or_insert_with(|| ds.product.small.iter().find(|s| s.dims.len() == 1 && leaf(&s.name) == d).and_then(|s| crate::read_all(&ds, s).ok()).map(Arc::new)).clone()
    };
    let is = |d: &str, k: &str, unit: &str| {
        let n = d.to_lowercase();
        n == k || n == k[..3] || n.starts_with(&format!("{k}_")) || coord(d).and_then(|c| text(&attrs(&c.name), "units")).is_some_and(|u| u == unit)
    };
    let mut set = vec![];
    for v in &ds.product.vars {
        let n = v.dims.len();
        let times = v.levels[0].axis("time").and_then(|k| {
            let (unit, t0) = time::cf(&text(&attrs(&coord(&v.dims[k])?.name), "units")?)?;
            Some(Arc::new(vals(&v.dims[k])?.iter().map(|x| t0 + x * unit).collect::<Vec<f64>>()))
        });
        let (yd, xd) = (&v.dims[n - 2], &v.dims[n - 1]);
        let mut georef = Georef::None;
        if is(yd, "latitude", "degrees_north")
            && is(xd, "longitude", "degrees_east")
            && let (Some(la), Some(lo)) = (vals(yd), vals(xd))
            && la.len() > 1
            && lo.len() > 1
        {
            let (dx, dy) = (lo[1] - lo[0], la[1] - la[0]);
            let regular = |c: &[f64], d: f64| c.windows(2).all(|w| (w[1] - w[0] - d).abs() <= d.abs() * 1e-3);
            if regular(&lo, dx) && regular(&la, dy) {
                georef = Georef::Affine { gt: [lo[0] - dx / 2.0, dx, 0.0, la[0] - dy / 2.0, 0.0, dy], crs: Crs::wgs84() };
            }
        }
        set.push((times, georef));
    }
    for (v, (t, g)) in ds.product.vars.iter_mut().zip(set) {
        v.times = t.unwrap_or_default();
        v.georef = g;
    }
    Ok(ds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dds_and_das() {
        let v = dds("Dataset {\n Float64 lat[lat = 3];\n Grid {\n  Array:\n   Int16 sst[time = 2][lat = 3][lon = 4];\n  Maps:\n   Float64 time[time = 2];\n   Float64 lat[lat = 3];\n   Float64 lon[lon = 4];\n } sst;\n Sequence { Int32 a; } s;\n Structure { Byte m[5][6]; } g;\n} x.nc;").unwrap();
        let names: Vec<&str> = v.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["lat", "sst", "sst.time", "sst.lon", "g.m"]);
        assert_eq!(v[1].dims, [("time".into(), 2), ("lat".into(), 3), ("lon".into(), 4)]);
        assert_eq!(v[4].dims, [("dim_0".into(), 5), ("dim_1".into(), 6)]);
        let a = das("Attributes {\n sst {\n  String units \"deg; C\";\n  Float32 valid_range -5, 40;\n }\n NC_GLOBAL { String title \"a \\\"b\\\"\"; }\n}");
        assert_eq!(a["sst"], [("units".into(), "deg; C".into()), ("valid_range".into(), "-5, 40".into())]);
        assert_eq!(a["NC_GLOBAL"], [("title".to_string(), "a \"b\"".to_string())]);
    }

    #[test]
    fn edge_chunk_is_padded() {
        assert_eq!(pad(vec![1, 2, 3, 4], &[2, 2], &[3, 3], 1), [1, 2, 0, 3, 4, 0, 0, 0, 0]);
        let r = [b"x;\nData:\n".as_slice(), &[0, 0, 0, 2, 0, 0, 0, 2, 0xff, 0xff, 0xff, 0xfe, 0, 0, 0, 7]].concat();
        assert_eq!(values(&r, DType::I16, 2, false).unwrap(), [0xff, 0xfe, 0, 7]);
    }
}
