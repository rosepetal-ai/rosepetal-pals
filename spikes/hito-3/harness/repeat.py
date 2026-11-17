#!/usr/bin/env python3
"""The verdict of the repeated backend measurements of hito 3 (H3-D1).

    repeat.py --runs <results.json>... --production <kad-iroh.json> <kad-libp2p.json>

Task 7 of phase 2 of the hito 3 plan repeats the measurements of phase 1,
which are the conditions of the ratification of H3-D1
(docs/decisions/2026-11-16-hito-3-backend-dht.md, "Condiciones"):

- `--runs`: whole runs of harness.sh, of kad-iroh and of kad-libp2p, phase
  1's and the repeated ones, all with the same `params`;
- `--production`: one S6 run of each with the production intervals
  (params-production.env, `--only S6`), with kad-iroh dialing each peer
  once. They are told apart by their `backend`, not by their order.

It prints, in Markdown and in Spanish, the tables of repeticion.md: each
run's commit, date and status of S0 to S7; the minimum, median and maximum
across runs of S1 `first_valid_ms_p50`, S3 `routers_forget_s`, S7
`resolved_s_max` and S6 `router_bytes_per_min_p50`; the production S6 by
role; and the verdict, condition by condition, as the ratification words
them:

1. kad-iroh is in `fail` in S3 or in S7 in any of its runs, and kad-libp2p
   in none. Read as written: kad-libp2p in `fail` in neither S3 nor S7 in
   any of its runs. When reading it scenario by scenario would give another
   answer, the output says so;
2. with production intervals, a kad-iroh router's idle traffic (S6
   `router_bytes_per_min_p50`) is more than double kad-libp2p's: strictly
   more.

Exits 0 when no switching condition holds, 3 when one does, and 1 when a
JSON is invalid (check_results.py's schema, a host path, another backend,
other params, a run without S3 or S7, production runs without the
production intervals or without S6 measured) or has a scenario in
`error`: that run is repeated under the same tag, and does not count.
"""

import argparse
import json
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_results import HOST_PATHS, SCENARIOS, schema_problems  # noqa: E402

IROH, LIBP2P = "kad-iroh", "kad-libp2p"
BACKENDS = (IROH, LIBP2P)
# The production intervals of the hito 3 plan ("Valores que el spec no
# fija"); the idle period must cover two republications.
PRODUCTION = {"TTL_S": 3600, "REPUBLISH_S": 1200}
PRODUCTION_KEYS = ("TTL_S", "REPUBLISH_S", "IDLE_S")
# The scenarios of condition 1, and the figures whose spread is given.
JUDGED = ("S3", "S7")
SPREAD = (("S1", "first_valid_ms_p50"), ("S3", "routers_forget_s"), ("S7", "resolved_s_max"),
          ("S6", "router_bytes_per_min_p50"))
IDLE = "router_bytes_per_min_p50"
ROLES = ("router", "provider", "client")


def fmt(value):
    if value is None:
        return "—"
    if isinstance(value, bool):
        return "sí" if value else "no"
    if isinstance(value, float):
        return str(int(value)) if value.is_integer() else f"{round(value, 1)}"
    return str(value)


def table(header, rows):
    lines = ["| " + " | ".join(header) + " |", "|" + "-|" * len(header)]
    return "\n".join(lines + ["| " + " | ".join(row) + " |" for row in rows])


class Run:
    """A results JSON: its name (the file's stem, never its path) and
    document."""

    def __init__(self, path):
        self.name = Path(path).stem
        self.doc = None
        self.problems = []
        try:
            text = Path(path).read_text(encoding="utf-8")
            self.doc = json.loads(text)
        except (OSError, json.JSONDecodeError) as error:
            self.problems.append(f"no se puede leer: {error}")
            return
        self.problems += schema_problems(self.doc)
        if HOST_PATHS.search(text):
            self.problems.append("nombra una ruta del anfitrión; los resultados llevan rutas relativas")
        if self.problems:
            return
        if self.backend not in BACKENDS:
            self.problems.append(f"backend {self.backend}, no uno de {', '.join(BACKENDS)}")
        for s in SCENARIOS:
            if self.status(s) == "error":
                self.problems.append(f"{s} en error: esa ejecución se repite con la misma etiqueta, y esta no cuenta")

    @property
    def backend(self):
        return self.doc["backend"]

    @property
    def params(self):
        return self.doc["params"]

    def status(self, scenario):
        return self.doc["scenarios"][scenario]["status"]

    def metric(self, scenario, key):
        return self.doc["scenarios"][scenario]["metrics"].get(key)


