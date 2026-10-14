//! GRIB editions 1 and 2. A message (or a field of a GRIB 2 message) is one 2D field, and one chunk. The
//! reader puts the fields with the same parameter, level type and grid in one variable. The valid time,
//! the level and the ensemble member are its other dimensions. A combination without a field is a chunk
//! without data.
//!
//! Specifications: WMO Manual on Codes, volume I.2 (FM 92 GRIB edition 2, FM 92 GRIB edition 1),
//! <https://library.wmo.int/idurl/4/35625>. CCSDS 121.0-B-3 (lossless data compression) for template 5.42.
use crate::Source;
use eo_core::*;
use std::sync::Arc;

/// True if the source starts with a GRIB message, maybe after a header of less than 1 kB.
pub fn is_grib(src: &Source) -> bool {
    let Ok(b) = src.len().and_then(|n| src.read(0..n.min(1024))) else { return false };
    find(&b).is_some()
}

/// Position of the first "GRIB" indicator of edition 1 or 2 in `b`.
fn find(b: &[u8]) -> Option<usize> {
    b.windows(8).position(|w| &w[..4] == b"GRIB" && (w[7] == 1 || w[7] == 2))
}

/// Unsigned big-endian integer of `n` bytes at `p`. 0 after the end of `b`.
fn be(b: &[u8], p: usize, n: usize) -> u64 {
    b.get(p..p + n).map_or(0, |s| s.iter().fold(0, |a, &x| a << 8 | x as u64))
}

/// Signed integer of `n` bytes at `p`: the first bit is the sign, the other bits are the magnitude.
fn sm(b: &[u8], p: usize, n: usize) -> i64 {
    let (v, m) = (be(b, p, n) as i64, 1i64 << (8 * n - 1));
    if v & m != 0 { -(v & (m - 1)) } else { v }
}

/// Reads of a source through a window of 64 kB: the sections before the data of a message need one
/// request (a remote file) or one system call, not one for each section.
struct Win<'a> {
    src: &'a Source,
    len: u64,
    at: u64,
    buf: bytes::Bytes,
}

impl Win<'_> {
    fn read(&mut self, r: std::ops::Range<u64>) -> Result<bytes::Bytes> {
        if r.end > self.len {
            return Err(format!("{}: truncated GRIB message", self.src.name()).into());
        }
        if r.start < self.at || r.end > self.at + self.buf.len() as u64 {
            self.buf = self.src.read(r.start..self.len.min(r.end.max(r.start + 65536)))?;
            self.at = r.start;
        }
        Ok(self.buf.slice((r.start - self.at) as usize..(r.end - self.at) as usize))
    }
}

/// One 2D field of the file.
struct Field {
    /// Equal for the fields of one variable: parameter, level type, statistics and grid.
    key: Vec<u8>,
    ed: u8,
    /// GRIB 2: discipline, category, number. GRIB 1: table version, 0, parameter.
    param: (u8, u8, u8),
    lev_type: u8,
    level: f64,
    member: f64,
    reft: f64,
    valid: f64,
    /// Section 3 (GRIB 2) or the GDS (GRIB 1).
    grid: bytes::Bytes,
    /// Data representation template (GRIB 2), or the BDS flags (GRIB 1).
    pack: u16,
    centre: u16,
    loc: ChunkLoc,
}

