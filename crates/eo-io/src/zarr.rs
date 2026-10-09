//! Zarr v2 and v3 stores (EOPF products, GeoZarr). Each chunk object is a source. With the sharding codec,
//! each inner chunk is a byte range of its shard: the index of a shard is read at the first use of the shard.
//!
//! Specifications:
//! - Zarr v2: <https://zarr-specs.readthedocs.io/en/latest/v2/v2.0.html>
//! - Zarr v3 and the sharding codec: <https://zarr-specs.readthedocs.io/en/latest/v3/core/index.html>,
//!   <https://zarr-specs.readthedocs.io/en/latest/v3/codecs/sharding-indexed/index.html>
//! - Multiscales convention: <https://github.com/zarr-conventions/multiscales>
//! - EOPF products: <https://cpm.pages.eopf.copernicus.eu/eopf-cpm/main/PSFD/index.html>
use crate::{Dataset, Source, codec};
use eo_core::*;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::runtime::Handle;

/// Metadata of the store: path (no leading '/') to (is array, metadata, attributes).
type Nodes = BTreeMap<String, (bool, Value, Value)>;

/// Path of `p` in `base`. An empty base is the root of the store: the path has no leading '/'.
fn join(base: &str, p: &str) -> String {
    let b = base.trim_end_matches('/');
    if p.is_empty() || b.is_empty() { format!("{b}{p}") } else { format!("{b}/{p}") }
}

fn json(src: &Source) -> Result<Value> {
    let b = src.read_whole()?;
    serde_json::from_slice(&b).map_err(|e| Error(format!("{}: {e}", src.name())))
}

/// True if `url` is a Zarr store (local directory or remote URL).
pub fn is_zarr(url: &str, rt: &Handle) -> bool {
    ["zarr.json", ".zmetadata", ".zgroup"].iter().any(|f| {
        let s = Source::new(&join(url, f), rt);
        if s.is_local() { std::path::Path::new(s.name()).exists() } else { s.len().is_ok() }
    })
}

/// All nodes: consolidated metadata, or (local store) a walk of the directories.
fn nodes(url: &str, rt: &Handle) -> Result<(Nodes, u8)> {
    let mut out = Nodes::new();
    let (s3, s2) = (Source::new(&join(url, "zarr.json"), rt), Source::new(&join(url, ".zmetadata"), rt));
    // A remote store: ask for the metadata of the two versions at the same time (one round trip).
    let (b3, b2) = if s3.is_local() {
        (s3.read_whole().ok(), s2.read_whole().ok())
    } else {
        let (a, b) = rt.block_on(async { tokio::join!(s3.get_whole(), s2.get_whole()) });
        (a.ok().flatten(), b.ok().flatten())
    };
    let parse = |b: &bytes::Bytes| serde_json::from_slice::<Value>(b).ok();
    if let Some(root) = b3.as_ref().and_then(parse) {
        let attrs = root.get("attributes").cloned().unwrap_or(Value::Null);
        out.insert(String::new(), (false, root.clone(), attrs));
        let cm = root.pointer("/consolidated_metadata/metadata").and_then(Value::as_object);
        match cm {
            Some(m) if !m.is_empty() => {
                for (k, v) in m {
                    let arr = v.get("node_type").and_then(Value::as_str) == Some("array");
                    out.insert(k.trim_matches('/').to_string(), (arr, v.clone(), v.get("attributes").cloned().unwrap_or(Value::Null)));
                }
            }
            _ => walk(url, "", rt, 3, &mut out)?,
        }
        return Ok((out, 3));
    }
    match b2.as_ref().and_then(parse) {
        Some(z) => {
            let m = z.get("metadata").and_then(Value::as_object).ok_or("bad .zmetadata")?;
            for (k, v) in m {
                let (dir, f) = k.rsplit_once('/').unwrap_or(("", k.as_str()));
                let e = out.entry(dir.to_string()).or_insert((false, Value::Null, Value::Null));
                match f {
                    ".zarray" => (e.0, e.1) = (true, v.clone()),
                    ".zattrs" => e.2 = v.clone(),
                    _ => {}
                }
            }
        }
        None => walk(url, "", rt, 2, &mut out)?,
    }
    Ok((out, 2))
}