def differ(a, b, skip=()):
    return sorted(k for k in set(a) | set(b) if k not in skip and a.get(k) != b.get(k))


def check(runs, production):
    """The problems that keep the runs from being judged."""
    problems = [f"{r.name}: {p}" for r in runs + production for p in r.problems]
    if problems:
        return problems
    reference = runs[0]
    for r in runs:
        shown = ", ".join(f"{k}: {reference.params.get(k)!r} frente a {r.params.get(k)!r}"
                          for k in differ(reference.params, r.params))
        if shown:
            problems.append(f"{r.name}: params distintos de los de {reference.name}: {shown}")
        for s in JUDGED:
            if r.status(s) not in ("pass", "fail"):
                problems.append(f"{r.name}: {s} en {r.status(s)}: una ejecución de --runs tiene que medir S3 y S7")
    for b in BACKENDS:
        if not any(r.backend == b for r in runs):
            problems.append(f"--runs no tiene ninguna ejecución de {b}")
    if sorted(r.backend for r in production) != sorted(BACKENDS):
        problems.append(f"--production tiene que ser una ejecución de {IROH} y una de {LIBP2P}")
        return problems
    for r in production:
        for key, value in PRODUCTION.items():
            if r.params.get(key) != value:
                problems.append(f"{r.name}: {key} es {r.params.get(key)!r}, no {value}, el de producción")
        idle, republish = r.params.get("IDLE_S"), r.params.get("REPUBLISH_S")
        if not isinstance(idle, int) or not isinstance(republish, int) or idle < 2 * republish:
            problems.append(f"{r.name}: IDLE_S es {idle!r}: el reposo tiene que cubrir dos vueltas de republicación")
        shown = ", ".join(differ(reference.params, r.params, PRODUCTION_KEYS))
        if shown:
            problems.append(f"{r.name}: params distintos de los de --runs fuera de {', '.join(PRODUCTION_KEYS)}: {shown}")
        if r.status("S6") != "measured":
            problems.append(f"{r.name}: S6 en {r.status('S6')}, no measured")
        elif not isinstance(r.metric("S6", IDLE), (int, float)):
            problems.append(f"{r.name}: S6 sin {IDLE}")
    return problems


def runs_table(runs):
    rows = [[f"`{r.name}`", f"`{r.backend}`", f"`{r.doc['commit']}`", r.doc["date"]]
            + [f"`{r.status(s)}`" for s in SCENARIOS] for r in runs]
    return table(["Ejecución", "Backend", "Commit", "Fecha (UTC)"] + SCENARIOS, rows)


def spread_table(runs):
    rows = []
    for scenario, key in SPREAD:
        for b in BACKENDS:
            mine = [r for r in runs if r.backend == b]
            values = [v for v in (r.metric(scenario, key) for r in mine) if v is not None]
            if values:
                low, mid, high = min(values), statistics.median(values), max(values)
            else:
                low = mid = high = None
            rows.append([f"{scenario} `{key}`", f"`{b}`", f"{len(values)} de {len(mine)}",
                         fmt(low), fmt(mid), fmt(high)])
    return table(["Cifra", "Backend", "Con cifra", "Mínimo", "Mediana", "Máximo"], rows)


def production_table(production):
    by = {r.backend: r for r in production}
    keys = []
    for b in BACKENDS:
        for key in by[b].doc["scenarios"]["S6"]["metrics"]:
            if key not in keys:
                keys.append(key)
    rows = []
    for role in ROLES:
        for key in (k for k in keys if k.startswith(role + "_")):
            rows.append([f"`{key}`"] + [fmt(by[b].metric("S6", key)) for b in BACKENDS])
    header = ["S6, papel y métrica"] + [f"`{b}` (`{by[b].name}`)" for b in BACKENDS]
    params = ", ".join(f"{k} = {by[IROH].params[k]}" for k in PRODUCTION_KEYS)
    origin = "; ".join(f"`{by[b].name}`: commit `{by[b].doc['commit']}`, {by[b].doc['date']}" for b in BACKENDS)
    return f"{params}. {origin}.\n\n" + table(header, rows)


