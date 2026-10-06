#!/usr/bin/env python3
"""Writes a list of the daily files of the ESA CCI land surface temperature at 0.01 degrees: one URL for each day.

    scripts/cci_lst_list.py 2023-01-01 2023-12-31 > lst_2023.txt
    scripts/cci_lst_list.py 2021-07-01 2021-07-31 --sensor modis-terra --night > list.txt
    eoview --series lst_2023.txt        # the days are the time steps of one layer. The variable is `lst`

The files are on the CEDA archive, without an account: NetCDF-4, the full globe at 0.01 degrees
(36 000 x 18 000 pixels), about 0.7 GB for each day, in chunks of 1000 x 1000 pixels and without overviews.
A view of the full globe reads all the chunks of the variable. The script makes the names from the dates:
it does not examine if each file is there.

Sensors and years: slstr-a (Sentinel-3A, 2016 to 2023), slstr-b (Sentinel-3B), modis-terra, modis-aqua.
"""
import argparse
import datetime

BASE = "https://dap.ceda.ac.uk/neodc/esacci/land_surface_temperature/data"
SENSORS = {
    "slstr-a": ("SENTINEL3A_SLSTR", "SLSTRA"),
    "slstr-b": ("SENTINEL3B_SLSTR", "SLSTRB"),
    "modis-terra": ("TERRA_MODIS", "MODIST"),
    "modis-aqua": ("AQUA_MODIS", "MODISA"),
}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("first", help="first day, for example 2023-01-01")
    ap.add_argument("last", help="last day")
    ap.add_argument("--sensor", default="slstr-a", choices=sorted(SENSORS))
    ap.add_argument("--night", action="store_true", help="the night passes (default: the day passes)")
    ap.add_argument("--version", default="4.00")
    a = ap.parse_args()
    directory, name = SENSORS[a.sensor]
    day, last = datetime.date.fromisoformat(a.first), datetime.date.fromisoformat(a.last)
    while day <= last:
        part = "NIGHT" if a.night else "DAY"
        print(f"{BASE}/{directory}/L3C/0.01/v{a.version}/daily/{day:%Y/%m/%d}/ESACCI-LST-L3C-LST-{name}-0.01deg_1DAILY_{part}-{day:%Y%m%d}000000-fv{a.version}.nc")
        day += datetime.timedelta(days=1)


if __name__ == "__main__":
    main()
