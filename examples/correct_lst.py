"""Script of the example project features.eoview (the Python panel of eoview, F7).

The mean of 30 days of land surface temperature still has some gaps where clouds were there each day. The
script fills them with the mean of the valid pixels around them, converts kelvin to degrees Celsius, and
makes a chart: the mean temperature of each latitude. Then it reads the values of the shapes of the views.
"""
import eoview as ev
import matplotlib.pyplot as plt
import numpy as np

# The aggregate layer of the left view, at the full resolution.
name = next(l["name"] for l in ev.layers() if "mean" in l["name"])
mean = ev.layer(name, extent="all", level=0)
a = np.asarray(mean, dtype=np.float64)
print(name, a.shape, "gaps:", int(np.isnan(a).sum()))


def box(x, k):
    """Sum of each (2k+1) x (2k+1) window."""
    c = np.pad(np.pad(x, k).cumsum(0).cumsum(1), ((1, 0), (1, 0)))
    n = 2 * k + 1
    return c[n:, n:] - c[:-n, n:] - c[n:, :-n] + c[:-n, :-n]


filled = a.copy()
for _ in range(3):
    gaps = np.isnan(filled)
    valid = (~gaps).astype(np.float64)
    near = box(np.where(gaps, 0.0, filled), 3) / np.maximum(box(valid, 3), 1)
    # Fill only the gaps with enough valid pixels around them: the sea stays empty.
    filled[gaps & (box(valid, 3) >= 12)] = near[gaps & (box(valid, 3) >= 12)]
print("gaps after the fill:", int(np.isnan(filled).sum()))

ev.output(filled - 273.15, name="lst mean filled", units="°C")

# A chart that eoview does not have: the mean temperature of each latitude, with its spread.
lat = mean["y"].values if hasattr(mean, "coords") and "y" in mean.coords else np.arange(a.shape[0])
c = filled - 273.15
fig, ax = plt.subplots(figsize=(6, 3.2))
ax.fill_between(lat, np.nanpercentile(c, 10, axis=1), np.nanpercentile(c, 90, axis=1), alpha=0.3, label="10 to 90 %")
ax.plot(lat, np.nanmean(c, axis=1), label="mean")
ax.set_xlabel("Latitude (degrees)")
ax.set_ylabel("LST, July 2023 (°C)")
ax.legend()
ax.grid(alpha=0.3)
ev.plot(fig, name="Mean temperature by latitude")

# The shapes of the views: the pixels of the region "France", and the values along the transect.
fr = ev.layer(name, region="France")
print("France: %d pixels, mean %.2f °C" % (np.isfinite(fr).sum(), np.nanmean(fr) - 273.15))
line = ev.layer(name, region="Lisbon to Stockholm")
print("Lisbon to Stockholm: %d points on %.0f km" % (line.size, line["distance"][-1] / 1000))
