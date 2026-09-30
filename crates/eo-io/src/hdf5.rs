//! HDF5 metadata reader (NetCDF-4 files are HDF5). It finds the datasets, their attributes and the byte
//! ranges of their chunks. It does not read data: the chunk engine reads and decodes the chunks in parallel.
//! This avoids the global lock of the HDF5 C library.
//!
//! Specification: HDF5 File Format Specification Version 3.0,
//! <https://support.hdfgroup.org/documentation/hdf5/latest/_f_m_t3.html>.
//!
//! Supported: superblock versions 0 to 3, object headers versions 1 and 2, groups with symbol tables,
//! compact links or dense links, compact or dense attributes (fractal heap with a direct root block or one
//! indirect level, version 2 B-tree of depth 0), data layout versions 3 and 4 (contiguous; chunked with a
//! version 1 B-tree or a single chunk), filters deflate, shuffle, Fletcher-32 and Zstd, attributes of
//! numbers and fixed-length strings.
use crate::source::Source;
use eo_core::*;
use std::collections::HashMap;
use std::sync::Mutex;

const BLOCK: u64 = 64 << 10;

#[derive(Clone, Debug, PartialEq)]
pub enum Attr {
    Num(Vec<f64>),
    Text(String),
}

/// A dataset: its path, shape, type, attributes and chunk locations.
#[derive(Clone, Debug)]
pub struct Dataset {
    pub path: String,
    pub shape: Vec<u64>,
    pub dtype: DType,
    pub le: bool,
    pub chunk: Vec<u64>,
    /// (chunk position in elements, file offset, size in bytes, filter mask).
    pub chunks: Vec<(Vec<u64>, u64, u64, u32)>,
    pub codecs: Vec<Codec>,
    pub attrs: HashMap<String, Attr>,
}

/// Reader of the file, with a cache of aligned blocks.
struct Rd<'a> {
    src: &'a Source,
    len: u64,
    so: usize,
    sl: usize,
    blocks: Mutex<HashMap<u64, bytes::Bytes>>,
}

impl Rd<'_> {
    fn bytes(&self, off: u64, n: u64) -> Result<bytes::Bytes> {
        let end = off.checked_add(n).filter(|&e| e <= self.len).ok_or("HDF5 structure is after end of file")?;
        if self.src.is_local() {
            return self.src.read(off..end);
        }
        let (b0, b1) = (off / BLOCK, (end.max(1) - 1) / BLOCK);
        if b0 != b1 {
            return self.src.read(off..end);
        }
        let mut m = self.blocks.lock().unwrap();
        if !m.contains_key(&b0) {
            m.insert(b0, self.src.read(b0 * BLOCK..((b0 + 1) * BLOCK).min(self.len))?);
        }
        Ok(m[&b0].slice((off - b0 * BLOCK) as usize..(end - b0 * BLOCK) as usize))
    }
}

/// Cursor over a byte slice, little-endian.
struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl Cur<'_> {
    fn u(&mut self, n: usize) -> Result<u64> {
        let s = self.b.get(self.p..self.p + n).ok_or("truncated HDF5 message")?;
        self.p += n;
        Ok(s.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64))
    }
    fn skip(&mut self, n: usize) {
        self.p += n;
    }
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let s = self.b.get(self.p..self.p + n).ok_or("truncated HDF5 message")?;
        self.p += n;
        Ok(s)
    }
}

/// Undefined address.
fn undef(a: u64, so: usize) -> bool {
    a == if so == 8 { u64::MAX } else { (1u64 << (8 * so)) - 1 }
}

