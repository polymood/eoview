//! SMOS products in the Earth Explorer format: an XML header (.HDR) and a binary data block (.DBL), little-
//! endian. The data block is a list of grid points of the DGG (ISEA 4H9 grid, about 15 km).
//!
//! The record layouts come from the binX schemas of the data blocks (SMOS L1 and L2 product specifications,
//! SO-TN-IDR-GS-0005 and SO-TN-IDR-GS-0006), and the record sizes are checked on the sample products:
//! - L2 user data products (MIR_SMUDP2 0300, soil moisture; MIR_OSUDP2 0200, ocean salinity): u32 number of
//!   grid points, then records of a fixed size (221 and 190 bytes).
//! - L1C browse (MIR_BWLF1C, MIR_BWSF1C 0300) and science (MIR_SCLF1C, MIR_SCSF1C 0300) products: for a
//!   science product, a u32 number of snapshots and the snapshot records (166 bytes), then a u32 number of
//!   grid points, then for each grid point a header of 17 bytes, a counter (u8 browse, u16 science) and the
//!   brightness temperature records (14 bytes browse, 28 bytes science).
//!
//! Each field of a grid point is a 1D variable. The latitude and the longitude make images of the other
//! variables (`points`). Not supported: the brightness temperatures of L1C products (their number changes
//! from grid point to grid point), the other SMOS products and versions, a zipped product (open the .HDR or
//! the .DBL of the unzipped product).
use crate::{Dataset, Source};
use eo_core::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The fields of the L2 records: type and name.
const SMUDP2: &str = "u4 Grid_Point_ID f4 Latitude f4 Longitude f4 Altitude i4 Days u4 Seconds u4 Microseconds f4 Soil_Moisture f4 Soil_Moisture_DQX f4 Optical_Thickness_Nad f4 Optical_Thickness_Nad_DQX f4 Surface_Temperature f4 Surface_Temperature_DQX f4 TTH f4 TTH_DQX f4 RTT f4 RTT_DQX f4 Scattering_Albedo_H f4 Scattering_Albedo_H_DQX f4 DIFF_Albedos f4 DIFF_Albedos_DQX f4 Roughness_Param f4 Roughness_Param_DQX f4 Dielect_Const_MD_RE f4 Dielect_Const_MD_RE_DQX f4 Dielect_Const_MD_IM f4 Dielect_Const_MD_IM_DQX f4 Dielect_Const_Non_MD_RE f4 Dielect_Const_Non_MD_RE_DQX f4 Dielect_Const_Non_MD_IM f4 Dielect_Const_Non_MD_IM_DQX f4 TB_ASL_Theta_B_H f4 TB_ASL_Theta_B_H_DQX f4 TB_ASL_Theta_B_V f4 TB_ASL_Theta_B_V_DQX f4 TB_TOA_Theta_B_H f4 TB_TOA_Theta_B_H_DQX f4 TB_TOA_Theta_B_V f4 TB_TOA_Theta_B_V_DQX u2 Confidence_Flags u1 GQX u1 Chi_2 u1 Chi_2_P u2 N_Wild u2 M_AVA0 u2 M_AVA f4 AFP u2 N_AF_FOV u2 N_Sun_Tails u2 N_Sun_Glint_Area u2 N_Sun_FOV u2 N_RFI_Mitigations u2 N_Strong_RFI u2 N_Point_Source_RFI u2 N_Tails_Point_Source_RFI u2 N_Software_Error u2 N_Instrument_Error u2 N_ADF_Error u2 N_Calibration_Error u2 N_X_Band u4 Science_Flags u2 N_Sky u2 Processing_Flags u1 S_Tree_1 u1 S_Tree_2 u1 DGG_Current_Flags f4 Tau_Cur_DQX f4 HR_Cur_DQX u2 N_RFI_X u2 N_RFI_Y u1 RFI_Prob";
const OSUDP2: &str = "u4 Grid_Point_ID f4 Latitude f4 Longitude f4 Equiv_ftprt_diam f4 Mean_acq_time f4 SSS1 f4 Sigma_SSS1 f4 SSS2 f4 Sigma_SSS2 f4 SSS3 f4 Sigma_SSS3 f4 A_card f4 Sigma_Acard f4 WS f4 Sigma_WS f4 SST f4 Sigma_SST f4 Tb_42.5H f4 Sigma_Tb_42.5H f4 Tb_42.5V f4 Sigma_Tb_42.5V f4 Tb_42.5X f4 Sigma_Tb_42.5X f4 Tb_42.5Y f4 Sigma_Tb_42.5Y u4 Control_Flags_1 u4 Control_Flags_2 u4 Control_Flags_3 u4 Control_Flags_4 u2 Dg_chi2_1 u2 Dg_chi2_2 u2 Dg_chi2_3 u2 Dg_chi2_Acard u2 Dg_chi2_P_1 u2 Dg_chi2_P_2 u2 Dg_chi2_P_3 u2 Dg_chi2_P_Acard u2 Dg_quality_SSS_1 u2 Dg_quality_SSS_2 u2 Dg_quality_SSS_3 u2 Dg_quality_Acard u1 Dg_num_iter_1 u1 Dg_num_iter_2 u1 Dg_num_iter_3 u1 Dg_num_iter_4 u2 Dg_num_meas_l1c u2 Dg_num_meas_valid u2 Dg_border_fov u2 Dg_eaf_fov u2 Dg_af_fov u2 Dg_sun_tails u2 Dg_sun_glint_area u2 Dg_sun_glint_fov u2 Dg_sun_fov u2 Dg_sun_glint_L2 u2 Dg_Suspect_ice u2 Dg_galactic_Noise_Error u2 Dg_galactic_Noise_Pol u2 Dg_moonglint u4 Science_Flags_1 u4 Science_Flags_2 u4 Science_Flags_3 u4 Science_Flags_4 u2 Dg_sky";
/// The header of an L1C grid point.
const L1C: &str = "u4 Grid_Point_ID f4 Grid_Point_Latitude f4 Grid_Point_Longitude f4 Grid_Point_Altitude u1 Grid_Point_Mask";

