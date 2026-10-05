//! Chunk decoders. The code comes from radview.
use eo_core::{Array, Codec, Error, Result};
use std::ops::Add;
use std::borrow::Cow;

/// Largest decoded chunk.
pub const MAX_CHUNK: usize = 1 << 30;

/// Values of the decoded bytes `d` of a real array, as f64. For small arrays (coordinates).
pub fn to_f64(a: &Array, d: &[u8]) -> Vec<f64> {
    use eo_core::DType::*;
    let es = a.dtype.size();
    d.chunks_exact(es)
        .map(|e| {
            let mut w = [0u8; 8];
            if a.le { w[..es].copy_from_slice(e) } else { e.iter().rev().enumerate().for_each(|(k, &x)| w[k] = x) }
            let u = u64::from_le_bytes(w);
            match a.dtype {
                F32 => f32::from_bits(u as u32) as f64,
                F64 => f64::from_bits(u),
                I8 => u as u8 as i8 as f64,
                I16 => u as u16 as i16 as f64,
                I32 => u as u32 as i32 as f64,
                I64 => u as i64 as f64,
                _ => u as f64,
            }
        })
        .collect()
}

/// Decode the encoded bytes of one chunk of `a`. The result has exactly `a.chunk_bytes()` bytes.
/// A short chunk (for example the last TIFF strip) gets zeros at the end: the decoders accept
/// an output buffer that is larger than the data.
/// Uncompressed data is not copied.
pub fn decode<'a>(a: &Array, src: &'a [u8]) -> Result<Cow<'a, [u8]>> {
    let need = a.chunk_bytes();
    if a.codecs.is_empty() && src.len() >= need {
        return Ok(Cow::Borrowed(&src[..need]));
    }
    // ponytail: a chunk decodes into one buffer. Decode large compressed strips by rows if a product needs it.
    if need > MAX_CHUNK {
        return Err(Error(format!("chunk of {} MB is too large to decode (limit {} MB)", need >> 20, MAX_CHUNK >> 20)));
    }
    let mut cur: Cow<[u8]> = Cow::Borrowed(src);
    for c in &a.codecs {
        cur = step(a, c, cur, need).map_err(|e| Error(format!("{c:?}: {e}")))?;
    }
    let mut out = cur.into_owned();
    out.resize(need, 0);
    Ok(Cow::Owned(out))
}

/// Output buffer of a decompressor: `need` bytes, then cut to the decoded size.
fn inflate(need: usize, f: impl FnOnce(&mut [u8]) -> std::result::Result<usize, String>) -> Result<Vec<u8>> {
    let mut out = vec![0u8; need];
    let n = f(&mut out)?;
    out.truncate(n);
    Ok(out)
}

fn step<'a>(a: &Array, c: &Codec, cur: Cow<'a, [u8]>, need: usize) -> Result<Cow<'a, [u8]>> {
    let s = &cur[..];
    let es = a.dtype.part().size();
    let owned = |v: Vec<u8>| Ok(Cow::Owned(v));
    match c {
        Codec::Deflate => owned(inflate(need, |o| libdeflater::Decompressor::new().zlib_decompress(s, o).map_err(|e| e.to_string()))?),
        Codec::Gzip => owned(inflate(need, |o| libdeflater::Decompressor::new().gzip_decompress(s, o).map_err(|e| e.to_string()))?),
        Codec::Zstd => owned(inflate(need, |o| zstd::bulk::decompress_to_buffer(s, o).map_err(|e| e.to_string()))?),
        Codec::Lzw => owned(inflate(need, |o| lzw(s, o).map(|_| o.len()).map_err(|e| e.0))?),
        Codec::PackBits => owned(inflate(need, |o| Ok(packbits(s, o)))?),
        Codec::Lz4 { header } => {
            let s = if *header { s.get(4..).ok_or("truncated LZ4 data")? } else { s };
            owned(inflate(need, |o| lz4_flex::block::decompress_into(s, o).map_err(|e| e.to_string()))?)
        }
        Codec::Blosc => owned(crate::blosc::decode(s)?),
        Codec::Shuffle { size } => {
            let mut o = vec![0u8; s.len()];
            crate::blosc::unshuffle(s, &mut o, *size as usize);
            owned(o)
        }
        Codec::Checksum => Ok(match cur {
            Cow::Borrowed(b) => Cow::Borrowed(&b[..b.len().saturating_sub(4)]),
            Cow::Owned(mut v) => {
                v.truncate(v.len().saturating_sub(4));
                Cow::Owned(v)
            }
        }),
        Codec::Delta => {
            let mut v = cur.into_owned();
            v.resize(need, 0);
            delta(a, &mut v);
            owned(v)
        }
        Codec::Predictor { kind, stride, row } => {
            let mut v = cur.into_owned();
            v.resize(need, 0);
            for r in v.chunks_exact_mut(*row as usize * es) {
                unpredict(*kind, es, *stride as usize, a.le, r);
            }
            owned(v)
        }
        Codec::Jpeg2000 { reduce, header } => owned(crate::jp2::decode_tile(a, s, *reduce, header)?),
        Codec::Jpeg { tables } => owned(jpeg(a, s, tables)?),
    }
}