/// Open a GRIB file. `idx` is the index of `src` in the dataset sources.
pub fn open(src: &Source, idx: u32, name: String) -> Result<Product> {
    let len = src.len()?;
    let mut fields = vec![];
    let mut w = Win { src, len, at: 0, buf: Default::default() };
    let mut p = find(&w.read(0..len.min(1024))?).ok_or("no GRIB message")? as u64;
    while p + 16 <= len {
        let h = w.read(p..p + 16)?;
        if !h.starts_with(b"GRIB") {
            // Padding between messages: the next message is in the next 1 kB.
            match find(&w.read(p..len.min(p + 1024))?) {
                Some(k) => p += k as u64,
                None => break,
            }
            continue;
        }
        // ponytail: GRIB 1 messages of more than 8 MB (ECMWF length coding) are not supported.
        let n = if h[7] == 2 { be(&h, 8, 8) } else { be(&h, 4, 3) };
        if n < 16 || p + n > len {
            eprintln!("{}: truncated GRIB message at byte {p}", src.name());
            break;
        }
        if h[7] == 2 { grib2(&mut w, idx, p, n, &mut fields)? } else { grib1(&mut w, idx, p, n, &mut fields)? }
        p += n;
    }
    // Variables: the fields with the same key, in the order of the file.
    let mut groups: Vec<Vec<Field>> = vec![];
    for f in fields {
        match groups.iter_mut().find(|g| g[0].key == f.key) {
            Some(g) => g.push(f),
            None => groups.push(vec![f]),
        }
    }
    let mut vars: Vec<Variable> = vec![];
    let mut metas = vec![];
    let mut all_times: Vec<Arc<Vec<f64>>> = vec![];
    for fs in &groups {
        let f0 = &fs[0];
        let Some(g) = grid(f0.ed, &f0.grid) else {
            eprintln!("{}: {}: grid not supported (reduced or spectral)", src.name(), param(f0).0);
            continue;
        };
        // Dimensions: valid time, level, member. If two fields have the same valid time (forecasts from
        // two reference times), the dimensions are the reference time and the forecast step.
        let coords = |alt: bool| -> Vec<Vec<f64>> {
            fs.iter().map(|f| if alt { vec![f.reft, (f.valid - f.reft) / 3600.0, f.level, f.member] } else { vec![f.valid, f.level, f.member] }).collect()
        };
        let mut s: Vec<Vec<u64>> = coords(false).iter().map(|x| x.iter().map(|v| v.to_bits()).collect()).collect();
        s.sort();
        let alt = s.windows(2).any(|w| w[0] == w[1]);
        let c = coords(alt);
        let dnames: &[&str] = if alt { &["time", "step", "level", "member"] } else { &["time", "level", "member"] };
        let axes: Vec<(usize, Vec<f64>)> = (0..dnames.len())
            .map(|d| {
                let mut v: Vec<f64> = c.iter().map(|x| x[d]).collect();
                v.sort_by(f64::total_cmp);
                v.dedup_by(|a, b| a.to_bits() == b.to_bits());
                (d, v)
            })
            .filter(|(_, v)| v.len() > 1)
            .collect();
        let mut chunks = vec![ChunkLoc { src: idx, off: 0, len: 0 }; axes.iter().map(|a| a.1.len()).product()];
        for (f, x) in fs.iter().zip(&c) {
            let i = axes.iter().fold(0, |i, (d, v)| i * v.len() + v.partition_point(|q| q.total_cmp(&x[*d]).is_lt()));
            // Two fields with the same coordinates: the first one.
            if chunks[i].len == 0 {
                chunks[i] = f.loc;
            }
        }
        let mut dims: Vec<String> = axes.iter().map(|(d, _)| dnames[*d].to_string()).collect();
        let mut shape: Vec<u64> = axes.iter().map(|a| a.1.len() as u64).collect();
        let (yx, wh) = if g.column_major { (["x", "y"], [g.ni, g.nj]) } else { (["y", "x"], [g.nj, g.ni]) };
        dims.extend(yx.map(String::from));
        shape.extend(wh);
        let mut chunk = vec![1; axes.len()];
        chunk.extend(wh);
        let a = Array { dims: dims.clone(), shape: shape.clone(), chunk, dtype: DType::F32, le: true, codecs: vec![Codec::Grib { edition: f0.ed }], chunks: chunks.into(), place: None };
        a.validate()?;
        let times = match axes.first() {
            Some((0, t)) => match all_times.iter().find(|x| ***x == *t) {
                Some(x) => x.clone(),
                None => {
                    all_times.push(Arc::new(t.clone()));
                    all_times.last().unwrap().clone()
                }
            },
            _ => Default::default(),
        };
        let (short, long, units) = param(f0);
        let mut vname = short.clone();
        for k in 1.. {
            if !vars.iter().any(|v| v.name == vname) {
                break;
            }
            vname = if k == 1 { format!("{short}_{}", level_name(f0.ed, f0.lev_type)) } else { format!("{short}_{k}") };
        }
        let list = |v: &[f64]| v.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join(" ");
        let mut attrs: Attrs = vec![("name".into(), long), ("units".into(), units.clone())];
        attrs.push(match f0.ed {
            2 => ("parameter".into(), format!("discipline {}, category {}, number {}", f0.param.0, f0.param.1, f0.param.2)),
            _ => ("parameter".into(), format!("table {}, number {}", f0.param.0, f0.param.2)),
        });
        attrs.push(("level type".into(), format!("{} ({})", level_name(f0.ed, f0.lev_type), f0.lev_type)));
        let one = |d: usize| axes.iter().find(|a| a.0 == d).map_or_else(|| list(&[c[0][d]]), |a| list(&a.1));
        let (lev, mem) = if alt { (2, 3) } else { (1, 2) };
        if !f0.level.is_nan() {
            attrs.push(("level".into(), one(lev)));
        }
        if !f0.member.is_nan() {
            attrs.push(("member".into(), one(mem)));
        }
        if alt {
            attrs.push(("step (hours)".into(), one(1)));
        }
        if times.is_empty() {
            attrs.push((if alt { "reference time" } else { "valid time" }.into(), time::text(c[0][0])));
        }
        attrs.push(("grid".into(), g.desc.clone()));
        attrs.push(("packing".into(), packing(f0.ed, f0.pack)));
        attrs.push(("centre".into(), centre(f0.centre)));
        metas.push(Meta { name: vname.clone(), dims: dims.iter().cloned().zip(shape.iter().copied()).collect(), dtype: "f32".into(), attrs });
        vars.push(Variable { bands: vec![vname.clone()], name: vname, group: String::new(), levels: vec![a], fill: None, scale: 1.0, offset: 0.0, units, georef: g.georef, times, dims });
    }
    if vars.is_empty() {
        return Err(format!("{name}: no GRIB field on a supported grid").into());
    }
    let mut idims: Vec<(String, u64)> = metas.iter().flat_map(|m| m.dims.iter().cloned()).collect();
    idims.sort();
    idims.dedup();
    let eds: Vec<u8> = groups.iter().map(|g| g[0].ed).collect();
    let n: usize = groups.iter().map(|g| g.len()).sum();
    let attrs = vec![
        ("edition".into(), if eds.contains(&1) && eds.contains(&2) { "1 and 2".into() } else { eds[0].to_string() }),
        ("fields".into(), n.to_string()),
        ("centre".into(), centre(groups[0][0].centre)),
    ];
    let valid = groups.iter().flatten().map(|f| f.valid).fold(None, |a: Option<(f64, f64)>, t| Some(a.map_or((t, t), |(x, y)| (x.min(t), y.max(t)))));
    let desc = format!("GRIB {}, {} fields, {} variables", attrs[0].1, n, vars.len());
    Ok(Product { name, desc, vars, small: vec![], info: Info { attrs, dims: idims, vars: metas }, valid })
}

/// Seconds of a time unit (GRIB 2 code table 4.4, GRIB 1 table 4). None for a month, a year or more.
// ponytail: with these units the valid time is the reference time. Add calendar months if a product needs it.
fn unit(u: u8, ed: u8) -> Option<f64> {
    Some(match (u, ed) {
        (0, _) => 60.0,
        (1, _) => 3600.0,
        (2, _) => 86400.0,
        (10, _) => 10800.0,
        (11, _) => 21600.0,
        (12, _) => 43200.0,
        (13, 2) | (254, 1) => 1.0,
        (13, 1) => 900.0,
        (14, 1) => 1800.0,
        _ => return None,
    })
}

/// Time of year, month, day, hour, minute, second.
fn ymd(y: u64, m: u64, d: u64, h: u64, mi: u64, s: u64) -> f64 {
    time::days(y as i64, m as i64, d as i64) as f64 * 86400.0 + (h * 3600 + mi * 60 + s) as f64
}