/// Messages of an object header: (type, data).
fn messages(r: &Rd, addr: u64) -> Result<Vec<(u16, Vec<u8>)>> {
    let head = r.bytes(addr, 16.min(r.len - addr))?;
    let mut out = vec![];
    if &head[..4] == b"OHDR" {
        let flags = head[5];
        let mut p = 6;
        if flags & 0x20 != 0 {
            p += 16;
        }
        if flags & 0x10 != 0 {
            p += 4;
        }
        let sz = 1usize << (flags & 3);
        let b = r.bytes(addr, (p + sz) as u64)?;
        let n = Cur { b: &b, p }.u(sz)?;
        let mut blocks = vec![(addr + (p + sz) as u64, n)];
        while let Some((a, n)) = blocks.pop() {
            let b = r.bytes(a, n)?;
            let mut c = Cur { b: &b, p: 0 };
            // Message header: type, size, flags, and the creation order if the header tracks it.
            let hsz = if flags & 0x04 != 0 { 6 } else { 4 };
            // A gap smaller than a message header can end the block.
            while c.p + hsz <= b.len() {
                let t = c.u(1)? as u16;
                let size = c.u(2)? as usize;
                c.skip(hsz - 3);
                let Ok(data) = c.take(size).map(<[u8]>::to_vec) else { break };
                if t == 0x10 {
                    let mut d = Cur { b: &data, p: 0 };
                    let (o, l) = (d.u(r.so)?, d.u(r.sl)?);
                    // "OCHK" signature, messages, checksum.
                    blocks.push((o + 4, l.saturating_sub(8)));
                } else {
                    out.push((t, data));
                }
            }
        }
    } else if head[0] == 1 {
        let nmsg = u16::from_le_bytes([head[2], head[3]]) as usize;
        let size = u32::from_le_bytes(head[8..12].try_into().unwrap()) as u64;
        let mut blocks = vec![(addr + 16, size)];
        let mut seen = 0;
        while let Some((a, n)) = blocks.pop() {
            let b = r.bytes(a, n)?;
            let mut c = Cur { b: &b, p: 0 };
            while c.p + 8 <= b.len() && seen < nmsg {
                let t = c.u(2)? as u16;
                let size = c.u(2)? as usize;
                c.skip(4);
                let data = c.take(size)?.to_vec();
                seen += 1;
                if t == 0x10 {
                    let mut d = Cur { b: &data, p: 0 };
                    blocks.push((d.u(r.so)?, d.u(r.sl)?));
                } else {
                    out.push((t, data));
                }
            }
        }
    } else {
        return Err(format!("unknown HDF5 object header at {addr}").into());
    }
    Ok(out)
}

/// Datatype message: (numeric type, little-endian, size). None for other classes.
fn datatype(b: &[u8]) -> Option<(Option<DType>, bool, usize)> {
    let class = b.first()? & 0x0F;
    let bits = *b.get(1)?;
    let size = u32::from_le_bytes(b.get(4..8)?.try_into().ok()?) as usize;
    let le = bits & 1 == 0;
    let t = match (class, bits & 0x08 != 0, size) {
        (0, false, 1) => Some(DType::U8),
        (0, true, 1) => Some(DType::I8),
        (0, false, 2) => Some(DType::U16),
        (0, true, 2) => Some(DType::I16),
        (0, false, 4) => Some(DType::U32),
        (0, true, 4) => Some(DType::I32),
        (0, false, 8) => Some(DType::U64),
        (0, true, 8) => Some(DType::I64),
        (1, _, 4) => Some(DType::F32),
        (1, _, 8) => Some(DType::F64),
        (3, _, _) => None,
        _ => return Some((None, le, size)),
    };
    Some((t, le, size))
}

fn dataspace(b: &[u8]) -> Option<Vec<u64>> {
    let (ver, rank) = (*b.first()?, *b.get(1)? as usize);
    let p = if ver == 1 { 8 } else { 4 };
    (0..rank).map(|i| Some(u64::from_le_bytes(b.get(p + 8 * i..p + 8 * i + 8)?.try_into().ok()?))).collect()
}