/// Decode a JPEG chunk (u8, 1 or 3 values per pixel, pixel interleaved). With TIFF JPEGTables, the stream is
/// the tables (without their end marker) and the chunk (without its start marker), as libtiff does.
fn jpeg(a: &Array, src: &[u8], tables: &[u8]) -> Result<Vec<u8>> {
    use zune_jpeg::zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions};
    let stream: Cow<[u8]> = if tables.len() > 4 && src.len() > 2 {
        Cow::Owned([&tables[..tables.len() - 2], &src[2..]].concat())
    } else {
        Cow::Borrowed(src)
    };
    let nb = a.axis("band").map_or(1, |b| a.chunk[b] as usize);
    let cs = if nb == 1 { ColorSpace::Luma } else { ColorSpace::RGB };
    let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&stream[..]), DecoderOptions::default().jpeg_set_out_colorspace(cs));
    let px = d.decode().map_err(|e| Error(format!("{e:?}")))?;
    let info = d.info().ok_or("JPEG without header")?;
    let (w, h) = (info.width as usize, info.height as usize);
    let (y, x) = (a.axis("y").unwrap(), a.axis("x").unwrap());
    let (ch, cw) = (a.chunk[y] as usize, a.chunk[x] as usize);
    let mut out = vec![0u8; a.chunk_bytes()];
    let row = w.min(cw) * nb;
    for r in 0..h.min(ch) {
        out[r * cw * nb..][..row].copy_from_slice(&px[r * w * nb..][..row]);
    }
    Ok(out)
}

/// Undo the numcodecs delta filter: cumulative sum of the values.
fn delta(a: &Array, v: &mut [u8]) {
    macro_rules! go {
        ($t:ty, $add:ident) => {{
            const N: usize = std::mem::size_of::<$t>();
            let (g, p): (fn([u8; N]) -> $t, fn($t) -> [u8; N]) =
                if a.le { (<$t>::from_le_bytes, <$t>::to_le_bytes) } else { (<$t>::from_be_bytes, <$t>::to_be_bytes) };
            let mut acc = <$t>::default();
            for c in v.chunks_exact_mut(N) {
                acc = acc.$add(g(c.try_into().unwrap()));
                c.copy_from_slice(&p(acc));
            }
        }};
    }
    use eo_core::DType::*;
    match a.dtype.part() {
        U8 => go!(u8, wrapping_add),
        I8 => go!(i8, wrapping_add),
        U16 => go!(u16, wrapping_add),
        I16 => go!(i16, wrapping_add),
        U32 => go!(u32, wrapping_add),
        I32 => go!(i32, wrapping_add),
        U64 => go!(u64, wrapping_add),
        I64 => go!(i64, wrapping_add),
        F32 => go!(f32, add),
        _ => go!(f64, add),
    }
}

fn lzw(src: &[u8], out: &mut [u8]) -> Result<()> {
    let mut d = weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
    let (mut a, mut b) = (0, 0);
    loop {
        let r = d.decode_bytes(&src[a..], &mut out[b..]);
        (a, b) = (a + r.consumed_in, b + r.consumed_out);
        match r.status {
            Ok(weezl::LzwStatus::Ok) if b < out.len() && r.consumed_in + r.consumed_out > 0 => continue,
            s => return s.map(drop).map_err(|e| Error(e.to_string())),
        }
    }
}

/// Undo a TIFF predictor on one row. `s` is the value size, `n` the values per pixel.
fn unpredict(kind: u16, s: usize, n: usize, le: bool, row: &mut [u8]) {
    macro_rules! diff {
        ($t:ty) => {{
            const N: usize = std::mem::size_of::<$t>();
            let (g, p): (fn([u8; N]) -> $t, fn($t) -> [u8; N]) =
                if le { (<$t>::from_le_bytes, <$t>::to_le_bytes) } else { (<$t>::from_be_bytes, <$t>::to_be_bytes) };
            for i in n..row.len() / N {
                let a = g(row[N * i..N * i + N].try_into().unwrap());
                let b = g(row[N * (i - n)..N * (i - n) + N].try_into().unwrap());
                row[N * i..N * i + N].copy_from_slice(&p(a.wrapping_add(b)));
            }
        }};
    }
    match (kind, s) {
        (2, 1) => (n..row.len()).for_each(|i| row[i] = row[i].wrapping_add(row[i - n])),
        (2, 2) => diff!(u16),
        (2, 4) => diff!(u32),
        (2, 8) => diff!(u64),
        (3, _) => {
            // Floating point predictor: byte differences, then byte planes (MSB plane first).
            (n..row.len()).for_each(|i| row[i] = row[i].wrapping_add(row[i - n]));
            let tmp = row.to_vec();
            let wc = row.len() / s;
            for j in 0..wc {
                for k in 0..s {
                    let plane = if le { s - 1 - k } else { k };
                    row[j * s + k] = tmp[plane * wc + j];
                }
            }
        }
        _ => {}
    }
}

fn packbits(src: &[u8], out: &mut [u8]) -> usize {
    let (mut i, mut o) = (0, 0);
    while i < src.len() && o < out.len() {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let k = (n as usize + 1).min(src.len() - i).min(out.len() - o);
            out[o..o + k].copy_from_slice(&src[i..i + k]);
            i += n as usize + 1;
            o += k;
        } else if n != -128 && i < src.len() {
            let k = ((1 - n as isize) as usize).min(out.len() - o);
            out[o..o + k].fill(src[i]);
            i += 1;
            o += k;
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packbits_decodes() {
        let src = [0xFEu8, 0xAA, 0x02, 0x80, 0x00, 0x2A];
        let mut out = [0u8; 6];
        packbits(&src, &mut out);
        assert_eq!(out, [0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A]);
    }

    #[test]
    fn predictor2_u16() {
        let mut row: Vec<u8> = [1u16, 1, 1, 65535].iter().flat_map(|v| v.to_le_bytes()).collect();
        unpredict(2, 2, 1, true, &mut row);
        let v: Vec<u16> = row.chunks(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(v, [1, 2, 3, 2]);
    }
}