/// The fields of a GRIB 2 message at `start` (length `n`).
fn grib2(src: &mut Win, idx: u32, start: u64, n: u64, out: &mut Vec<Field>) -> Result<()> {
    let disc = src.read(start + 6..start + 7)?[0];
    let (mut s1, mut s3, mut s4, mut s5) = Default::default();
    let (mut s5_at, mut bitmap_at, mut ind) = (0, None, 255);
    let (mut p, end) = (start + 16, start + n);
    while p + 5 <= end {
        let h = src.read(p..p + 5)?;
        if h.starts_with(b"7777") {
            break;
        }
        let len = be(&h, 0, 4);
        if len < 5 || p + len > end {
            return Err(format!("GRIB 2 message at byte {start}: bad length of section {}", h[4]).into());
        }
        match h[4] {
            1 => s1 = src.read(p..p + len)?,
            3 => s3 = src.read(p..p + len)?,
            4 => s4 = src.read(p..p + len)?,
            5 => (s5, s5_at) = (src.read(p..p + len)?, p),
            6 => {
                ind = src.read(p + 5..p + 6)?[0];
                match ind {
                    0 => bitmap_at = Some(p),
                    254 => {}
                    _ => bitmap_at = None,
                }
            }
            7 if s1.len() < 21 || s3.len() < 38 || s4.len() < 11 || s5.len() < 12 => return Err(format!("GRIB 2 message at byte {start}: section 1, 3, 4 or 5 is missing").into()),
            7 => {
                // The chunk has sections 5 to 7, or starts at the section 6 of a bitmap that an earlier field defined.
                let off = if ind == 254 { bitmap_at.ok_or("GRIB 2: no bitmap before indicator 254")? } else { s5_at };
                out.push(field2(disc, &s1, &s3, &s4, &s5, ChunkLoc { src: idx, off, len: p + len - off }));
            }
            _ => {}
        }
        p += len;
    }
    Ok(())
}

fn field2(disc: u8, s1: &bytes::Bytes, s3: &bytes::Bytes, s4: &[u8], s5: &[u8], loc: ChunkLoc) -> Field {
    let t = |i| be(s1, i, 1);
    let reft = ymd(be(s1, 12, 2), t(14), t(15), t(16), t(17), t(18));
    let tpl = be(s4, 7, 2) as u16;
    // Product templates 4.0 to 4.15 have the same first octets. 4.40 to 4.43 (chemical constituents)
    // have 2 more octets after the parameter number.
    let sh = match tpl {
        0..=15 if s4.len() >= 34 => 0,
        40..=43 if s4.len() >= 36 => 2,
        _ => usize::MAX,
    };
    let mut key = vec![disc, s4[9], s4[10]];
    key.extend((tpl as u32).to_be_bytes());
    let (mut level, mut lev_type, mut member, mut valid) = (f64::NAN, 255, f64::NAN, reft);
    if sh != usize::MAX {
        key.extend(&s4[11..11 + sh]);
        let ft = be(s4, 18 + sh, 4) as f64;
        valid = reft + unit(s4[17 + sh], 2).map_or(0.0, |u| ft * u);
        lev_type = s4[22 + sh];
        let (sf, sv) = (sm(s4, 23 + sh, 1), be(s4, 24 + sh, 4));
        if lev_type != 255 && sv != 0xFFFF_FFFF {
            level = sv as f64 / 10f64.powi(sf as i32);
            if lev_type == 100 || lev_type == 108 {
                level /= 100.0; // Pa to hPa
            }
        }
        key.extend([lev_type, s4[28 + sh]]);
        if matches!(tpl, 1 | 11 | 41 | 43) {
            member = s4.get(35 + sh).map_or(f64::NAN, |&m| m as f64);
        }
        // Statistics over a time interval: the valid time is the end of the interval.
        let end = match tpl {
            8 | 42 => Some(34 + sh),
            11 | 43 => Some(37 + sh),
            _ => None,
        };
        if let Some(e) = end {
            let b = |i| be(s4, e + i, 1);
            valid = ymd(be(s4, e, 2), b(2), b(3), b(4), b(5), b(6));
            key.push(s4.get(e + 12).copied().unwrap_or(255)); // type of statistical processing
        }
    }
    key.extend(&s3[5..]);
    Field { key, ed: 2, param: (disc, s4[9], s4[10]), lev_type, level, member, reft, valid, grid: s3.clone(), pack: be(s5, 9, 2) as u16, centre: be(s1, 5, 2) as u16, loc }
}

/// The field of a GRIB 1 message at `start` (length `n`). The chunk is the message after section 0.
fn grib1(src: &mut Win, idx: u32, start: u64, n: u64, out: &mut Vec<Field>) -> Result<()> {
    let pl = be(&src.read(start + 8..start + 11)?, 0, 3);
    let pds = src.read(start + 8..start + 8 + pl.max(28))?;
    if pds[7] & 0x80 == 0 {
        return Err("GRIB 1 message without grid description (predefined grids are not supported)".into());
    }
    let gl = be(&src.read(start + 8 + pl..start + 11 + pl)?, 0, 3);
    let grid = src.read(start + 8 + pl..start + 8 + pl + gl.max(32))?;
    let b = |i: usize| be(&pds, i, 1);
    let reft = ymd((b(24).max(1) - 1) * 100 + b(12), b(13), b(14), b(15), b(16), 0);
    // Valid time: the time range indicator says which period P1 or P2 is (GRIB 1 table 5).
    let (p1, p2) = (b(18) as f64, b(19) as f64);
    let ft = match b(20) {
        2..=5 => p2,
        10 => be(&pds, 18, 2) as f64,
        _ => p1,
    };
    let valid = reft + unit(pds[17], 1).map_or(0.0, |u| ft * u);
    let lev_type = pds[9];
    // Level types with one 2-octet value. Others are layers: the level is the top of the layer.
    let level = match lev_type {
        100 | 103 | 105 | 107 | 109 | 111 | 113 | 115 | 117 | 119 | 125 | 160 => be(&pds, 10, 2) as f64,
        1..=9 | 102 | 200 | 201 => f64::NAN,
        _ => pds[10] as f64,
    };
    let mut key = vec![pds[3], pds[8], lev_type, pds[20]];
    key.extend(&grid[3..]);
    let loc = ChunkLoc { src: idx, off: start + 8, len: n - 12 };
    out.push(Field { key, ed: 1, param: (pds[3], 0, pds[8]), lev_type, level, member: f64::NAN, reft, valid, grid, pack: 0, centre: pds[4] as u16, loc });
    Ok(())
}