fn attribute(b: &[u8]) -> Option<(String, Attr)> {
    let ver = *b.first()?;
    let name_len = u16::from_le_bytes(b.get(2..4)?.try_into().ok()?) as usize;
    let dt_len = u16::from_le_bytes(b.get(4..6)?.try_into().ok()?) as usize;
    let ds_len = u16::from_le_bytes(b.get(6..8)?.try_into().ok()?) as usize;
    let pad = |n: usize| if ver == 1 { n.div_ceil(8) * 8 } else { n };
    let mut p = if ver == 3 { 9 } else { 8 };
    let name = String::from_utf8_lossy(b.get(p..p + name_len)?).trim_end_matches('\0').to_string();
    p += pad(name_len);
    let dt = b.get(p..p + dt_len)?;
    p += pad(dt_len);
    let ds = b.get(p..p + ds_len)?;
    p += pad(ds_len);
    let n: u64 = dataspace(ds).map_or(1, |d| d.iter().product());
    let (t, le, size) = datatype(dt)?;
    let data = b.get(p..)?;
    if dt[0] & 0x0F == 3 {
        let s = data.get(..size * n as usize)?;
        return Some((name, Attr::Text(String::from_utf8_lossy(s).trim_end_matches('\0').trim().to_string())));
    }
    let t = t?;
    let vals = (0..n as usize)
        .map(|i| {
            let v = data.get(i * size..(i + 1) * size)?;
            let mut w = [0u8; 8];
            if le {
                w[..size].copy_from_slice(v)
            } else {
                v.iter().rev().enumerate().for_each(|(k, &x)| w[k] = x)
            }
            let u = u64::from_le_bytes(w);
            Some(match t {
                DType::U8 | DType::U16 | DType::U32 | DType::U64 => u as f64,
                DType::I8 => u as u8 as i8 as f64,
                DType::I16 => u as u16 as i16 as f64,
                DType::I32 => u as u32 as i32 as f64,
                DType::I64 => u as i64 as f64,
                DType::F32 => f32::from_bits(u as u32) as f64,
                _ => f64::from_bits(u),
            })
        })
        .collect::<Option<Vec<f64>>>()?;
    Some((name, Attr::Num(vals)))
}

/// Leaf entries of a version 1 B-tree of chunks: (chunk position, address, size, filter mask).
fn chunk_btree(r: &Rd, addr: u64, rank: usize, out: &mut Vec<(Vec<u64>, u64, u64, u32)>) -> Result<()> {
    let hs = 8 + 2 * r.so;
    let head = r.bytes(addr, hs as u64)?;
    if &head[..4] != b"TREE" || head[4] != 1 {
        return Err("bad HDF5 chunk B-tree node".into());
    }
    let (level, n) = (head[5], u16::from_le_bytes([head[6], head[7]]) as usize);
    let ks = 8 + 8 * (rank + 1);
    let b = r.bytes(addr + hs as u64, (n * (ks + r.so) + ks) as u64)?;
    let mut c = Cur { b: &b, p: 0 };
    for _ in 0..n {
        let size = c.u(4)?;
        let mask = c.u(4)? as u32;
        let pos: Vec<u64> = (0..rank).map(|_| c.u(8)).collect::<Result<_>>()?;
        c.skip(8);
        let child = c.u(r.so)?;
        if level == 0 {
            out.push((pos, child, size, mask));
        } else {
            chunk_btree(r, child, rank, out)?;
        }
    }
    Ok(())
}

fn filters(b: &[u8], size: usize) -> Result<Vec<Codec>> {
    let (ver, n) = (b[0], b[1] as usize);
    let mut c = Cur { b, p: if ver == 1 { 8 } else { 2 } };
    let mut out = vec![];
    for _ in 0..n {
        let id = c.u(2)?;
        let name_len = if ver == 1 || id >= 256 { c.u(2)? as usize } else { 0 };
        let _flags = c.u(2)?;
        let nv = c.u(2)? as usize;
        c.skip(if ver == 1 { name_len.div_ceil(8) * 8 } else { name_len });
        c.skip(4 * nv);
        if ver == 1 && nv % 2 == 1 {
            c.skip(4);
        }
        out.push(match id {
            1 => Codec::Deflate,
            2 => Codec::Shuffle { size: size as u32 },
            3 => Codec::Checksum,
            32015 => Codec::Zstd,
            _ => return Err(format!("HDF5 filter {id} is not supported").into()),
        });
    }
    // The pipeline lists the filters in the order of the writer: decode in the reverse order.
    out.reverse();
    Ok(out)
}