/// Read the metadata files of a local store.
fn walk(url: &str, rel: &str, rt: &Handle, ver: u8, out: &mut Nodes) -> Result<()> {
    let dir = join(url, rel);
    if !Source::new(&dir, rt).is_local() {
        return Err(format!("{url}: remote Zarr store without consolidated metadata").into());
    }
    let read = |f: &str| json(&Source::new(&join(&dir, f), rt)).ok();
    if ver == 3 {
        if let Some(m) = read("zarr.json") {
            let arr = m.get("node_type").and_then(Value::as_str) == Some("array");
            out.insert(rel.to_string(), (arr, m.clone(), m.get("attributes").cloned().unwrap_or(Value::Null)));
            if arr {
                return Ok(());
            }
        }
    } else if let Some(m) = read(".zarray") {
        out.insert(rel.to_string(), (true, m, read(".zattrs").unwrap_or(Value::Null)));
        return Ok(());
    } else {
        out.insert(rel.to_string(), (false, Value::Null, read(".zattrs").unwrap_or(Value::Null)));
    }
    for e in std::fs::read_dir(&dir)?.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            let n = e.file_name().to_string_lossy().to_string();
            walk(url, &if rel.is_empty() { n.clone() } else { format!("{rel}/{n}") }, rt, ver, out)?;
        }
    }
    Ok(())
}

fn dtype(s: &str) -> Option<(DType, bool)> {
    let le = !s.starts_with('>');
    let t = match s.trim_start_matches(['<', '>', '|', '=']) {
        "u1" | "uint8" => DType::U8,
        "i1" | "int8" => DType::I8,
        "u2" | "uint16" => DType::U16,
        "i2" | "int16" => DType::I16,
        "u4" | "uint32" => DType::U32,
        "i4" | "int32" => DType::I32,
        "u8" | "uint64" => DType::U64,
        "i8" | "int64" => DType::I64,
        "f4" | "float32" => DType::F32,
        "f8" | "float64" => DType::F64,
        "c8" | "complex64" => DType::CF32,
        "c16" | "complex128" => DType::CF64,
        _ => return None,
    };
    Some((t, le))
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => match s.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => s.parse().ok(),
        },
        _ => None,
    }
}

fn u64s(v: Option<&Value>) -> Option<Vec<u64>> {
    v?.as_array()?.iter().map(Value::as_u64).collect()
}

/// Bytes-to-bytes codecs, in decode order. Error for a codec that the viewer cannot decode.
fn codecs_v3(list: &[Value]) -> Result<(Vec<Codec>, bool)> {
    let mut le = true;
    let mut out = vec![];
    for c in list {
        let name = c.get("name").and_then(Value::as_str).unwrap_or("");
        let cfg = c.get("configuration");
        match name.trim_start_matches("numcodecs.") {
            "bytes" => le = cfg.and_then(|c| c.get("endian")).and_then(Value::as_str) != Some("big"),
            "blosc" => out.push(Codec::Blosc),
            "zstd" => out.push(Codec::Zstd),
            "gzip" => out.push(Codec::Gzip),
            "zlib" => out.push(Codec::Deflate),
            "crc32c" => out.push(Codec::Checksum),
            "lz4" => out.push(Codec::Lz4 { header: true }),
            "shuffle" => out.push(Codec::Shuffle { size: cfg.and_then(|c| c.get("elementsize")).and_then(Value::as_u64).unwrap_or(1) as u32 }),
            "delta" => out.push(Codec::Delta),
            "transpose" => {
                let o = u64s(cfg.and_then(|c| c.get("order"))).unwrap_or_default();
                if o.iter().enumerate().any(|(i, &v)| i as u64 != v) {
                    return Err("Zarr transpose codec is not supported".into());
                }
            }
            n => return Err(format!("Zarr codec {n} is not supported").into()),
        }
    }
    out.reverse();
    Ok((out, le))
}

/// Chunk key of chunk position `pos`.
fn key(pos: &[u64], sep: &str, v3_default: bool) -> String {
    let p: Vec<String> = pos.iter().map(u64::to_string).collect();
    let k = p.join(sep);
    if v3_default { if k.is_empty() { "c".into() } else { format!("c{sep}{k}") } } else if k.is_empty() { "0".into() } else { k }
}