/// Records in one chunk of an L2 variable.
const K: u64 = 4096;

fn dtype(t: &str) -> DType {
    match t {
        "u1" => DType::U8,
        "u2" => DType::U16,
        "u4" => DType::U32,
        "i4" => DType::I32,
        _ => DType::F32,
    }
}

/// (offset, type, name) of each field of a record.
fn fields(s: &str) -> Vec<(u64, DType, &str)> {
    let mut o = 0;
    s.split(' ').collect::<Vec<_>>().chunks(2).map(|f| {
        let d = dtype(f[0]);
        o += d.size() as u64;
        (o - d.size() as u64, d, f[1])
    }).collect()
}

/// The .HDR and .DBL of the SMOS product of `path` (one of them), if its header names a SMOS data block.
pub fn files(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let ext = path.extension()?.to_str()?.to_ascii_uppercase();
    if ext != "HDR" && ext != "DBL" {
        return None;
    }
    let (h, d) = (path.with_extension("HDR"), path.with_extension("DBL"));
    let head = std::fs::read(&h).ok()?;
    (head.len() < 1 << 24 && String::from_utf8_lossy(&head).contains("<Datablock_Schema>DBL_SM_") && d.exists()).then_some((h, d))
}

/// The text of the `<Tag>value</Tag>` lines of the fixed header and of the main product header.
fn header(text: &str) -> Attrs {
    let main = text.split("<Specific_Product_Header>").next().unwrap_or("");
    main.lines()
        .filter_map(|l| {
            let l = l.trim();
            let (tag, rest) = l.strip_prefix('<')?.split_once('>')?;
            let tag = tag.split(' ').next()?;
            let v = rest.strip_suffix(&format!("</{tag}>"))?.trim();
            (!v.is_empty()).then(|| (tag.to_string(), v.to_string()))
        })
        .take(200)
        .collect()
}

fn var(name: &str, a: Array) -> Variable {
    let fill = (a.dtype == DType::F32).then_some(-999.0);
    Variable { name: name.into(), group: String::new(), levels: vec![a], bands: vec![name.into()], fill, scale: 1.0, offset: 0.0, units: String::new(), georef: Georef::None, times: Default::default(), dims: vec!["grid_point".into()] }
}