/// All datasets of the file (recursive), with at least `min_rank` dimensions.
pub fn datasets(src: &Source, min_rank: usize) -> Result<Vec<Dataset>> {
    let len = src.len()?;
    let mut base = 0;
    let sig = [0x89, b'H', b'D', b'F', b'\r', b'\n', 0x1a, b'\n'];
    loop {
        if src.read(base..(base + 8).min(len))?[..] == sig {
            break;
        }
        base = if base == 0 { 512 } else { base * 2 };
        if base >= len {
            return Err("not an HDF5 file".into());
        }
    }
    let sb = src.read(base..(base + 128).min(len))?;
    let ver = sb[8];
    let mut r = Rd { src, len, so: 8, sl: 8, blocks: Default::default() };
    let root = if ver >= 2 {
        (r.so, r.sl) = (sb[9] as usize, sb[10] as usize);
        let mut c = Cur { b: &sb, p: 12 };
        let (_base, _ext, _eof) = (c.u(r.so)?, c.u(r.so)?, c.u(r.so)?);
        c.u(r.so)?
    } else {
        (r.so, r.sl) = (sb[13] as usize, sb[14] as usize);
        let mut c = Cur { b: &sb, p: if ver == 0 { 24 } else { 28 } };
        c.skip(4 * r.so);
        // Root group symbol table entry: link name offset, object header address.
        c.skip(r.so);
        c.u(r.so)?
    };
    let mut out = vec![];
    group(&r, root + base, "", min_rank, &mut out, 0)?;
    Ok(out)
}

fn group(r: &Rd, addr: u64, path: &str, min_rank: usize, out: &mut Vec<Dataset>, depth: usize) -> Result<()> {
    if depth > 16 {
        return Ok(());
    }
    let msgs = messages(r, addr).map_err(|e| Error(format!("object header at {addr}: {e}")))?;
    let mut links: Vec<(String, u64)> = vec![];
    for (t, d) in &msgs {
        match t {
            0x06 => links.extend(link(r, d)?),
            0x11 => {
                let mut c = Cur { b: d, p: 0 };
                let (bt, heap) = (c.u(r.so)?, c.u(r.so)?);
                symbol_table(r, bt, heap, &mut links)?;
            }
            0x02 => {
                let mut c = Cur { b: d, p: 1 };
                let flags = c.u(1)?;
                if flags & 1 != 0 {
                    c.skip(8);
                }
                let (heap, bt) = (c.u(r.so)?, c.u(r.so)?);
                if !undef(heap, r.so) {
                    for m in dense(r, heap, bt, 5)? {
                        links.extend(link(r, &m)?);
                    }
                }
            }
            _ => {}
        }
    }
    if links.is_empty() && msgs.iter().any(|m| m.0 == 0x08) {
        if let Some(d) = dataset(r, &msgs, path)?.filter(|d| d.shape.len() >= min_rank) {
            out.push(d);
        }
        return Ok(());
    }
    for (name, a) in links {
        let p = if path.is_empty() { name } else { format!("{path}/{name}") };
        if let Err(e) = group(r, a, &p, min_rank, out, depth + 1) {
            eprintln!("HDF5 {p}: {e}");
        }
    }
    Ok(())
}

/// Hard link of a link message: (name, object header address).
fn link(r: &Rd, d: &[u8]) -> Result<Option<(String, u64)>> {
    let mut c = Cur { b: d, p: 1 };
    let flags = c.u(1)?;
    let lt = if flags & 0x08 != 0 { c.u(1)? } else { 0 };
    if flags & 0x04 != 0 {
        c.skip(8);
    }
    if flags & 0x10 != 0 {
        c.skip(1);
    }
    let n = c.u(1 << (flags & 3))? as usize;
    let name = String::from_utf8_lossy(c.take(n)?).to_string();
    Ok((lt == 0).then_some((name, c.u(r.so)?)))
}

/// Number of bytes to encode `v` (H5VM_limit_enc_size).
fn enc_size(v: u64) -> usize {
    (63 - v.max(1).leading_zeros() as usize) / 8 + 1
}

