#!/usr/bin/env python3
"""The Markdown table of the hito 3 backend comparison, for its decision record.

    summarize.py --compat <compat.json> <results.json>...

One column per candidate, in the order of the files, and one row per
metric. The compatibility rows (MSRV, crates, duplicates, build time,
binary size) come from compat.json, which the results do not copy because
it is measured again apart (compat.sh); while compat.json is provisional,
its size cells say `provisional`. Then, for each scenario, a row with its
status and one per metric, each cell the figure with its scenario's status,
`—` where there is none. Check the results with check_results.py first:
this only prints them.
"""

import argparse
import json
import sys

SCENARIOS = [f"S{i}" for i in range(8)]


def fmt(value):
    if value is None:
        return "—"
    if isinstance(value, bool):
        return "sí" if value else "no"
    if isinstance(value, float):
        return f"{value:g}"
    return str(value).replace("|", "\\|")


def compat_rows(compat, backends):
    candidates = (compat or {}).get("candidates") or {}
    provisional = bool((compat or {}).get("provisional"))

    def cell(backend, key, size=False):
        row = candidates.get(backend)
        if not isinstance(row, dict):
            return "—"
        if size and provisional:
            return "provisional"
        value = row.get(key)
        if key == "duplicates":
            return fmt(len(value)) if isinstance(value, list) else "—"
        return fmt(value)

    rows = [
        ("MSRV 1.91 (`cargo +1.91 check`)", "msrv_1_91", False),
        ("Crates", "crates", False),
        ("Crates en más de una versión", "duplicates", False),
        ("Compilación release desde cero (s)", "release_build_s", False),
        ("Binario (bytes)", "binary_bytes", True),
        ("Binario sobre `baseline` (bytes)", "binary_bytes_over_baseline", True),
    ]
    return [[label] + [cell(b, key, size) for b in backends] for label, key, size in rows]


def scenario_rows(results, backends):
    rows = []
    for name in SCENARIOS:
        per = {b: (results[b].get("scenarios") or {}).get(name) or {} for b in backends}
        rows.append([f"**{name}** estado"] + [fmt(per[b].get("status")) for b in backends])
        keys = []
        for b in backends:
            for key in per[b].get("metrics") or {}:
                if key not in keys:
                    keys.append(key)
        for key in keys:
            cells = []
            for b in backends:
                metrics = per[b].get("metrics") or {}
                status = per[b].get("status") or "—"
                cells.append(f"{fmt(metrics.get(key))} ({status})")
            rows.append([f"{name} `{key}`"] + cells)
    return rows


def table(header, rows):
    lines = ["| " + " | ".join(header) + " |", "|" + "-|" * len(header)]
    lines += ["| " + " | ".join(row) + " |" for row in rows]
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--compat", required=True, help="docs/decisions/hito-3-backend/compat.json")
    parser.add_argument("results", nargs="+", help="results/<id>.json")
    args = parser.parse_args()
    with open(args.compat, encoding="utf-8") as f:
        compat = json.load(f)
    results = {}
    for path in args.results:
        with open(path, encoding="utf-8") as f:
            doc = json.load(f)
        results[doc.get("backend") or path] = doc
    backends = list(results)
    header = ["Métrica"] + [f"`{b}`" for b in backends]
    print(table(header, compat_rows(compat, backends) + scenario_rows(results, backends)))
    if compat.get("provisional"):
        print(f"\nCompatibilidad provisional: {compat['provisional']}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