pub fn open(hdr: &Path, dbl: &Path, rt: &tokio::runtime::Handle) -> Result<Dataset> {
    let text = std::fs::read_to_string(hdr).map_err(|e| Error(format!("{}: {e}", hdr.display())))?;
    let kind = text.split("<File_Type>").nth(1).and_then(|s| s.split('<').next()).unwrap_or("").trim().to_string();
    let src = Source::open(&dbl.to_string_lossy(), rt)?;
    let len = src.len()?;
    let u32_at = |o: u64| -> Result<u64> { Ok(u32::from_le_bytes(src.read(o..o + 4)?[..4].try_into().unwrap()) as u64) };
    let name = dbl.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let arr = |n: u64, chunk: u64, d: DType, codecs: Vec<Codec>, chunks: Vec<ChunkLoc>| Array { dims: vec!["grid_point".into()], shape: vec![n], chunk: vec![chunk], dtype: d, le: true, codecs, chunks: Chunks::List(chunks), place: None };
    let mut small = vec![];
    let l2 = match kind.as_str() {
        "MIR_SMUDP2" => Some(SMUDP2),
        "MIR_OSUDP2" => Some(OSUDP2),
        _ => None,
    };
    if let Some(l) = l2 {
        let f = fields(l);
        let stride = f.last().map_or(0, |x| x.0 + x.1.size() as u64);
        let n = u32_at(0)?;
        if 4 + n * stride > len {
            return Err(format!("{name}: {n} grid points of {stride} bytes are not in the file of {len} bytes (an other version of {kind}?)").into());
        }
        for (off, d, fname) in f {
            let chunks = (0..n.div_ceil(K)).map(|c| ChunkLoc { src: 0, off: 4 + c * K * stride, len: K.min(n - c * K) * stride }).collect();
            small.push(var(fname, arr(n, K, d, vec![Codec::Field { off: off as u32, stride: stride as u32 }], chunks)));
        }
    } else if let Some((counter, bt)) = match kind.as_str() {
        "MIR_BWLF1C" | "MIR_BWSF1C" => Some((1, 14)),
        "MIR_SCLF1C" | "MIR_SCSF1C" => Some((2, 28)),
        _ => None,
    } {
        let mut o = if counter == 2 { 4 + u32_at(0)?.checked_mul(166).ok_or("too many snapshots")? } else { 0 };
        let n = u32_at(o)?;
        o += 4;
        // ponytail: one read for each grid point to find the next one (a full orbit has about 200 000).
        let mut at = Vec::with_capacity(n.min(1 << 24) as usize);
        for _ in 0..n {
            if o + 17 + counter > len {
                return Err(format!("{name}: the grid points go past the end of the file").into());
            }
            let c = src.read(o + 17..o + 17 + counter)?;
            let m = if counter == 1 { c[0] as u64 } else { u16::from_le_bytes([c[0], c[1]]) as u64 };
            at.push(o);
            o += 17 + counter + m * bt;
        }
        let mut f = fields(L1C);
        f.push((17, if counter == 1 { DType::U8 } else { DType::U16 }, "BT_Data_Counter"));
        for (off, d, fname) in f {
            let chunks = at.iter().map(|&r| ChunkLoc { src: 0, off: r + off, len: d.size() as u64 }).collect();
            small.push(var(fname, arr(n, 1, d, vec![], chunks)));
        }
    } else {
        return Err(format!("{name}: SMOS product {kind}: this reader shows MIR_SMUDP2, MIR_OSUDP2 and the L1C products (BWLF1C, BWSF1C, SCLF1C, SCSF1C) only").into());
    }
    // The main variable first: the view shows it.
    let main = match kind.as_str() {
        "MIR_SMUDP2" => "Soil_Moisture",
        "MIR_OSUDP2" => "SSS1",
        _ => "BT_Data_Counter",
    };
    small.sort_by_key(|v| v.name != main);
    let info = Info { attrs: header(&text), dims: vec![("grid_point".into(), small[0].levels[0].shape[0])], vars: vec![] };
    let desc = format!("SMOS {kind}, {} grid points", small[0].levels[0].shape[0]);
    let mut ds = Dataset { product: Product { name, desc, vars: vec![], small, valid: None, info }, sources: vec![Arc::new(src)] };
    crate::points::add_images(&mut ds, rt)?;
    Ok(ds)
}

#[cfg(test)]
mod tests {
    fn write(dir: &std::path::Path, kind: &str, dbl: &[u8]) -> std::path::PathBuf {
        let h = dir.join(format!("SM_TEST_{kind}.HDR"));
        std::fs::write(&h, format!("<Earth_Explorer_Header>\n<Fixed_Header>\n<File_Type>{kind}</File_Type>\n</Fixed_Header>\n<Datablock_Schema>DBL_SM_XXXX_{kind}_0300.binXschema.xml</Datablock_Schema>\n")).unwrap();
        std::fs::write(h.with_extension("DBL"), dbl).unwrap();
        h
    }

    /// L2: 2 grid points of 221 bytes (soil moisture 0.25 and the fill -999). L1C browse: 2 grid points with
    /// 1 and 0 brightness temperatures.
    #[test]
    fn l2_and_l1c() {
        let dir = std::env::temp_dir().join(format!("eoview-smos-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut d = 2u32.to_le_bytes().to_vec();
        for (lat, sm) in [(10f32, 0.25f32), (10.1, -999.0)] {
            let mut r = vec![0u8; 221];
            r[4..8].copy_from_slice(&lat.to_le_bytes());
            r[28..32].copy_from_slice(&sm.to_le_bytes());
            d.extend(r);
        }
        let ds = crate::open(write(&dir, "MIR_SMUDP2", &d).to_str().unwrap(), rt.handle()).unwrap();
        let sm = ds.product.small.iter().find(|v| v.name == "Soil_Moisture").unwrap();
        let v = crate::read_all(&ds, sm).unwrap();
        assert!(v[0] == 0.25 && v[1].is_nan());
        assert_eq!(ds.product.vars[0].name, "Soil_Moisture");
        let mut d = 2u32.to_le_bytes().to_vec();
        for (lat, n) in [(-5f32, 1u8), (-5.1, 0)] {
            let mut r = vec![0u8; 17];
            r[4..8].copy_from_slice(&lat.to_le_bytes());
            d.extend(r);
            d.push(n);
            d.extend(vec![0u8; 14 * n as usize]);
        }
        let ds = crate::open(write(&dir, "MIR_BWLF1C", &d).to_str().unwrap(), rt.handle()).unwrap();
        let lat = ds.product.small.iter().find(|v| v.name == "Grid_Point_Latitude").unwrap();
        assert_eq!(crate::read_all(&ds, lat).unwrap(), vec![-5.0, -5.099999904632568]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