/// The grid of a variable: size, georeferencing and a description for the user.
struct Grid {
    ni: u64,
    nj: u64,
    /// The points in the j direction are adjacent (scanning mode bit 3).
    column_major: bool,
    georef: Georef,
    desc: String,
}

/// The grid of section 3 (GRIB 2) or of the GDS (GRIB 1). None for a reduced grid or a spectral field.
fn grid(ed: u8, s: &[u8]) -> Option<Grid> {
    // Positions of the values in the two editions: (template, Ni, Nj, La1, Lo1, La2, Lo2, Di, Dj or N,
    // scanning mode). GRIB 1 angles are in millidegrees, on 3 octets.
    let (tpl, ni, nj, ang, k) = if ed == 2 {
        let tpl = be(s, 12, 2);
        let (b, sub) = (be(s, 38, 4), be(s, 42, 4));
        let unit = if b == 0 || b == 0xFFFF_FFFF || sub == 0 || sub == 0xFFFF_FFFF { 1e-6 } else { b as f64 / sub as f64 };
        if tpl >= 50 && tpl != 90 {
            return None;
        }
        (tpl, be(s, 30, 4), be(s, 34, 4), [46, 50, 55, 59, 63, 67, 71], unit)
    } else {
        // GRIB 1 table 6 to the GRIB 2 templates: 0 lat/lon, 4 Gaussian. Others have no georeferencing.
        let tpl = match be(s, 5, 1) {
            0 => 0,
            4 => 40,
            50..=90 => return None,
            t => 1000 + t,
        };
        (tpl, be(s, 6, 2), be(s, 8, 2), [10, 13, 17, 20, 23, 25, 27], 1e-3)
    };
    // A reduced grid has no number of points along a row.
    let (w, missing) = if ed == 2 { (4, 0xFFFF_FFFF) } else { (2, 0xFFFF) };
    if ni == 0 || nj == 0 || ni == missing || nj == missing {
        return None;
    }
    // Angles: 4 octets in GRIB 2, 3 octets in GRIB 1.
    let a = |i: usize| sm(s, ang[i], w.max(3)) as f64 * k;
    let incr = |i: usize| Some(be(s, ang[i], w)).filter(|&v| v != missing && v != 0).map(|v| v as f64 * k);
    let scan = match (ed, tpl) {
        (_, 0 | 1 | 40) | (1, _) => ang[6],
        (2, 10) => 59,
        (2, 20 | 30) => 64,
        (2, 90) => 63,
        _ => usize::MAX,
    };
    let scan = s.get(scan).copied().unwrap_or(0);
    let column_major = scan & 0x20 != 0;
    let (lo1, lo2) = (a(1), a(3));
    // A grid that starts east of 180 degrees starts at a negative longitude: -20, not 340. A global grid from 0
    // to 360 stays (the viewer repeats it).
    let lo1 = if lo1 > 180.0 { lo1 - 360.0 } else { lo1 };
    // The increments come from the first and the last points first: Di and Dj can be rounded (to
    // millidegrees in GRIB 1), as in ecCodes.
    let step = |span: f64, n: u64| Some(span / (n.max(2) - 1) as f64).filter(|d| *d > 0.0);
    let di = step(if scan & 0x80 != 0 { lo1 - lo2 } else { lo2 - lo1 }.rem_euclid(360.0), ni).or(incr(4)).unwrap_or(1.0);
    let dx = if scan & 0x80 != 0 { -di } else { di };
    let (la1, la2) = (a(0), a(2));
    let size = format!("{ni} x {nj}");
    Some(match tpl {
        0 => {
            let dj = step((la2 - la1).abs(), nj).or(incr(5)).unwrap_or(1.0);
            let dy = if scan & 0x40 != 0 { dj } else { -dj };
            let gt = [lo1 - dx / 2.0, dx, 0.0, la1 - dy / 2.0, 0.0, dy];
            Grid { ni, nj, column_major, georef: Georef::Affine { gt, crs: Crs::wgs84() }, desc: format!("regular latitude/longitude, {size}, {di} x {dj} degrees") }
        }
        40 => {
            // Regular Gaussian grid: the rows are at the Gaussian latitudes, from the one nearest to La1.
            let nlat = be(s, ang[5], w) as usize;
            let mut lats = gauss(nlat);
            if scan & 0x40 != 0 {
                lats.reverse();
            }
            let j0 = lats.iter().enumerate().min_by(|x, y| (x.1 - la1).abs().total_cmp(&(y.1 - la1).abs())).map_or(0, |x| x.0);
            let lats = lats.get(j0..j0 + nj as usize)?;
            let cols: Vec<f64> = (0..=8).map(|c| 0.5 + c as f64 * (ni - 1) as f64 / 8.0).collect();
            let lon = lats.iter().flat_map(|_| cols.iter().map(|c| lo1 + (c - 0.5) * dx)).collect();
            let lat = lats.iter().flat_map(|l| cols.iter().map(move |_| *l)).collect();
            let rows = (0..nj).map(|j| j as f64 + 0.5).collect();
            Grid { ni, nj, column_major, georef: Georef::Grid { cols, rows, lon, lat }, desc: format!("regular Gaussian, N{nlat}, {size}") }
        }
        t => Grid { ni, nj, column_major, georef: Georef::None, desc: format!("template {}, {size} (no georeferencing)", t % 1000) },
    })
}

/// The 2n latitudes of a Gaussian grid with n latitudes between a pole and the equator, from north to
/// south (degrees): the roots of the Legendre polynomial of degree 2n.
fn gauss(n: usize) -> Vec<f64> {
    let m = 2 * n;
    (0..m)
        .map(|i| {
            let mut x = (std::f64::consts::PI * (i as f64 + 0.75) / (m as f64 + 0.5)).cos();
            for _ in 0..100 {
                let (mut p0, mut p1) = (1.0, x);
                for k in 2..=m {
                    (p0, p1) = (p1, ((2 * k - 1) as f64 * x * p1 - (k - 1) as f64 * p0) / k as f64);
                }
                let dx = p1 / (m as f64 * (x * p1 - p0) / (x * x - 1.0));
                x -= dx;
                if dx.abs() < 1e-15 {
                    break;
                }
            }
            x.asin().to_degrees()
        })
        .collect()
}