/// Objects of a fractal heap, in the order of a version 2 B-tree of records of type `kind`
/// (5: links by name, 8: attributes by name). The objects are link or attribute messages.
fn dense(r: &Rd, heap: u64, bt: u64, kind: u8) -> Result<Vec<Vec<u8>>> {
    let h = r.bytes(heap, 256.min(r.len - heap))?;
    if &h[..4] != b"FRHP" {
        return Err("bad HDF5 fractal heap".into());
    }
    let mut c = Cur { b: &h, p: 5 };
    let id_len = c.u(2)? as usize;
    let filt_len = c.u(2)? as usize;
    let flags = c.u(1)?;
    let max_obj = c.u(4)?;
    c.skip(r.sl + r.so + r.sl + r.so + r.sl * 8);
    let width = c.u(2)?;
    let start = c.u(r.sl)?;
    let max_direct = c.u(r.sl)?;
    let max_bits = c.u(2)? as usize;
    let _start_rows = c.u(2)?;
    let root = c.u(r.so)?;
    let rows = c.u(2)?;
    let off_size = max_bits.div_ceil(8);
    let len_size = enc_size(max_direct).min(enc_size(max_obj));
    // Direct block (heap offset of its start, file address) that contains heap offset `off`.
    let block_of = |off: u64| -> Result<(u64, u64)> {
        if rows == 0 {
            return Ok((0, root));
        }
        let ib = r.bytes(root, 5 + r.so as u64 + off_size as u64)?;
        if &ib[..4] != b"FHIB" {
            return Err("bad HDF5 fractal heap indirect block".into());
        }
        let entry = r.so + if filt_len > 0 { r.sl + 4 } else { 0 };
        let first = 5 + r.so + off_size;
        let (mut row_off, mut k) = (0u64, 0u64);
        for row in 0..rows {
            let size = if row < 2 { start } else { start << (row - 1) };
            if size > max_direct {
                break;
            }
            for col in 0..width {
                let (b0, b1) = (row_off + col * size, row_off + (col + 1) * size);
                if off >= b0 && off < b1 {
                    let e = r.bytes(root + first as u64 + (k * entry as u64), r.so as u64)?;
                    return Ok((b0, Cur { b: &e, p: 0 }.u(r.so)?));
                }
                k += 1;
            }
            row_off += width * size;
        }
        Err("HDF5 fractal heap object in a nested indirect block is not supported".into())
    };
    let _ = flags;
    // Version 2 B-tree: header, then a root leaf (depth 0) or a root internal node with leaves (depth 1).
    let mut internal_records: Vec<Vec<u8>> = vec![];
    let b = r.bytes(bt, 40.min(r.len - bt))?;
    if &b[..4] != b"BTHD" {
        return Err("bad HDF5 version 2 B-tree".into());
    }
    let mut c = Cur { b: &b, p: 6 };
    let node = c.u(4)? as usize;
    let rec = c.u(2)? as usize;
    let depth = c.u(2)?;
    c.skip(2);
    let (rootn, nrec) = (c.u(r.so)?, c.u(2)? as usize);
    // Leaves: (address, number of records). A root of depth 1 lists its leaves after its records.
    let leaves = match depth {
        0 => vec![(rootn, nrec)],
        1 => {
            let nsize = enc_size(((node - 10) / rec) as u64);
            let ib = r.bytes(rootn, (6 + nrec * rec + (nrec + 1) * (r.so + nsize)) as u64)?;
            if &ib[..4] != b"BTIN" {
                return Err("bad HDF5 version 2 B-tree internal node".into());
            }
            let mut c = Cur { b: &ib, p: 6 + nrec * rec };
            let mut v = vec![];
            for _ in 0..=nrec {
                v.push((c.u(r.so)?, c.u(nsize)? as usize));
            }
            // The records of the internal node are objects too.
            let mut own = vec![];
            for k in 0..nrec {
                own.push(ib[6 + k * rec..6 + (k + 1) * rec].to_vec());
            }
            v.push((u64::MAX, own.len()));
            internal_records = own;
            v
        }
        _ => return Err("HDF5 version 2 B-tree with depth > 1 is not supported".into()),
    };
    let mut records: Vec<Vec<u8>> = vec![];
    for (addr, n) in leaves {
        if addr == u64::MAX {
            records.append(&mut internal_records);
            continue;
        }
        let leaf = r.bytes(addr, (6 + n * rec) as u64)?;
        if &leaf[..4] != b"BTLF" {
            return Err("bad HDF5 version 2 B-tree leaf".into());
        }
        records.extend((0..n).map(|k| leaf[6 + k * rec..6 + (k + 1) * rec].to_vec()));
    }
    let id_at = if kind == 5 { 4 } else { 0 };
    let mut out = vec![];
    for rcd in &records {
        let id = rcd.get(id_at..id_at + id_len).ok_or("bad HDF5 B-tree record")?;
        match (id[0] >> 4) & 3 {
            0 => {
                let mut c = Cur { b: &id[1..], p: 0 };
                let (off, len) = (c.u(off_size)?, c.u(len_size)?);
                let (b0, addr) = block_of(off)?;
                out.push(r.bytes(addr + off - b0, len)?.to_vec());
            }
            2 => {
                let n = (id[0] & 0x0F) as usize + 1;
                out.push(id.get(1..1 + n).ok_or("bad tiny heap object")?.to_vec());
            }
            _ => return Err("HDF5 huge heap objects are not supported".into()),
        }
    }
    Ok(out)
}

