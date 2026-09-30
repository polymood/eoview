//! Chunk decoders. The code comes from radview.
use eo_core::{Array, Codec, Error, Result};
use std::borrow::Cow;

/// Largest decoded chunk.
pub const MAX_CHUNK: usize = 1 << 30;

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
    let mut out = vec![0u8; need];
    let mut first = true;
    for c in &a.codecs {
        let res = match c {
            Codec::Predictor { kind, stride, row } => {
                if first {
                    let n = src.len().min(need);
                    out[..n].copy_from_slice(&src[..n]);
                }
                let rb = *row as usize * a.dtype.part().size();
                for r in out.chunks_exact_mut(rb) {
                    unpredict(*kind, a.dtype.part().size(), *stride as usize, a.le, r);
                }
                Ok(())
            }
            _ if !first => Err(Error(format!("{c:?} must be the first codec"))),
            Codec::Deflate => {
                libdeflater::Decompressor::new().zlib_decompress(src, &mut out).map(drop).map_err(|e| Error(e.to_string()))
            }
            Codec::Zstd => zstd::bulk::decompress_to_buffer(src, &mut out).map(drop).map_err(|e| Error(e.to_string())),
            Codec::Lzw => lzw(src, &mut out),
            Codec::PackBits => {
                packbits(src, &mut out);
                Ok(())
            }
        };
        res.map_err(|e| Error(format!("{c:?}: {e}")))?;
        first = false;
    }
    if first {
        let n = src.len().min(need);
        out[..n].copy_from_slice(&src[..n]);
    }
    Ok(Cow::Owned(out))
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

fn packbits(src: &[u8], out: &mut [u8]) {
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