/// Array of the store at `path`. Its chunk sources go to `sources`.
fn array(url: &str, path: &str, meta: &Value, ver: u8, rt: &Handle, sources: &mut Vec<Arc<Source>>) -> Result<Array> {
    let shape = u64s(meta.get("shape")).ok_or("Zarr array without shape")?;
    let dims: Vec<String>;
    let (dt, le, chunk, codecs, sep, v3def);
    let mut shard: Option<(Vec<u64>, bool, usize)> = None;
    if ver == 2 {
        (dt, le) = meta.get("dtype").and_then(Value::as_str).and_then(dtype).ok_or("Zarr dtype is not supported")?;
        chunk = u64s(meta.get("chunks")).ok_or("Zarr array without chunks")?;
        if meta.get("order").and_then(Value::as_str) == Some("F") {
            return Err("Zarr order F is not supported".into());
        }
        let mut c = vec![];
        if let Some(cmp) = meta.get("compressor").filter(|v| !v.is_null()) {
            let id = cmp.get("id").and_then(Value::as_str).unwrap_or("");
            c.push(match id {
                "blosc" => Codec::Blosc,
                "zstd" => Codec::Zstd,
                "zlib" => Codec::Deflate,
                "gzip" => Codec::Gzip,
                "lz4" => Codec::Lz4 { header: true },
                _ => return Err(format!("Zarr compressor {id} is not supported").into()),
            });
        }
        for f in meta.get("filters").and_then(Value::as_array).into_iter().flatten().rev() {
            let id = f.get("id").and_then(Value::as_str).unwrap_or("");
            c.push(match id {
                "delta" => Codec::Delta,
                "shuffle" => Codec::Shuffle { size: f.get("elementsize").and_then(Value::as_u64).unwrap_or(1) as u32 },
                _ => return Err(format!("Zarr filter {id} is not supported").into()),
            });
        }
        codecs = c;
        sep = meta.get("dimension_separator").and_then(Value::as_str).unwrap_or(".").to_string();
        v3def = false;
        dims = vec![];
    } else {
        let t = meta.get("data_type").and_then(Value::as_str).unwrap_or("");
        let d = dtype(t).ok_or_else(|| Error(format!("Zarr data type {t} is not supported")))?.0;
        let outer = u64s(meta.pointer("/chunk_grid/configuration/chunk_shape")).ok_or("Zarr array without chunk shape")?;
        let enc = meta.get("chunk_key_encoding");
        v3def = enc.and_then(|e| e.get("name")).and_then(Value::as_str) != Some("v2");
        sep = enc.and_then(|e| e.pointer("/configuration/separator")).and_then(Value::as_str).unwrap_or(if v3def { "/" } else { "." }).to_string();
        let list = meta.get("codecs").and_then(Value::as_array).cloned().unwrap_or_default();
        let sh = list.iter().find(|c| c.get("name").and_then(Value::as_str) == Some("sharding_indexed"));
        match sh {
            Some(s) => {
                let cfg = s.get("configuration").ok_or("sharding without configuration")?;
                let inner = u64s(cfg.get("chunk_shape")).ok_or("sharding without chunk shape")?;
                let (c, l) = codecs_v3(cfg.get("codecs").and_then(Value::as_array).map_or(&[][..], |v| v))?;
                let crc = cfg.get("index_codecs").and_then(Value::as_array).is_some_and(|v| v.iter().any(|c| c.get("name").and_then(Value::as_str) == Some("crc32c")));
                let end = cfg.get("index_location").and_then(Value::as_str) != Some("start");
                (dt, le, chunk, codecs) = (d, l, inner, c);
                shard = Some((outer, end, if crc { 4 } else { 0 }));
            }
            None => {
                let (c, l) = codecs_v3(&list)?;
                (dt, le, chunk, codecs) = (d, l, outer, c);
            }
        }
        dims = meta.get("dimension_names").and_then(Value::as_array).map(|v| v.iter().map(|d| d.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
    }
    let n = shape.len();
    if n == 0 {
        return Err("Zarr array without dimensions".into());
    }
    // The last two dimensions are the image rows and columns.
    let dims: Vec<String> = (0..n)
        .map(|i| match i {
            _ if i == n - 1 => "x".into(),
            _ if i + 2 == n => "y".into(),
            _ => match dims.get(i).map(|d| d.to_lowercase()) {
                Some(d) if d == "band" || d == "bands" => "band".into(),
                Some(d) if !d.is_empty() => d,
                _ => format!("dim_{i}"),
            },
        })
        .collect();
    let grid: Vec<u64> = shape.iter().zip(&chunk).map(|(s, c)| s.div_ceil(*c)).collect();
    let total = grid.iter().product::<u64>() as usize;
    let pos = |mut i: u64, g: &[u64]| {
        let mut p = vec![0; g.len()];
        for d in (0..g.len()).rev() {
            p[d] = i % g[d];
            i /= g[d];
        }
        p
    };
    let base = join(url, path);
    let chunks = match shard {
        // One keyed source for all chunk objects. A data cube has millions of chunks for each variable:
        // a source and a location for each chunk fill the memory.
        None => {
            let (b, g, s) = (base.clone(), grid.clone(), sep.clone());
            sources.push(Arc::new(Source::keyed(&base, rt, move |i| join(&b, &key(&pos(i, &g), &s, v3def)))));
            eo_core::Chunks::Keyed { src: sources.len() as u32 - 1, n: total }
        }
        Some((outer, end, crc)) => {
            let mut chunks = Vec::with_capacity(total);
            let per: Vec<u64> = outer.iter().zip(&chunk).map(|(o, c)| o / c).collect();
            let nper = per.iter().product::<u64>() as usize;
            let sgrid: Vec<u64> = shape.iter().zip(&outer).map(|(s, o)| s.div_ceil(*o)).collect();
            let nshards = sgrid.iter().product::<u64>();
            // One source for each shard. The engine reads the index of a shard at the first use of one of
            // its chunks: the open of a product reads no shard (a remote product has many).
            let first = sources.len() as u32;
            for sh in 0..nshards {
                sources.push(Arc::new(Source::shard(&join(&base, &key(&pos(sh, &sgrid), &sep, v3def)), rt, nper as u64, end, crc as u64)));
            }
            for i in 0..total as u64 {
                let p = pos(i, &grid);
                let sp: Vec<u64> = p.iter().zip(&per).map(|(p, n)| p / n).collect();
                let ip: Vec<u64> = p.iter().zip(&per).map(|(p, n)| p % n).collect();
                let sh = sp.iter().zip(&sgrid).fold(0, |a, (p, n)| a * n + p);
                let k = ip.iter().zip(&per).fold(0, |a, (p, n)| a * n + p);
                chunks.push(ChunkLoc { src: first + sh as u32, off: k, len: ChunkLoc::SHARD });
            }
            chunks.into()
        }
    };
    let a = Array { dims, shape, chunk, dtype: dt, le, codecs, chunks, place: None };
    a.validate()?;
    Ok(a)
}

/// First two values of a 1D coordinate array: one request (a range, if the chunk is not compressed).
async fn first_two(a: Array, srcs: Vec<Arc<Source>>) -> Option<(f64, f64)> {
    let loc = a.chunks.at(0);
    let src = srcs.get(loc.src as usize)?;
    let two = 2 * a.dtype.size();
    let v = if a.codecs.is_empty() && loc.len == ChunkLoc::WHOLE {
        codec::to_f64(&a, &src.get_ranges(&[0..two as u64]).await.ok()?.remove(0))
    } else {
        let b = src.get_chunk(loc).await.ok().filter(|b| !b.is_empty())?;
        codec::to_f64(&a, codec::decode(&a, &b).ok()?.get(..two)?)
    };
    Some((*v.first()?, *v.get(1)?))
}

/// All values of a small 1D array (a time coordinate).
async fn values_1d(a: &Array, srcs: &[Arc<Source>]) -> Option<Vec<f64>> {
    let mut out = vec![];
    for loc in a.chunks.iter() {
        let b = srcs.get(loc.src as usize)?.get_chunk(loc).await.ok().filter(|b| !b.is_empty())?;
        out.extend(codec::to_f64(a, &codec::decode(a, &b).ok()?));
    }
    out.truncate(a.shape[0] as usize);
    Some(out)
}

/// Times of the `n` steps of a time dimension: the `time` coordinate array of the group or of a group
/// above it, with CF units. Empty if the store does not have them. `cache`: one read for each coordinate.
fn times(url: &str, nodes: &Nodes, group: &str, n: u64, ver: u8, rt: &Handle, cache: &mut std::collections::HashMap<String, Arc<Vec<f64>>>) -> Arc<Vec<f64>> {
    let mut g = group;
    loop {
        let p = join(g, "time").trim_start_matches('/').to_string();
        if let Some(m) = nodes.get(&p).filter(|m| m.0) {
            return cache
                .entry(p.clone())
                .or_insert_with(|| {
                    let mut srcs = vec![];
                    let a = array(url, &p, &m.1, ver, rt, &mut srcs).ok().filter(|a| a.shape == [n]);
                    let vals = a.and_then(|a| rt.block_on(values_1d(&a, &srcs)));
                    Arc::new(match (vals, m.2.get("units").and_then(Value::as_str).and_then(time::cf)) {
                        (Some(v), Some((unit, t0))) => v.into_iter().map(|x| t0 + x * unit).collect(),
                        _ => vec![],
                    })
                })
                .clone();
        }
        if g.is_empty() {
            return Default::default();
        }
        g = g.rsplit_once('/').map_or("", |u| u.0);
    }
}

/// Names of the dimensions before the rows and the columns. Zarr v2 has them in the attribute
/// `_ARRAY_DIMENSIONS` (xarray). The engine uses the names "band" and "time".
fn name_dims(a: &mut Array, attrs: &Value) {
    let names = attrs.get("_ARRAY_DIMENSIONS").and_then(Value::as_array);
    for i in 0..a.dims.len().saturating_sub(2) {
        let d = names.and_then(|v| v.get(i)).and_then(Value::as_str).map_or(a.dims[i].clone(), str::to_lowercase);
        a.dims[i] = match d.as_str() {
            "band" | "bands" => "band".into(),
            d if d.starts_with("time") => "time".into(),
            _ => d,
        };
    }
}

/// Affine transforms (GDAL order) of the groups that have coordinate arrays (pixel centers), and true if
/// the coordinates are longitude and latitude.
/// The coordinates of all groups are read at the same time: a remote product has many groups.
fn transforms(url: &str, nodes: &Nodes, ver: u8, rt: &Handle) -> std::collections::HashMap<String, ([f64; 6], bool)> {
    let mut jobs = vec![];
    for g in nodes.iter().filter(|n| !n.1.0).map(|n| n.0) {
        let arr = |n: &str| {
            let p = join(g, n).trim_start_matches('/').to_string();
            let m = nodes.get(&p).filter(|m| m.0)?;
            let mut srcs = vec![];
            let a = array(url, &p, &m.1, ver, rt, &mut srcs).ok()?;
            (a.shape.len() == 1 && a.shape[0] >= 2 && a.chunk[0] >= 2).then_some((a, srcs))
        };
        // Coordinates x and y in the CRS of the product, or longitude and latitude (data cubes).
        for (k, (nx, ny)) in [("x", "y"), ("lon", "lat"), ("longitude", "latitude")].into_iter().enumerate() {
            if let (Some(x), Some(y)) = (arr(nx), arr(ny)) {
                jobs.push((g.clone(), x, y, k > 0));
                break;
            }
        }
    }
    // No spawned tasks: `block_on` of a handle does not run the tasks of a current-thread runtime.
    let got = rt.block_on(futures_util::future::join_all(jobs.into_iter().map(|(g, x, y, geo)| async move {
        let (x, y) = tokio::join!(first_two(x.0, x.1), first_two(y.0, y.1));
        (g, x, y, geo)
    })));
    got.into_iter()
        .filter_map(|(g, x, y, geo)| {
            let (x, y) = (x?, y?);
            let (dx, dy) = (x.1 - x.0, y.1 - y.0);
            Some((g, ([x.0 - dx / 2.0, dx, 0.0, y.0 - dy / 2.0, 0.0, dy], geo)))
        })
        .collect()
}

/// First value of `key` in the attributes of the node, its parents, and the root (recursive search).
fn find_attr<'a>(nodes: &'a Nodes, path: &str, keys: &[&str]) -> Option<&'a Value> {
    fn search<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
        match v {
            Value::Object(m) => keys.iter().find_map(|k| m.get(*k)).or_else(|| m.values().find_map(|x| search(x, keys))),
            _ => None,
        }
    }
    let mut p = Some(path);
    while let Some(q) = p {
        if let Some(v) = nodes.get(q).and_then(|n| search(&n.2, keys)) {
            return Some(v);
        }
        p = if q.is_empty() { None } else { Some(q.rsplit_once('/').map_or("", |x| x.0)) };
    }
    None
}