/// Links of a version 1 group: symbol table B-tree and local heap.
fn symbol_table(r: &Rd, bt: u64, heap: u64, links: &mut Vec<(String, u64)>) -> Result<()> {
    let h = r.bytes(heap, 8 + 2 * r.sl as u64 + r.so as u64)?;
    if &h[..4] != b"HEAP" {
        return Err("bad HDF5 local heap".into());
    }
    let mut c = Cur { b: &h, p: 8 };
    let (size, _free) = (c.u(r.sl)?, c.u(r.sl)?);
    let data = r.bytes(c.u(r.so)?, size)?;
    let mut nodes = vec![bt];
    while let Some(n) = nodes.pop() {
        let hs = 8 + 2 * r.so;
        let head = r.bytes(n, hs as u64)?;
        if &head[..4] == b"TREE" {
            let (level, k) = (head[5], u16::from_le_bytes([head[6], head[7]]) as usize);
            let b = r.bytes(n + hs as u64, (k * (r.sl + r.so) + r.sl) as u64)?;
            let mut c = Cur { b: &b, p: 0 };
            for _ in 0..k {
                c.skip(r.sl);
                let child = c.u(r.so)?;
                if level > 0 {
                    nodes.push(child);
                } else {
                    nodes.push(child | 1 << 63);
                }
            }
        } else if n >> 63 == 1 || &head[..4] == b"SNOD" {
            let a = n & !(1 << 63);
            let sh = r.bytes(a, 8)?;
            if &sh[..4] != b"SNOD" {
                continue;
            }
            let k = u16::from_le_bytes([sh[6], sh[7]]) as usize;
            let es = 2 * r.so + 24;
            let b = r.bytes(a + 8, (k * es) as u64)?;
            for e in b.chunks_exact(es) {
                let mut c = Cur { b: e, p: 0 };
                let (name_off, oh) = (c.u(r.so)? as usize, c.u(r.so)?);
                let end = data[name_off..].iter().position(|&x| x == 0).map_or(data.len(), |p| name_off + p);
                links.push((String::from_utf8_lossy(&data[name_off..end]).to_string(), oh));
            }
        }
    }
    Ok(())
}

