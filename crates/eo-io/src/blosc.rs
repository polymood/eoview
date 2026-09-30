//! Blosc 1 decoder: compressors LZ4, zlib and Zstd, byte shuffle and bit shuffle.
//! Format: <https://github.com/Blosc/c-blosc/blob/main/README_CHUNK_FORMAT.rst>.
//! Bit shuffle: <https://github.com/kiyo-masui/bitshuffle> (the scalar algorithm of bitshuffle_core.c).
use eo_core::{Error, Result};

const SHUFFLE: u8 = 0x1;
const MEMCPYED: u8 = 0x2;
const BITSHUFFLE: u8 = 0x4;
const NOSPLIT: u8 = 0x10;

fn u32le(b: &[u8], i: usize) -> Result<usize> {
    let s = b.get(i..i + 4).ok_or("truncated Blosc data")?;
    Ok(u32::from_le_bytes(s.try_into().unwrap()) as usize)
}

/// Decode a Blosc container. The result has the size in the header.
pub fn decode(src: &[u8]) -> Result<Vec<u8>> {
    if src.len() < 16 {
        return Err("truncated Blosc header".into());
    }
    let (flags, ts) = (src[2], (src[3] as usize).max(1));
    let (nbytes, blocksize) = (u32le(src, 4)?, u32le(src, 8)?);
    if flags & MEMCPYED != 0 {
        return src.get(16..16 + nbytes).map(<[u8]>::to_vec).ok_or_else(|| "truncated Blosc data".into());
    }
    if blocksize == 0 {
        return Ok(vec![]);
    }
    let comp = flags >> 5;
    let nblocks = nbytes.div_ceil(blocksize);
    let mut out = vec![0u8; nbytes];
    let mut tmp = vec![0u8; blocksize];
    for b in 0..nblocks {
        let start = u32le(src, 16 + 4 * b)?;
        let bsize = blocksize.min(nbytes - b * blocksize);
        let leftover = bsize < blocksize;
        let split = flags & NOSPLIT == 0 && ts <= 16 && blocksize / ts >= 128 && !leftover;
        let nsplits = if split { ts } else { 1 };
        let neblock = bsize / nsplits;
        let mut p = start;
        for s in 0..nsplits {
            let cb = u32le(src, p)?;
            p += 4;
            let data = src.get(p..p + cb).ok_or("truncated Blosc block")?;
            let dst = &mut tmp[s * neblock..(s + 1) * neblock];
            if cb == neblock {
                dst.copy_from_slice(data);
            } else {
                let n = match comp {
                    1 => lz4_flex::block::decompress_into(data, dst).map_err(|e| Error(format!("Blosc LZ4: {e}")))?,
                    3 => libdeflater::Decompressor::new().zlib_decompress(data, dst).map_err(|e| Error(format!("Blosc zlib: {e}")))?,
                    4 => zstd::bulk::decompress_to_buffer(data, dst).map_err(|e| Error(format!("Blosc Zstd: {e}")))?,
                    c => return Err(format!("Blosc compressor {c} is not supported (LZ4, zlib, Zstd)").into()),
                };
                if n != neblock {
                    return Err("Blosc block has a wrong size".into());
                }
            }
            p += cb;
        }
        let dst = &mut out[b * blocksize..b * blocksize + bsize];
        if flags & BITSHUFFLE != 0 && ts > 1 && (bsize / ts) % 8 == 0 {
            bitunshuffle(&tmp[..bsize], dst, bsize / ts, ts);
        } else if flags & SHUFFLE != 0 && ts > 1 {
            unshuffle(&tmp[..bsize], dst, ts);
        } else {
            dst.copy_from_slice(&tmp[..bsize]);
        }
    }
    Ok(out)
}

/// Byte unshuffle: `src` has the first bytes of all values, then the second bytes, ...
pub fn unshuffle(src: &[u8], dst: &mut [u8], ts: usize) {
    let n = src.len() / ts;
    for (b, plane) in src[..n * ts].chunks_exact(n).enumerate() {
        for (i, &v) in plane.iter().enumerate() {
            dst[i * ts + b] = v;
        }
    }
    dst[n * ts..].copy_from_slice(&src[n * ts..]);
}

/// Transpose the bits of 8 bytes (a 8 x 8 bit matrix in a little-endian u64).
fn trans8x8(mut x: u64) -> u64 {
    let mut t = (x ^ (x >> 7)) & 0x00AA_00AA_00AA_00AA;
    x = x ^ t ^ (t << 7);
    t = (x ^ (x >> 14)) & 0x0000_CCCC_0000_CCCC;
    x = x ^ t ^ (t << 14);
    t = (x ^ (x >> 28)) & 0x0000_0000_F0F0_F0F0;
    x ^ t ^ (t << 28)
}

/// Bit unshuffle of `n` values of `ts` bytes. `n` is a multiple of 8.
fn bitunshuffle(src: &[u8], dst: &mut [u8], n: usize, ts: usize) {
    // bshuf_trans_byte_bitrow: bit rows back to groups of 8 bytes.
    let row = n / 8;
    let mut tmp = vec![0u8; n * ts];
    for j in 0..ts {
        for i in 0..row {
            for k in 0..8 {
                tmp[i * 8 * ts + j * 8 + k] = src[(j * 8 + k) * row + i];
            }
        }
    }
    // bshuf_shuffle_bit_eightelem: transpose the bits of each group of 8 values.
    let nbyte = n * ts;
    for j in (0..8 * ts).step_by(8) {
        for i in (0..nbyte).step_by(8 * ts) {
            let mut x = trans8x8(u64::from_le_bytes(tmp[i + j..i + j + 8].try_into().unwrap()));
            for k in 0..8 {
                dst[i + j / 8 + k * ts] = x as u8;
                x >>= 8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitshuffle_round_trip() {
        // Reference shuffle (bshuf_trans_bit_elem, scalar), then our unshuffle.
        let (n, ts) = (16usize, 2usize);
        let v: Vec<u8> = (0..n * ts).map(|i| (i * 37 + 11) as u8).collect();
        // Bit plane p = byte b, bit k of each value; plane bits are in value order, LSB first in each byte.
        let mut enc = vec![0u8; n * ts];
        for b in 0..ts {
            for k in 0..8 {
                for i in 0..n {
                    let bit = (v[i * ts + b] >> k) & 1;
                    enc[(b * 8 + k) * (n / 8) + i / 8] |= bit << (i % 8);
                }
            }
        }
        let mut dec = vec![0u8; n * ts];
        bitunshuffle(&enc, &mut dec, n, ts);
        assert_eq!(dec, v);
    }
}