fn epsg(nodes: &Nodes, path: &str) -> Option<u32> {
    let v = find_attr(nodes, path, &["proj:epsg", "proj:code", "horizontal_crs_code"])?;
    match v {
        Value::Number(n) => n.as_u64().map(|n| n as u32),
        Value::String(s) => s.rsplit(':').next()?.parse().ok(),
        _ => None,
    }
}

/// Affine transform (GDAL order) of a group without coordinate arrays: the `spatial:transform` attribute.
fn attr_transform(nodes: &Nodes, group: &str) -> Option<[f64; 6]> {
    let t = find_attr(nodes, group, &["spatial:transform"])?.as_array()?;
    let t: Vec<f64> = t.iter().filter_map(Value::as_f64).collect();
    (t.len() >= 6).then(|| [t[2], t[0], t[1], t[5], t[3], t[4]])
}

/// Groups of the multiscale levels of `group`: from the multiscales attribute, else from sibling groups
/// named r<N>m (EOPF Sentinel-2).
fn scales(nodes: &Nodes, group: &str) -> Option<Vec<String>> {
    let attrs = &nodes.get(group)?.2;
    let ms = attrs.get("multiscales");
    let from_layout = ms.and_then(|m| m.get("layout")).and_then(Value::as_array).map(|l| l.iter().filter_map(|e| e.get("asset")?.as_str().map(String::from)).collect::<Vec<_>>());
    let from_datasets = ms
        .and_then(Value::as_array)
        .and_then(|a| a.first()?.get("datasets")?.as_array().cloned())
        .map(|d| d.iter().filter_map(|e| e.get("path")?.as_str().map(String::from)).collect::<Vec<_>>());
    if let Some(l) = from_layout.or(from_datasets).filter(|l| !l.is_empty()) {
        return Some(l);
    }
    let mut r: Vec<(u32, String)> = nodes
        .keys()
        .filter_map(|k| {
            let (p, n) = k.rsplit_once('/').unwrap_or(("", k));
            let v = n.strip_prefix('r')?.strip_suffix('m')?.parse().ok()?;
            (p == group).then(|| (v, n.to_string()))
        })
        .collect();
    r.sort();
    (r.len() > 1).then(|| r.into_iter().map(|x| x.1).collect())
}

