#!/usr/bin/env python3
"""Writes a list of product URLs from a STAC search: one URL on each line, in time order.

    scripts/stac_list.py --catalog earth-search --collection sentinel-2-l2a \\
        --bbox -5 42 8.5 51.2 --date 2025-07-01/2025-08-31 --cloud 5 --best-per-tile > list.txt
    eoview $(cat list.txt)             # each product in its own view
    eoview --series $(cat list.txt)    # the products are the time steps of one layer

Catalogs without an account:
  earth-search  Sentinel-2 L2A as COG files (asset `visual`: true color; `red`, `nir`, `scl`, and so on)
  eopf          EOPF Sentinel Zarr samples: Sentinel-1, -2 and -3 (asset `product`: the Zarr store)

Only the standard library of Python.
"""
import argparse
import json
import sys
import urllib.request

CATALOGS = {
    "earth-search": ("https://earth-search.aws.element84.com/v1", "visual"),
    "eopf": ("https://stac.core.eopf.eodc.eu", "product"),
}


def fetch(url: str, body: dict | None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data, {"Content-Type": "application/json", "Accept": "application/geo+json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def search(root: str, body: dict, limit: int):
    """The items of the search, page after page."""
    url, n = root.rstrip("/") + "/search", 0
    while url:
        page = fetch(url, body)
        for item in page.get("features", []):
            yield item
            n += 1
            if n >= limit:
                return
        link = next((l for l in page.get("links", []) if l.get("rel") == "next"), None)
        if not link or not page.get("features"):
            return
        # The next page is a POST with a new body, or a GET of a full URL.
        url = link["href"]
        body = {**body, **link["body"]} if link.get("merge") else link.get("body") if link.get("method", "GET").upper() == "POST" else None


def tile(item: dict) -> str:
    p = item["properties"]
    return p.get("grid:code") or "{}{}{}".format(p.get("mgrs:utm_zone", ""), p.get("mgrs:latitude_band", ""), p.get("mgrs:grid_square", "")) or item["id"]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--catalog", default="earth-search", help="earth-search, eopf, or the URL of a STAC API")
    ap.add_argument("--collection", required=True)
    ap.add_argument("--bbox", nargs=4, type=float, required=True, metavar=("WEST", "SOUTH", "EAST", "NORTH"))
    ap.add_argument("--date", required=True, help="START/END, for example 2025-07-01/2025-08-31")
    ap.add_argument("--asset", help="asset of each item (default: visual for earth-search, product for eopf)")
    ap.add_argument("--cloud", type=float, help="maximum cloud cover in percent (items without this property stay)")
    ap.add_argument("--best-per-tile", action="store_true", help="keep one item for each MGRS tile: the item with the least cloud")
    ap.add_argument("--limit", type=int, default=20000, help="maximum number of items to read (default 20000)")
    a = ap.parse_args()

    root, asset = CATALOGS.get(a.catalog, (a.catalog, "product"))
    asset = a.asset or asset
    start, end = a.date.split("/")
    body = {"collections": [a.collection], "bbox": a.bbox, "datetime": f"{start}T00:00:00Z/{end}T23:59:59Z", "limit": 200}
    items, read = [], 0
    for item in search(root, body, a.limit):
        read += 1
        cloud = item["properties"].get("eo:cloud_cover")
        if a.cloud is not None and cloud is not None and cloud > a.cloud:
            continue
        if asset not in item["assets"]:
            sys.exit(f"{item['id']}: no asset '{asset}'. Assets: {', '.join(item['assets'])}")
        items.append(item)
    if a.best_per_tile:
        best: dict[str, dict] = {}
        for item in items:
            t, c = tile(item), item["properties"].get("eo:cloud_cover", 0.0)
            if t not in best or c < best[t]["properties"].get("eo:cloud_cover", 0.0):
                best[t] = item
        items = list(best.values())
    items.sort(key=lambda i: i["properties"].get("datetime") or "")
    for item in items:
        print(item["assets"][asset]["href"])
    print(f"{read} items read, {len(items)} URLs written", file=sys.stderr)


if __name__ == "__main__":
    main()
