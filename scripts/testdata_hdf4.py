"""Write small HDF4 files to testdata/ for the HDF4 reader (crates/eo-cache/tests/hdf4.rs).

hdf4_sds.hdf: scientific data sets (SDS). The values are the flat index in C order, thus a test can compute
the expected value at each index.
- temperature: 2D float32 (6, 8), named dimensions, attributes; Latitude, Longitude: 2D float32 (6, 8)
- counts: 3D int16 (3, 6, 8), CF scale_factor, add_offset, _FillValue (value 0 is fill)
- calibrated: 2D int16 (6, 8), SDsetcal (HDF4 convention: value = scale * (stored - offset))
- deflated: 2D int32 (6, 8), deflate compression
- tiles: 2D uint16 (10, 12), chunks of 4 x 5, deflate in each chunk; plain: the same without compression
- growing: 2D int32 (4, 8), unlimited first dimension, written in two parts (linked blocks)
- huffman: 2D int16 (6, 8), skipping Huffman compression (not supported by the reader)
- lat (10), lon (12): 1D float32, the geographic grid of tiles
hdf4_eos_geo.hdf: an SDS with the dimensions of an HDF-EOS 2 grid with GCTP_GEO projection, and the
StructMetadata.0 attribute of the grid (pyhdf does not write HDF-EOS: the text is written by hand).

pyhdf has no function for chunks: the script calls SDsetchunk of the HDF4 library of pyhdf (ctypes).

Run: UV_CACHE_DIR=target/scratch/uvcache uv run --with numpy --with pyhdf python scripts/testdata_hdf4.py
"""
import ctypes
import glob
import os

import numpy as np
import pyhdf
from pyhdf.SD import SD, SDC

D = os.path.join(os.path.dirname(__file__), "..", "testdata")
LIB = glob.glob(os.path.join(os.path.dirname(pyhdf.__file__), "..", "pyhdf.libs", "libmfhdf*"))
MFHDF = ctypes.CDLL(LIB[0]) if LIB else ctypes.CDLL("libmfhdf.so")


class ChunkDef(ctypes.Structure):
    # HDF_CHUNK_DEF (comp member): chunk_lengths[32], comp_type, model_type, comp_info (union).
    _fields_ = [("lengths", ctypes.c_int32 * 32), ("comp_type", ctypes.c_int32), ("model_type", ctypes.c_int32),
                ("cinfo", ctypes.c_int32 * 16)]


def set_chunk(sds, lengths, deflate):
    c = ChunkDef()
    for i, n in enumerate(lengths):
        c.lengths[i] = n
    if deflate:
        c.comp_type, c.cinfo[0] = 4, 6  # COMP_CODE_DEFLATE, level 6
    # HDF_COMP = 3, HDF_CHUNK = 1
    assert MFHDF.SDsetchunk(ctypes.c_int32(sds._id), c, ctypes.c_int32(3 if deflate else 1)) == 0


def sds(f, name, typ, dims, shape, data, compress=None):
    v = f.create(name, typ, shape)
    for i, d in enumerate(dims):
        v.dim(i).setname(d)
    if compress:
        v.setcompress(*compress)
    if data is not None:
        v[:] = data
    return v


def flat(shape, dtype):
    return np.arange(np.prod(shape)).reshape(shape).astype(dtype)


f = SD(os.path.join(D, "hdf4_sds.hdf"), SDC.WRITE | SDC.CREATE | SDC.TRUNC)
f.title = "eoview HDF4 test"
f.attr("version").set(SDC.INT32, 3)

v = sds(f, "temperature", SDC.FLOAT32, ("row", "col"), (6, 8), flat((6, 8), np.float32))
v.units = "K"
v.long_name = "temperature at the surface"
v.attr("valid_range").set(SDC.FLOAT32, [0.0, 100.0])
y, x = np.mgrid[0:6, 0:8]
sds(f, "Latitude", SDC.FLOAT32, ("row", "col"), (6, 8), (45.0 - y).astype(np.float32)).units = "degrees_north"
sds(f, "Longitude", SDC.FLOAT32, ("row", "col"), (6, 8), (10.0 + x).astype(np.float32)).units = "degrees_east"

v = sds(f, "counts", SDC.INT16, ("level", "row", "col"), (3, 6, 8), flat((3, 6, 8), np.int16))
v.setfillvalue(0)
v.scale_factor = 0.5
v.add_offset = 10.0
v.units = "W m-2"

v = sds(f, "calibrated", SDC.INT16, ("row", "col"), (6, 8), flat((6, 8), np.int16))
v.setcal(2.0, 0.0, 5.0, 0.0, SDC.FLOAT32)

sds(f, "deflated", SDC.INT32, ("row", "col"), (6, 8), flat((6, 8), np.int32), (SDC.COMP_DEFLATE, 6))
sds(f, "huffman", SDC.INT16, ("row", "col"), (6, 8), flat((6, 8), np.int16), (SDC.COMP_SKPHUFF, 2))

for name, deflate in [("tiles", True), ("plain", False)]:
    v = sds(f, name, SDC.UINT16, ("lat", "lon"), (10, 12), None)
    set_chunk(v, (4, 5), deflate)
    v[:] = flat((10, 12), np.uint16)
    v.endaccess()

v = sds(f, "growing", SDC.INT32, ("step", "col"), (SDC.UNLIMITED, 8), None)
a = flat((4, 8), np.int32)
v[0:2] = a[0:2]
v.endaccess()
v = f.select("growing")
v[2:4] = a[2:4]
v.endaccess()

sds(f, "lat", SDC.FLOAT32, ("lat",), (10,), (50.0 - 0.5 * np.arange(10)).astype(np.float32)).units = "degrees_north"
sds(f, "lon", SDC.FLOAT32, ("lon",), (12,), (2.0 + 0.5 * np.arange(12)).astype(np.float32)).units = "degrees_east"
f.end()

# HDF-EOS 2 grid with the geographic projection: 1 degree pixels, upper-left corner at 10 E, 46 N.
# The corners are in the packed degrees format of GCTP (DDDMMMSSS.SS).
META = """GROUP=SwathStructure
END_GROUP=SwathStructure
GROUP=GridStructure
\tGROUP=GRID_1
\t\tGridName="Geo"
\t\tXDim=8
\t\tYDim=6
\t\tUpperLeftPointMtrs=(10000000.000000,46000000.000000)
\t\tLowerRightMtrs=(18000000.000000,40000000.000000)
\t\tProjection=GCTP_GEO
\t\tGROUP=Dimension
\t\tEND_GROUP=Dimension
\t\tGROUP=DataField
\t\t\tOBJECT=DataField_1
\t\t\t\tDataFieldName="ndvi"
\t\t\t\tDataType=DFNT_FLOAT32
\t\t\t\tDimList=("YDim","XDim")
\t\t\tEND_OBJECT=DataField_1
\t\tEND_GROUP=DataField
\tEND_GROUP=GRID_1
END_GROUP=GridStructure
END
"""
f = SD(os.path.join(D, "hdf4_eos_geo.hdf"), SDC.WRITE | SDC.CREATE | SDC.TRUNC)
setattr(f, "StructMetadata.0", META)
sds(f, "ndvi", SDC.FLOAT32, ("YDim:Geo", "XDim:Geo"), (6, 8), flat((6, 8), np.float32))
f.end()