pub fn open(url: &str, rt: &Handle) -> Result<Dataset> {
    let (nodes, ver) = nodes(url, rt)?;
    let mut sources: Vec<Arc<Source>> = vec![];
    let mut vars: Vec<Variable> = vec![];
    let tf = transforms(url, &nodes, ver, rt);
    let mut tcache = std::collections::HashMap::new();
    // Arrays at a multiscale level: (parent group, array name) to level groups.
    let mut done = std::collections::HashSet::new();
    // The level-0 array of each variable of the viewer, and the name of the variable.
    let mut shown: Vec<(String, String)> = vec![];
    let arrays: Vec<(&String, &Value)> = nodes.iter().filter(|n| n.1.0).map(|(k, v)| (k, &v.1)).collect();
    for (path, meta) in &arrays {
        if done.contains(*path) || u64s(meta.get("shape")).is_none_or(|s| s.len() < 2) {
            continue;
        }
        let (group, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        let (parent, _) = group.rsplit_once('/').unwrap_or(("", group));
        // Level groups that contain this array, fine to coarse (the first group with the array is level 0).
        let level_groups: Vec<String> = match scales(&nodes, parent) {
            Some(g) if g.iter().any(|x| join(parent, x) == group) => g
                .iter()
                .map(|x| join(parent, x))
                .filter(|g| nodes.get(&join(g, name)).is_some_and(|n| n.0))
                .collect(),
            _ => vec![group.to_string()],
        };
        let mut levels = vec![];
        let mut gts = vec![];
        for g in &level_groups {
            let p = join(g, name);
            done.insert(p.clone());
            match array(url, &p, &nodes[&p].1, ver, rt, &mut sources) {
                Ok(mut a) => {
                    name_dims(&mut a, &nodes[&p].2);
                    gts.push(tf.get(g).map(|t| t.0).or_else(|| attr_transform(&nodes, g)));
                    levels.push(a);
                }
                Err(e) if levels.is_empty() => {
                    eprintln!("{url}: {p}: {e}");
                    break;
                }
                Err(e) => eprintln!("{url}: {p}: overview ignored: {e}"),
            }
        }
        if levels.is_empty() {
            continue;
        }
        // Order by width, and place the levels with their transforms.
        let mut idx: Vec<usize> = (0..levels.len()).collect();
        idx.sort_by_key(|&i| std::cmp::Reverse(levels[i].len_of("x")));
        let levels: Vec<Array> = idx.iter().map(|&i| levels[i].clone()).collect();
        let gts: Vec<Option<[f64; 6]>> = idx.iter().map(|&i| gts[i]).collect();
        let mut levels = levels;
        if let Some(g0) = gts[0] {
            for (a, g) in levels.iter_mut().zip(&gts).skip(1) {
                if let Some(g) = g {
                    a.place = Some([g[1] / g0[1], g[5] / g0[5], (g[0] - g0[0]) / g0[1], (g[3] - g0[3]) / g0[5]]);
                }
            }
        }
        let p0 = join(&level_groups[idx[0]], name);
        let attrs = &nodes[&p0].2;
        let get = |k: &str| attrs.get(k).and_then(num);
        let fill = get("_FillValue").or_else(|| get("fill_value")).or_else(|| nodes[&p0].1.get("fill_value").and_then(num));
        let geo = tf.get(&level_groups[idx[0]]).is_some_and(|t| t.1);
        let crs = Crs { epsg: if geo { Some(4326) } else { epsg(&nodes, &p0) }, name: String::new() };
        let crs = Crs { name: crs.epsg.map_or(String::new(), |e| format!("EPSG:{e}")), ..crs };
        let georef = match gts[0] {
            Some(gt) if crs.epsg.is_some() => Georef::Affine { gt, crs },
            _ => Georef::None,
        };
        let nb = levels[0].len_of("band") as usize;
        let short = if level_groups.len() > 1 { join(parent, name) } else { p0.clone() };
        shown.push((p0.clone(), short.clone()));
        vars.push(Variable {
            bands: if nb == 1 { vec![name.to_string()] } else { (1..=nb).map(|b| format!("Band {b}")).collect() },
            name: short,
            group: String::new(),
            fill,
            scale: get("scale_factor").unwrap_or(1.0),
            offset: get("add_offset").unwrap_or(0.0),
            units: attrs.get("units").and_then(Value::as_str).unwrap_or("").to_string(),
            georef,
            times: levels[0].axis("time").map_or(Default::default(), |k| times(url, &nodes, &level_groups[idx[0]], levels[0].shape[k], ver, rt, &mut tcache)),
            levels,
        });
    }
    if vars.is_empty() {
        return Err(format!("{url}: Zarr store without 2D arrays").into());
    }
    // Reflectance bands first (the default layer is the first variable).
    vars.sort_by_key(|v| (!v.name.contains("reflectance"), v.name.clone()));
    let name = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url).to_string();
    let desc = format!("Zarr v{ver}, {} variables", vars.len());
    // The times with data, from the attributes of the store (the ARCO ERA5 store has them).
    let day = |k: &str| nodes.get("").and_then(|n| n.2.get(k)).and_then(Value::as_str).and_then(time::parse);
    let valid = day("valid_time_start").zip(day("valid_time_stop")).map(|(a, b)| (a, b + 86_399.0));
    let info = info(&nodes, &shown);
    Ok(Dataset { product: Product { name, desc, vars, valid, info }, sources })
}