/// Common parameters of GRIB 2: discipline, category, number, level type (0: all), level (0: all), short
/// name, name, units. The names are those of ecCodes.
type Param = (u8, u8, u8, u8, u16, &'static str, &'static str, &'static str);
const PARAMS: &[Param] = &[
    (0, 0, 0, 103, 2, "2t", "2 metre temperature", "K"),
    (0, 0, 6, 103, 2, "2d", "2 metre dewpoint temperature", "K"),
    (0, 2, 2, 103, 10, "10u", "10 metre U wind component", "m s-1"),
    (0, 2, 3, 103, 10, "10v", "10 metre V wind component", "m s-1"),
    (0, 2, 1, 103, 10, "10si", "10 metre wind speed", "m s-1"),
    (0, 3, 0, 101, 0, "msl", "Mean sea level pressure", "Pa"),
    (0, 3, 0, 1, 0, "sp", "Surface pressure", "Pa"),
    (0, 0, 0, 1, 0, "skt", "Skin temperature", "K"),
    (0, 0, 0, 0, 0, "t", "Temperature", "K"),
    (0, 0, 6, 0, 0, "dpt", "Dew point temperature", "K"),
    (0, 1, 0, 0, 0, "q", "Specific humidity", "kg kg-1"),
    (0, 1, 1, 0, 0, "r", "Relative humidity", "%"),
    (0, 1, 3, 0, 0, "pwat", "Precipitable water", "kg m-2"),
    (0, 1, 8, 0, 0, "tp", "Total precipitation", "kg m-2"),
    (0, 1, 193, 0, 0, "tp", "Total precipitation", "m"),
    (0, 1, 13, 0, 0, "sd", "Snow depth water equivalent", "kg m-2"),
    (0, 2, 1, 0, 0, "ws", "Wind speed", "m s-1"),
    (0, 2, 2, 0, 0, "u", "U component of wind", "m s-1"),
    (0, 2, 3, 0, 0, "v", "V component of wind", "m s-1"),
    (0, 2, 8, 0, 0, "w", "Vertical velocity", "Pa s-1"),
    (0, 2, 12, 0, 0, "vo", "Vorticity (relative)", "s-1"),
    (0, 2, 22, 0, 0, "gust", "Wind speed (gust)", "m s-1"),
    (0, 3, 0, 0, 0, "pres", "Pressure", "Pa"),
    (0, 3, 1, 0, 0, "prmsl", "Pressure reduced to MSL", "Pa"),
    (0, 3, 4, 0, 0, "z", "Geopotential", "m2 s-2"),
    (0, 3, 5, 0, 0, "gh", "Geopotential height", "gpm"),
    (0, 6, 1, 0, 0, "tcc", "Total cloud cover", "%"),
    (0, 7, 6, 0, 0, "cape", "Convective available potential energy", "J kg-1"),
    (0, 19, 0, 0, 0, "vis", "Visibility", "m"),
    (10, 0, 3, 0, 0, "swh", "Significant height of combined wind waves and swell", "m"),
    (10, 3, 0, 0, 0, "wtmp", "Water temperature", "K"),
    (2, 0, 0, 0, 0, "lsm", "Land-sea mask", "Proportion"),
];

/// GRIB 1 parameters: table version (2: WMO tables 1 to 3, 128: ECMWF), number, short name, name, units.
const PARAMS1: &[(u8, u8, &str, &str, &str)] = &[
    (2, 1, "pres", "Pressure", "Pa"),
    (2, 2, "prmsl", "Pressure reduced to MSL", "Pa"),
    (2, 7, "gh", "Geopotential height", "gpm"),
    (2, 11, "t", "Temperature", "K"),
    (2, 33, "u", "U component of wind", "m s-1"),
    (2, 34, "v", "V component of wind", "m s-1"),
    (2, 51, "q", "Specific humidity", "kg kg-1"),
    (2, 52, "r", "Relative humidity", "%"),
    (2, 61, "tp", "Total precipitation", "kg m-2"),
    (2, 71, "tcc", "Total cloud cover", "%"),
    (128, 129, "z", "Geopotential", "m2 s-2"),
    (128, 130, "t", "Temperature", "K"),
    (128, 131, "u", "U component of wind", "m s-1"),
    (128, 132, "v", "V component of wind", "m s-1"),
    (128, 133, "q", "Specific humidity", "kg kg-1"),
    (128, 134, "sp", "Surface pressure", "Pa"),
    (128, 151, "msl", "Mean sea level pressure", "Pa"),
    (128, 157, "r", "Relative humidity", "%"),
    (128, 164, "tcc", "Total cloud cover", "(0 - 1)"),
    (128, 165, "10u", "10 metre U wind component", "m s-1"),
    (128, 166, "10v", "10 metre V wind component", "m s-1"),
    (128, 167, "2t", "2 metre temperature", "K"),
    (128, 168, "2d", "2 metre dewpoint temperature", "K"),
    (128, 228, "tp", "Total precipitation", "m"),
    (128, 235, "skt", "Skin temperature", "K"),
];

/// Short name, name and units of the parameter of a field.
fn param(f: &Field) -> (String, String, String) {
    let (d, c, n) = f.param;
    let hit = if f.ed == 2 {
        PARAMS.iter().find(|p| (p.0, p.1, p.2) == (d, c, n) && (p.3 == 0 || (p.3 == f.lev_type && (p.4 == 0 || p.4 as f64 == f.level)))).map(|p| (p.5, p.6, p.7))
    } else {
        let t = if (1..=3).contains(&d) { 2 } else { d };
        PARAMS1.iter().find(|p| (p.0, p.1) == (t, n)).map(|p| (p.2, p.3, p.4))
    };
    match hit {
        Some((s, l, u)) => (s.into(), l.into(), u.into()),
        None if f.ed == 2 => (format!("{d}_{c}_{n}"), format!("discipline {d}, category {c}, number {n}"), String::new()),
        None => (format!("var{n}_t{d}"), format!("table {d}, parameter {n}"), String::new()),
    }
}

/// Name of a level type (GRIB 2 code table 4.5, GRIB 1 table 3), as ecCodes names it.
fn level_name(ed: u8, t: u8) -> String {
    let s = match (ed, t) {
        (_, 1) => "surface",
        (_, 100) => "isobaricInhPa",
        (2, 101) | (1, 102) => "meanSea",
        (2, 103) | (1, 105) => "heightAboveGround",
        (2, 105) | (1, 109) => "hybrid",
        (2, 106) | (1, 112) => "depthBelowLandLayer",
        (2, 107) | (1, 113) => "theta",
        (2, 160) | (1, 160) => "depthBelowSea",
        (_, 200) => "entireAtmosphere",
        (_, 8) => "nominalTop",
        _ => return format!("level{t}"),
    };
    s.into()
}

fn centre(c: u16) -> String {
    let s = match c {
        7 => "kwbc (NCEP)",
        34 => "rjtd (JMA)",
        54 => "cwao (CMC)",
        74 => "egrr (Met Office)",
        78 => "edzw (DWD)",
        85 => "lfpw (Météo-France)",
        98 => "ecmf (ECMWF)",
        _ => "",
    };
    format!("{c} {s}").trim().into()
}

fn packing(ed: u8, t: u16) -> String {
    if ed == 1 {
        return "simple (GRIB 1)".into();
    }
    let s = match t {
        0 => "simple",
        2 => "complex",
        3 => "complex with spatial differencing",
        4 => "IEEE floating point",
        40 => "JPEG 2000",
        41 => "PNG",
        42 => "CCSDS",
        _ => "not supported",
    };
    format!("{s} (template 5.{t})")
}

/// Bit reader, most significant bit first.
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    /// The 64 bits at the position (zeros after the end).
    fn peek(&self) -> u64 {
        let i = self.pos / 8;
        let mut w = [0u8; 8];
        if i < self.b.len() {
            let n = (self.b.len() - i).min(8);
            w[..n].copy_from_slice(&self.b[i..i + n]);
        }
        u64::from_be_bytes(w) << (self.pos % 8)
    }

    /// The next `n` bits (at most 56) as an unsigned integer.
    fn get(&mut self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        let v = self.peek() >> (64 - n);
        self.pos += n as usize;
        v
    }

    /// Fundamental sequence (CCSDS): the number of 0 bits before a 1 bit.
    fn fs(&mut self) -> u64 {
        let mut n = 0;
        while self.pos < self.b.len() * 8 {
            let z = self.peek().leading_zeros().min(56) as usize;
            self.pos += z;
            n += z as u64;
            if z < 56 {
                self.pos += 1;
                break;
            }
        }
        n
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// Decode a GRIB chunk into the f32 values of the grid (NaN: no data).
pub fn decode(a: &Array, s: &[u8], edition: u8) -> Result<Vec<u8>> {
    let n = a.chunk_bytes() / 4;
    let v = if edition == 2 { decode2(s, n)? } else { decode1(s, n)? };
    Ok(v.iter().flat_map(|x| x.to_le_bytes()).collect())
}

/// Spread the packed values on the points of the bitmap.
fn spread(vals: Vec<f32>, bitmap: Option<&[u8]>, n: usize) -> Result<Vec<f32>> {
    let Some(bm) = bitmap else {
        return if vals.len() >= n { Ok(vals[..n].to_vec()) } else { Err(format!("{} values for {n} points", vals.len()).into()) };
    };
    let mut it = vals.into_iter();
    (0..n).map(|i| if bm.get(i / 8).is_some_and(|b| b & (0x80 >> (i % 8)) != 0) { it.next().ok_or_else(|| Error("bitmap has more points than the data".into())) } else { Ok(f32::NAN) }).collect()
}

/// GRIB 2: sections 5, 6 and 7 of a field (and maybe the sections of an earlier field with the bitmap).
fn decode2(s: &[u8], n: usize) -> Result<Vec<f32>> {
    let (mut s5, mut bitmap, mut s7, mut p) = (&[][..], None, None, 0);
    while p + 5 <= s.len() {
        let len = be(s, p, 4) as usize;
        let sec = s.get(p..p + len).filter(|_| len >= 5).ok_or("truncated GRIB 2 section")?;
        match sec[4] {
            5 => s5 = sec,
            6 if sec[5] == 0 => bitmap = Some(&sec[6..]),
            6 if sec[5] != 254 => bitmap = None,
            7 => s7 = Some(&sec[5..]),
            _ => {}
        }
        p += len;
    }
    let d = s7.ok_or("GRIB 2 chunk without section 7")?;
    if s5.len() < 12 {
        return Err("GRIB 2 chunk without section 5".into());
    }
    let np = be(s5, 5, 4) as usize;
    let tpl = be(s5, 9, 2);
    let r = f32::from_bits(be(s5, 11, 4) as u32) as f64;
    let (e, dd, nbits) = (sm(s5, 15, 2) as i32, sm(s5, 17, 2) as i32, be(s5, 19, 1) as u32);
    if nbits > 32 {
        return Err(format!("GRIB 2: {nbits} bits per value").into());
    }
    let scale = |x: f64| ((r + x * 2f64.powi(e)) / 10f64.powi(dd)) as f32;
    let vals: Vec<f32> = match tpl {
        4 => match s5[11] {
            1 => d.as_chunks::<4>().0.iter().take(np).map(|b| f32::from_be_bytes(*b)).collect(),
            2 => d.as_chunks::<8>().0.iter().take(np).map(|b| f64::from_be_bytes(*b) as f32).collect(),
            k => return Err(format!("GRIB 2 IEEE precision {k} is not supported").into()),
        },
        0 | 2 | 3 | 40 | 41 | 42 if nbits == 0 => vec![scale(0.0); np],
        0 => {
            let mut b = Bits { b: d, pos: 0 };
            (0..np).map(|_| scale(b.get(nbits) as f64)).collect()
        }
        2 | 3 => complex(s5, d, np)?.into_iter().map(|x| x.map_or(f32::NAN, |x| scale(x as f64))).collect(),
        40 => {
            let img = jpeg2k::Image::from_bytes(d).map_err(|e| Error(format!("GRIB 2 JPEG 2000: {e}")))?;
            let c = img.components().first().ok_or("GRIB 2 JPEG 2000 without component")?;
            c.data().iter().take(np).map(|&x| scale(x as f64)).collect()
        }
        41 => png(d)?.into_iter().take(np).map(|x| scale(x as f64)).collect(),
        42 => aec(d, nbits, be(s5, 21, 1) as u8, be(s5, 22, 1) as usize, be(s5, 23, 2) as usize, np)?.into_iter().map(|x| scale(x as f64)).collect(),
        t => return Err(format!("GRIB 2 packing template 5.{t} is not supported").into()),
    };
    if vals.len() < np {
        return Err(format!("GRIB 2: {} values, {np} expected", vals.len()).into());
    }
    spread(vals, bitmap, n)
}

/// Complex packing (templates 5.2 and 5.3): groups of values, each with a reference and a bit width. None
/// is a missing value.
fn complex(s5: &[u8], d: &[u8], np: usize) -> Result<Vec<Option<i64>>> {
    if s5.len() < 47 {
        return Err("GRIB 2 complex packing: section 5 is too short".into());
    }
    let nbits = s5[19] as u32;
    let miss = s5[22];
    let ng = be(s5, 31, 4) as usize;
    let (wref, wbits) = (s5[35] as u64, s5[36] as u32);
    let (lref, linc, llast, lbits) = (be(s5, 37, 4), s5[41] as u64, be(s5, 42, 4), s5[46] as u32);
    let (order, nd) = if be(s5, 9, 2) == 3 { (s5[47] as usize, s5[48] as u32 * 8) } else { (0, 0) };
    if ng > np || order > 2 {
        return Err("bad GRIB 2 complex packing".into());
    }
    let mut b = Bits { b: d, pos: 0 };
    // Spatial differencing: the first values and the minimum of the differences, sign and magnitude.
    let mut sd = || {
        let v = b.get(nd) as i64;
        if nd > 0 && v >> (nd - 1) & 1 == 1 { -(v & ((1 << (nd - 1)) - 1)) } else { v }
    };
    let first: Vec<i64> = (0..order).map(|_| sd()).collect();
    let min = if order > 0 { sd() } else { 0 };
    b.align();
    let refs: Vec<u64> = (0..ng).map(|_| b.get(nbits)).collect();
    b.align();
    let widths: Vec<u32> = (0..ng).map(|_| (wref + b.get(wbits)) as u32).collect();
    if widths.iter().any(|&w| w > 32) {
        return Err("bad GRIB 2 complex packing: group width of more than 32 bits".into());
    }
    b.align();
    let mut lens: Vec<u64> = (0..ng).map(|_| lref + b.get(lbits) * linc).collect();
    if let Some(l) = lens.last_mut() {
        *l = llast;
    }
    b.align();
    let mut x = Vec::with_capacity(np);
    let ones = |w: u32| (1u64 << w) - 1;
    for g in 0..ng {
        let w = widths[g];
        for _ in 0..lens[g] {
            let v = b.get(w);
            let m = if w == 0 { refs[g] } else { v };
            let bits = if w == 0 { nbits } else { w };
            let missing = (miss >= 1 && m == ones(bits)) || (miss == 2 && m == ones(bits) - 1);
            x.push((!missing).then_some((refs[g] + v) as i64));
        }
    }
    x.truncate(np);
    if nd > 0 {
        let (mut p1, mut p2) = (0, 0);
        for (k, v) in x.iter_mut().flatten().enumerate() {
            *v = match (order, k) {
                (_, 0) => first[0],
                (2, 1) => first[1],
                (1, _) => *v + min + p1,
                _ => *v + min + 2 * p1 - p2,
            };
            (p2, p1) = (p1, *v);
        }
    }
    Ok(x)
}

/// PNG packing (template 5.41): the values are the pixels of a PNG image (grey or RGB(A) with 8 bits for
/// each channel). The bits of a pixel can be more than the bits of the values in section 5.
fn png(d: &[u8]) -> Result<Vec<u64>> {
    let err = |e: png::DecodingError| Error(format!("GRIB 2 PNG: {e}"));
    let mut r = png::Decoder::new(std::io::Cursor::new(d)).read_info().map_err(err)?;
    let mut buf = vec![0; r.output_buffer_size().ok_or("GRIB 2 PNG too large")?];
    let nbits = r.info().color_type.samples() as u32 * r.info().bit_depth as u32;
    if nbits > 32 {
        return Err("GRIB 2 PNG with more than 32 bits per pixel".into());
    }
    let info = r.next_frame(&mut buf).map_err(err)?;
    let mut out = Vec::with_capacity(info.width as usize * info.height as usize);
    for row in buf[..info.buffer_size()].chunks(info.line_size) {
        let mut b = Bits { b: row, pos: 0 };
        out.extend((0..info.width).map(|_| b.get(nbits)));
    }
    Ok(out)
}

/// CCSDS adaptive entropy decoding (template 5.42, as libaec). `flags`: the libaec flags (8: the unit delay
/// predictor, 32: each reference sample interval ends at a byte). `block`: samples in a block. `rsi`: blocks
/// in a reference sample interval.
fn aec(d: &[u8], bps: u32, flags: u8, block: usize, rsi: usize, n: usize) -> Result<Vec<u64>> {
    if !(1..=32).contains(&bps) || block == 0 || rsi == 0 || flags & 17 != 0 {
        return Err(format!("CCSDS: {bps} bits per sample, flags {flags} are not supported").into());
    }
    let pre = flags & 8 != 0;
    let id_len = if bps > 16 { 5 } else if bps > 8 { 4 } else { 3 };
    let xmax = (1u64 << bps) - 1;
    let mut b = Bits { b: d, pos: 0 };
    let mut out: Vec<u64> = Vec::with_capacity(n + rsi * block);
    while out.len() < n {
        if b.pos >= d.len() * 8 {
            return Err("CCSDS data is too short".into());
        }
        let seg = out.len();
        let mut k = 0;
        while k < rsi && out.len() < n {
            let r = (k == 0 && pre) as usize;
            let id = b.get(id_len);
            if id == 0 {
                let se = b.get(1) == 1;
                if r == 1 {
                    out.push(b.get(bps));
                }
                if se {
                    // Second extension: pairs of samples from one code.
                    let mut i = r;
                    while i < block {
                        let m = b.fs();
                        let beta = ((((8 * m + 1) as f64).sqrt() - 1.0) / 2.0) as u64;
                        let d1 = m - beta * (beta + 1) / 2;
                        if i.is_multiple_of(2) {
                            out.push(beta - d1);
                            i += 1;
                        }
                        out.push(d1);
                        i += 1;
                    }
                    k += 1;
                } else {
                    // Zero blocks. 5 (ROS): to the end of the segment or of a group of 64 blocks.
                    let mut z = b.fs() as usize + 1;
                    if z == 5 {
                        z = (rsi - k).min(64 - k % 64);
                    } else if z > 5 {
                        z -= 1;
                    }
                    out.resize(out.len() + z * block - r, 0);
                    k += z;
                }
            } else if id == (1 << id_len) - 1 {
                out.extend((0..block).map(|_| b.get(bps)));
                k += 1;
            } else {
                // Split: the fundamental sequences of all samples, then their low bits.
                if r == 1 {
                    out.push(b.get(bps));
                }
                let fs: Vec<u64> = (r..block).map(|_| b.fs()).collect();
                out.extend(fs.into_iter().map(|f| f << (id - 1) | b.get(id as u32 - 1)));
                k += 1;
            }
        }
        if pre {
            // Unit delay predictor: the first sample is the reference, the others are mapped differences.
            let med = xmax / 2 + 1;
            let mut x = out[seg];
            for v in &mut out[seg + 1..] {
                let half = (*v >> 1) + (*v & 1);
                let mask = if x & med != 0 { xmax } else { 0 };
                x = if half <= (mask ^ x) { if *v & 1 == 0 { x + (*v >> 1) } else { x - half } } else { mask ^ *v };
                *v = x;
            }
        }
        if flags & 32 != 0 {
            b.align();
        }
    }
    out.truncate(n);
    Ok(out)
}

/// IBM single precision floating point number (GRIB 1 reference value).
fn ibm(b: &[u8]) -> f64 {
    let (s, e, m) = (b[0] >> 7, (b[0] & 0x7F) as i32, be(b, 1, 3) as f64);
    let v = m * 16f64.powi(e - 64) / 16_777_216.0;
    if s == 1 { -v } else { v }
}

/// GRIB 1: the message after section 0 (PDS, GDS, BMS, BDS). Simple packing of grid point data.
fn decode1(s: &[u8], n: usize) -> Result<Vec<f32>> {
    let sec = |p: usize| be(s, p, 3) as usize;
    let flags = *s.get(7).ok_or("truncated GRIB 1 message")?;
    let dd = sm(s, 26, 2) as i32;
    let mut p = sec(0);
    if flags & 0x80 != 0 {
        p += sec(p);
    }
    let mut bitmap = None;
    if flags & 0x40 != 0 {
        if be(s, p + 4, 2) != 0 {
            return Err("GRIB 1 predefined bitmap is not supported".into());
        }
        bitmap = Some(s.get(p + 6..p + sec(p)).ok_or("truncated GRIB 1 bitmap")?);
        p += sec(p);
    }
    let bds = s.get(p..p + sec(p).max(11)).ok_or("truncated GRIB 1 data")?;
    if bds[3] & 0xC0 != 0 {
        return Err("GRIB 1 spherical harmonics or second-order packing is not supported".into());
    }
    let (e, r, nbits) = (sm(bds, 4, 2) as i32, ibm(&bds[6..10]), bds[10] as u32);
    if nbits > 32 {
        return Err(format!("GRIB 1: {nbits} bits per value").into());
    }
    let np = bitmap.map_or(n, |bm| (0..n).filter(|i| bm.get(i / 8).is_some_and(|b| b & (0x80 >> (i % 8)) != 0)).count());
    let mut b = Bits { b: &bds[11..], pos: 0 };
    let vals = (0..np).map(|_| ((r + b.get(nbits) as f64 * 2f64.powi(e)) / 10f64.powi(dd)) as f32).collect();
    spread(vals, bitmap, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaussian_latitudes() {
        // N1: the roots of P2 are +-1/sqrt(3). tests/grib.rs compares N32 with ecCodes.
        let g = gauss(1);
        assert!((g[0] - (1.0f64 / 3f64.sqrt()).asin().to_degrees()).abs() < 1e-12 && (g[0] + g[1]).abs() < 1e-12);
    }

    #[test]
    fn aec_decodes() {
        // 8 samples of 8 bits, block 8, one segment. Uncompressed block (id 7), without preprocessing.
        let enc = |bits: &[u8]| bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |a, (i, b)| a | b << (7 - i))).collect::<Vec<u8>>();
        let v: Vec<u64> = (10..18).collect();
        let mut bits: Vec<u8> = vec![1, 1, 1];
        for x in &v {
            bits.extend((0..8).rev().map(|k| (x >> k & 1) as u8));
        }
        assert_eq!(aec(&enc(&bits), 8, 0, 8, 1, 8).unwrap(), v);
        // Split block, k = 1 (id 2), with preprocessing: reference 100, then differences +1 +1 -1 (mapped 2 2 1)...
        let mut bits = vec![0, 1, 0];
        bits.extend((0..8).rev().map(|k| (100u64 >> k & 1) as u8));
        let mapped = [2u64, 2, 1, 0, 0, 3, 4];
        for m in mapped {
            bits.extend(std::iter::repeat_n(0, (m >> 1) as usize));
            bits.push(1);
        }
        for m in mapped {
            bits.push((m & 1) as u8);
        }
        assert_eq!(aec(&enc(&bits), 8, 8, 8, 1, 8).unwrap(), [100, 101, 102, 101, 101, 101, 99, 101]);
    }
}
