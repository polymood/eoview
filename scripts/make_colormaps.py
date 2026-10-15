"""Write crates/eoview/assets/colormaps.json: the built-in color maps of eoview, from the reference packages.

Run: UV_CACHE_DIR=target/scratch/uvcache uv run --no-project --with matplotlib \
       --with cmocean --with cmcrameri python scripts/make_colormaps.py

Each map has 32 stops (linear between them), and the discrete maps (tab10, Set1, ...) 256 samples: steps.
Sources and licenses: matplotlib (viridis, magma, inferno, plasma, cividis: CC0; the other maps: the
matplotlib license), ColorBrewer (Cynthia Brewer, Apache 2.0, through matplotlib), cmocean (Thyng et al.,
MIT), Scientific colour maps (Fabio Crameri, MIT, through cmcrameri).
"""
import json
import os

import cmcrameri.cm as cmc
import cmocean
import matplotlib
import numpy as np

# The first 12 keep the order of the color maps of eoview before this file (the index of the wind color map).
GROUPS = [
    ("Perceptual", "matplotlib", ["gray", "viridis", "magma", "inferno", "plasma", "cividis", "turbo"]),
    ("Classic", "matplotlib", ["jet", "hot", "terrain"]),
    ("Diverging", "ColorBrewer", ["RdBu", "RdYlGn"]),
    ("Sequential", "ColorBrewer", ["Greys", "Blues", "Greens", "Reds", "Oranges", "Purples", "YlOrRd", "YlOrBr", "YlGnBu", "YlGn", "BuPu", "GnBu", "OrRd", "PuBu", "PuBuGn", "PuRd", "RdPu", "BuGn"]),
    ("Sequential", "matplotlib", ["bone", "pink", "copper", "afmhot", "gist_heat", "cubehelix", "gist_earth", "ocean"]),
    ("Diverging", "ColorBrewer", ["RdYlBu", "Spectral", "BrBG", "PiYG", "PRGn", "PuOr", "RdGy"]),
    ("Diverging", "matplotlib", ["coolwarm", "bwr", "seismic"]),
    ("Cyclic", "matplotlib", ["twilight", "twilight_shifted", "hsv"]),
    ("Classic", "matplotlib", ["rainbow", "nipy_spectral", "gist_ncar", "gist_rainbow", "brg", "CMRmap"]),
    ("Qualitative", "matplotlib", ["tab10", "tab20", "Set1", "Set2", "Set3", "Paired", "Dark2", "Accent", "Pastel1"]),
    ("Oceanography", "cmocean", ["thermal", "haline", "solar", "ice", "oxy", "deep", "dense", "algae", "matter", "turbid", "speed", "amp", "tempo", "rain", "topo", "balance", "delta", "curl", "diff", "tarn", "phase"]),
    ("Scientific", "Crameri", ["batlow", "batlowK", "batlowW", "lajolla", "hawaii", "oslo", "tokyo", "bamako", "bilbao", "buda", "davos", "devon", "imola", "lapaz", "nuuk", "turku", "acton", "glasgow", "navia", "lipari"]),
    ("Diverging", "Crameri", ["roma", "vik", "broc", "cork", "berlin", "lisbon", "tofino", "bam", "vanimo", "managua"]),
    ("Cyclic", "Crameri", ["romaO", "vikO", "brocO", "corkO", "bamO"]),
    ("Multi-sequential", "Crameri", ["oleron", "bukavu", "fes"]),
]
# Names of the maps of eoview before this file: a workspace keeps the name.
RENAME = {"gray": "Gray", "viridis": "Viridis", "magma": "Magma", "inferno": "Inferno", "plasma": "Plasma", "cividis": "Cividis", "turbo": "Turbo", "jet": "Jet", "hot": "Hot", "terrain": "Terrain"}


def get(source, name):
    if source in ("matplotlib", "ColorBrewer"):
        return matplotlib.colormaps[name]
    if source == "cmocean":
        return cmocean.cm.cmap_d[name]
    return getattr(cmc, name)


def hexes(rgb):
    return ",".join("%02x%02x%02x" % tuple(int(round(v * 255)) for v in c[:3]) for c in rgb)


out, seen = [], set()
for group, source, names in GROUPS:
    for n in names:
        try:
            cm = get(source, n)
        except (KeyError, AttributeError) as e:
            print("missing", source, n, e)
            continue
        name = RENAME.get(n, n if source in ("ColorBrewer",) or n[0].isupper() else n[0].upper() + n[1:])
        if name in seen:
            continue
        seen.add(name)
        discrete = group == "Qualitative"
        x = (np.arange(256) + 0.5) / 256 if discrete else np.linspace(0, 1, 32)
        out.append({"name": name, "group": group, "source": source, "stops": hexes(cm(x))})
path = os.path.join(os.path.dirname(__file__), "..", "crates", "eoview", "assets", "colormaps.json")
with open(path, "w") as f:
    f.write("[\n" + ",\n".join(json.dumps(m) for m in out) + "\n]\n")
print(len(out), "color maps,", os.path.getsize(path), "bytes")