/// A JSON attribute as text: a string as it is, a list of numbers or strings with commas, other values as JSON.
fn attr_text(v: &Value) -> String {
    let t = match v {
        Value::String(s) => s.clone(),
        Value::Array(a) if a.iter().all(|x| !x.is_array() && !x.is_object()) => a.iter().map(|x| x.as_str().map_or(x.to_string(), str::to_string)).collect::<Vec<_>>().join(", "),
        _ => v.to_string(),
    };
    if t.chars().count() > 2000 { t.chars().take(2000).collect::<String>() + " ..." } else { t }
}

/// The metadata of a store for the user: the attributes of the root group, the dimensions and all arrays.
/// `shown`: the level-0 array of each variable of the viewer, and its name in the product.
fn info(nodes: &Nodes, shown: &[(String, String)]) -> Info {
    let attrs = |a: &Value| -> Attrs {
        let mut v: Attrs = a.as_object().map(|o| o.iter().filter(|(k, _)| *k != "_ARRAY_DIMENSIONS").map(|(k, x)| (k.clone(), attr_text(x))).collect()).unwrap_or_default();
        v.sort();
        v
    };
    let mut info = Info { attrs: nodes.get("").map(|n| attrs(&n.2)).unwrap_or_default(), ..Default::default() };
    let mut paths: Vec<&String> = nodes.iter().filter(|n| n.1.0).map(|n| n.0).collect();
    paths.sort();
    for p in paths {
        let (meta, a) = (&nodes[p].1, &nodes[p].2);
        let shape = u64s(meta.get("shape")).unwrap_or_default();
        let names: Vec<String> = a.get("_ARRAY_DIMENSIONS").or_else(|| meta.get("dimension_names")).and_then(Value::as_array)
            .map(|v| v.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
        let dims: Vec<(String, u64)> = shape.iter().enumerate().map(|(k, &n)| (names.get(k).filter(|s| !s.is_empty()).cloned().unwrap_or(format!("dim_{k}")), n)).collect();
        for d in &dims {
            if !info.dims.contains(d) {
                info.dims.push(d.clone());
            }
        }
        let dtype = meta.get("data_type").or_else(|| meta.get("dtype")).map(attr_text).unwrap_or_default();
        let name = shown.iter().find(|s| s.0 == *p).map_or(p.clone(), |s| s.1.clone());
        info.vars.push(Meta { name, dims, dtype, attrs: attrs(a) });
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data cube with 20 million chunks opens with one source for each array, and reads a chunk by its
    /// position. One source and one location for each chunk filled the memory (ERA5: 366 million chunks).
    #[test]
    fn cube_with_millions_of_chunks_opens_without_a_source_for_each_chunk() {
        let dir = std::env::temp_dir().join(format!("eoview-cube-{}.zarr", std::process::id()));
        let arr = |shape: &[u64], chunks: &[u64], dims: &[&str]| {
            (serde_json::json!({"zarr_format": 2, "shape": shape, "chunks": chunks, "dtype": "<f4", "compressor": null, "filters": null, "fill_value": 0, "order": "C"}), serde_json::json!({"_ARRAY_DIMENSIONS": dims}))
        };
        let mut m = serde_json::Map::new();
        m.insert(".zgroup".into(), serde_json::json!({"zarr_format": 2}));
        for v in 0..20 {
            let (a, d) = arr(&[1_000_000, 4, 8], &[1, 4, 8], &["time", "latitude", "longitude"]);
            m.insert(format!("v{v}/.zarray"), a);
            m.insert(format!("v{v}/.zattrs"), d);
        }
        std::fs::create_dir_all(dir.join("v3")).unwrap();
        std::fs::write(dir.join(".zmetadata"), serde_json::json!({"zarr_consolidated_format": 1, "metadata": m}).to_string()).unwrap();
        let px: Vec<u8> = (0..32).flat_map(|i| (i as f32).to_le_bytes()).collect();
        std::fs::write(dir.join("v3/123456.0.0"), &px).unwrap();

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ds = open(dir.to_str().unwrap(), rt.handle()).unwrap();
        assert_eq!((ds.product.vars.len(), ds.sources.len()), (20, 20));
        let a = &ds.product.vars.iter().find(|v| v.name.ends_with("v3")).unwrap().levels[0];
        assert_eq!(a.chunks.len(), 1_000_000);
        let loc = a.chunks.at(123_456);
        assert_eq!(ds.sources[loc.src as usize].read_chunk(loc).unwrap(), px);
        // A chunk that was not written has no bytes: the fill value.
        let none = a.chunks.at(7);
        assert!(ds.sources[none.src as usize].read_chunk(none).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