def verdict(runs, production):
    """The lines of the verdict, and whether a switching condition holds."""
    lines = []
    fails = {b: {s: [r.name for r in runs if r.backend == b and r.status(s) == "fail"] for s in JUDGED}
             for b in BACKENDS}
    count = {b: sum(1 for r in runs if r.backend == b) for b in BACKENDS}
    iroh_failed = [s for s in JUDGED if fails[IROH][s]]
    libp2p_failed = [s for s in JUDGED if fails[LIBP2P][s]]
    first = bool(iroh_failed) and not libp2p_failed

    def told(b):
        return "; ".join(f"en {s}, {len(fails[b][s])} de {count[b]}"
                         + (f" ({', '.join(f'`{n}`' for n in fails[b][s])})" if fails[b][s] else "")
                         for s in JUDGED)

    lines.append(f"1. **S3 y S7.** Ejecuciones en `fail`: `{IROH}`, {told(IROH)}; `{LIBP2P}`, {told(LIBP2P)}.")
    if first:
        lines.append(f"   **Se da:** `{IROH}` queda en `fail` en {' y '.join(iroh_failed)} y `{LIBP2P}` en ninguna.")
    elif iroh_failed:
        lines.append(f"   **No se da:** `{IROH}` queda en `fail` en {' y '.join(iroh_failed)}, pero `{LIBP2P}` "
                     f"también en {' y '.join(libp2p_failed)}.")
    else:
        lines.append(f"   **No se da:** `{IROH}` no queda en `fail` en S3 ni en S7 en ninguna de sus ejecuciones.")
    per_scenario = [s for s in JUDGED if fails[IROH][s] and not fails[LIBP2P][s]]
    if bool(per_scenario) != first:
        lines.append(f"   Leída escenario por escenario, la condición {'sí' if per_scenario else 'no'} se daría "
                     f"({', '.join(per_scenario) or 'en ninguno'}): las dos lecturas no coinciden aquí.")

    by = {r.backend: r for r in production}
    iroh, libp2p = by[IROH].metric("S6", IDLE), by[LIBP2P].metric("S6", IDLE)
    second = iroh > 2 * libp2p
    ratio = f"{iroh / libp2p:.2f}".replace(".", ",") if libp2p else "—"
    lines.append(f"2. **Reposo con intervalos de producción.** `{IDLE}`: `{IROH}`, {fmt(iroh)} B/min; `{LIBP2P}`, "
                 f"{fmt(libp2p)} B/min. Es {ratio} veces el de `{LIBP2P}`, y el doble son {fmt(2 * libp2p)} B/min.")
    lines.append(f"   **{'Se da' if second else 'No se da'}:** el de `{IROH}` "
                 f"{'pasa' if second else 'no pasa'} del doble del de `{LIBP2P}`.")

    held = [n for n, h in (("1 (S3 y S7)", first), ("2 (reposo con intervalos de producción)", second)) if h]
    if held:
        lines.append(f"\n**Veredicto: se da una condición de cambio a `{LIBP2P}`**, la {' y la '.join(held)}.")
    else:
        lines.append(f"\n**Veredicto: sigue `{IROH}`.** No se da ninguna condición de cambio.")
    return lines, bool(held)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--runs", nargs="+", required=True, help="whole runs of kad-iroh and kad-libp2p")
    parser.add_argument("--production", nargs=2, required=True, metavar=("KAD_IROH", "KAD_LIBP2P"),
                        help="the two S6 runs with the production intervals")
    args = parser.parse_args(argv)
    runs = [Run(p) for p in args.runs]
    production = [Run(p) for p in args.production]
    problems = check(runs, production)
    if problems:
        for problem in problems:
            print(problem)
        print("No se juzga ninguna condición: hay ejecuciones que no valen.")
        return 1
    lines, switch = verdict(runs, production)
    print("## Ejecuciones\n")
    print(runs_table(runs))
    print("\n## Variación entre ejecuciones\n")
    print(spread_table(runs))
    print("\n## S6 con intervalos de producción\n")
    print(production_table(production))
    print("\n## Veredicto\n")
    print("\n".join(lines))
    return 3 if switch else 0


if __name__ == "__main__":
    sys.exit(main())