fn dataset(r: &Rd, msgs: &[(u16, Vec<u8>)], path: &str) -> Result<Option<Dataset>> {
    let get = |t: u16| msgs.iter().find(|m| m.0 == t).map(|m| &m.1[..]);
    let Some(shape) = get(0x01).and_then(dataspace) else { return Ok(None) };
    let Some((Some(dtype), le, size)) = get(0x03).and_then(datatype) else { return Ok(None) };
    let lay = get(0x08).ok_or("dataset without layout")?;
    let rank = shape.len();
    let codecs = get(0x0B).map(|f| filters(f, size)).transpose().map_err(|e| Error(format!("filter pipeline: {e}")))?.unwrap_or_default();
    let mut attrs: HashMap<String, Attr> = msgs.iter().filter(|m| m.0 == 0x0C).filter_map(|m| attribute(&m.1)).collect();
    // Dense attribute storage (attribute info message).
    if let Some(d) = get(0x15) {
        let mut c = Cur { b: d, p: 1 };
        let flags = c.u(1)?;
        if flags & 1 != 0 {
            c.skip(2);
        }
        let (heap, bt) = (c.u(r.so)?, c.u(r.so)?);
        if !undef(heap, r.so) {
            attrs.extend(dense(r, heap, bt, 8)?.iter().filter_map(|m| attribute(m)));
        }
    }
    let (ver, class) = (lay[0], lay[1]);
    let lay_err = |e: Error| Error(format!("layout {ver}/{class}: {e}"));
    let mut c = Cur { b: lay, p: 2 };
    let (chunk, chunks) = (|| -> Result<(Vec<u64>, Vec<(Vec<u64>, u64, u64, u32)>)> { Ok(match (ver, class) {
        (3 | 4, 1) => {
            let (a, l) = (c.u(r.so)?, c.u(r.sl)?);
            let v = if undef(a, r.so) { vec![] } else { vec![(vec![0; rank], a, l, 0)] };
            (shape.clone(), v)
        }
        (3, 2) => {
            let dims = c.u(1)? as usize;
            let bt = c.u(r.so)?;
            let chunk: Vec<u64> = (0..dims - 1).map(|_| c.u(4)).collect::<Result<_>>()?;
            let mut v = vec![];
            if !undef(bt, r.so) {
                chunk_btree(r, bt, rank, &mut v)?;
            }
            (chunk, v)
        }
        (4, 2) => {
            let flags = c.u(1)?;
            let dims = c.u(1)? as usize;
            let enc = c.u(1)? as usize;
            let chunk: Vec<u64> = (0..dims - 1).map(|_| c.u(enc)).collect::<Result<_>>()?;
            c.u(enc)?;
            let index = c.u(1)?;
            match index {
                1 => {
                    let (size, mask) = if flags & 2 != 0 { (Some(c.u(r.sl)?), c.u(4)? as u32) } else { (None, 0) };
                    let a = c.u(r.so)?;
                    let n = size.unwrap_or(chunk.iter().product::<u64>() * dtype.size() as u64);
                    (chunk, if undef(a, r.so) { vec![] } else { vec![(vec![0; rank], a, n, mask)] })
                }
                i => return Err(format!("HDF5 chunk index type {i} is not supported").into()),
            }
        }
        _ => return Err(format!("HDF5 data layout {ver}/{class} is not supported").into()),
    }) })().map_err(lay_err)?;
    Ok(Some(Dataset { path: path.to_string(), shape, dtype, le, chunk, chunks, codecs, attrs }))
}

impl Dataset {
    pub fn num(&self, k: &str) -> Option<f64> {
        match self.attrs.get(k)? {
            Attr::Num(v) => v.first().copied(),
            Attr::Text(t) => t.parse().ok(),
        }
    }

    pub fn text(&self, k: &str) -> Option<&str> {
        match self.attrs.get(k)? {
            Attr::Text(t) => Some(t),
            _ => None,
        }
    }

    /// Chunked array. Chunks with a filter mask (a filter not applied) are an error. `src` is the source index.
    pub fn array(&self, src: u32) -> Result<Array> {
        let n = self.shape.len();
        let grid: Vec<u64> = self.shape.iter().zip(&self.chunk).map(|(s, c)| s.div_ceil(*c)).collect();
        let mut chunks = vec![ChunkLoc { src, off: 0, len: 0 }; grid.iter().product::<u64>() as usize];
        for (pos, a, l, mask) in &self.chunks {
            if *mask != 0 {
                return Err("HDF5 chunks with skipped filters are not supported".into());
            }
            let i = pos.iter().zip(&self.chunk).zip(&grid).fold(0u64, |i, ((p, c), g)| i * g + p / c) as usize;
            if let Some(c) = chunks.get_mut(i) {
                *c = ChunkLoc { src, off: *a, len: *l };
            }
        }
        let dims = (0..n)
            .map(|i| match n - 1 - i {
                0 => "x".to_string(),
                1 => "y".to_string(),
                _ => format!("dim_{i}"),
            })
            .collect();
        Ok(Array { dims, shape: self.shape.clone(), chunk: self.chunk.clone(), dtype: self.dtype, le: self.le, codecs: self.codecs.clone(), chunks, place: None })
    }
}
