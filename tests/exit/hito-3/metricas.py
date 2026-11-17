#!/usr/bin/env python3
"""Measurements of the hito 3 exit test (tests/exit/hito-3.sh,
docs/runbooks/hito-3-descubrimiento-kademlia.md).

In an agent's container, where the harness copies this file to
/root/pals-h3/metricas.py, it reads what only the container sees and asks
the agent's gateway, at 127.0.0.1:7420. Each command prints JSON lines:

  metricas.py muestra <PALS_HOME>
      The time; the counters of every interface but lo
      (/sys/class/net/<if>/statistics: rx_bytes, tx_bytes, rx_packets,
      tx_packets); the daemon's memory (VmRSS and VmHWM, in KiB, of
      /proc/<pid>/status, the PID in <PALS_HOME>/daemon.pid, null when it is
      not running); and its GET /dht (null when it does not answer 200).
  metricas.py busca <name> <body>
      POST /discovery/search with that JSON body, as `pals discover` does:
      the name, the HTTP code, the answer, when it started and ended, and
      the bytes through the interfaces meanwhile.
  metricas.py rafaga <n> <at once> <capability>...
      n searches, so many at once, cycling over the capabilities; a line
      each, as busca, with its index. Bytes are not counted: the searches
      overlap.
  metricas.py serie <stop file> <capability>...
      searches one after another, cycling over the capabilities, until the
      stop file exists; a line each, as busca.
  metricas.py contacto <PALS_HOME> [<agent id>]
      The agent's own contact record in force (its store's own_records), or
      the newest one it keeps of that agent (peer_records): its seq and its
      direct addresses; null when there is none.

On the host, over what the harness kept in <registro>:

  metricas.py informe <registro>
      The metrics of design §14 for the run: the searches of case 2 (the
      network whole and at rest), the idle measurement per role, and what
      the cases measured (registro/casos/*.json). Writes
      <registro>/metricas.json and prints a summary.

Percentiles are nearest-rank. Nothing here judges a case: the harness does.
"""

import concurrent.futures
import json
import math
import os
import socket
import statistics
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

GATEWAY = "http://127.0.0.1:7420"
COUNTERS = ("rx_bytes", "tx_bytes", "rx_packets", "tx_packets")
ROLES = {"router": ("r1", "r2", "r3", "r4"), "proveedor": ("p1", "p2", "p3", "p4"),
         "cliente": ("c1", "c2", "c3", "c4")}


# --- in a container ---------------------------------------------------------

def counters():
    """Every interface's counters but lo's."""
    out = {}
    base = Path("/sys/class/net")
    for iface in sorted(os.listdir(base)):
        if iface == "lo":
            continue
        stats = base / iface / "statistics"
        try:
            out[iface] = {k: int((stats / k).read_text()) for k in COUNTERS}
        except (OSError, ValueError):
            continue
    return out


def delta(before, after, key):
    """What the interfaces both samples have moved in `key`."""
    return sum(after[i][key] - before[i][key] for i in after if i in before)


def memory(home):
    """VmRSS and VmHWM of the daemon of `home`, in KiB; None when it is not
    running."""
    try:
        pid = int(Path(home, "daemon.pid").read_text().strip())
        status = Path(f"/proc/{pid}/status").read_text()
    except (OSError, ValueError):
        return None
    out = {}
    for line in status.splitlines():
        key, _, value = line.partition(":")
        if key in ("VmRSS", "VmHWM"):
            out[key.lower() + "_kib"] = int(value.split()[0])
    return out or None


def request(method, path, body=None, timeout=120):
    """(HTTP code, bytes, error) of a request to the gateway."""
    data = body.encode() if body is not None else None
    req = urllib.request.Request(GATEWAY + path, data=data, method=method,
                                 headers={"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as answer:
            return answer.status, answer.read(), None
    except urllib.error.HTTPError as err:
        return err.code, err.read(), None
    except (urllib.error.URLError, OSError) as err:
        return None, b"", str(err)


def parsed(raw):
    try:
        return json.loads(raw)
    except ValueError:
        return None


def sample(home):
    code, raw, _ = request("GET", "/dht", timeout=30)
    return {"t": time.time(), "net": counters(), "memoria": memory(home),
            "dht": parsed(raw) if code == 200 else None}


def search(name, body, measure=True):
    before = counters() if measure else None
    t0 = time.time()
    code, raw, error = request("POST", "/discovery/search", body)
    t1 = time.time()
    line = {"nombre": name, "nodo": socket.gethostname(), "cuerpo": parsed(body), "http": code,
            "t0": t0, "t1": t1, "error": error}
    if measure:
        after = counters()
        line["rx"] = delta(before, after, "rx_bytes")
        line["tx"] = delta(before, after, "tx_bytes")
    answer = parsed(raw)
    line["respuesta"] = answer
    if answer is None:
        line["texto"] = raw.decode("utf-8", "replace")[:2000]
    return line


def body_of(capability):
    return json.dumps({"search": {"capability": capability}})


def emit(line):
    print(json.dumps(line, sort_keys=True), flush=True)


def burst(n, at_once, capabilities):
    def one(i):
        capability = capabilities[i % len(capabilities)]
        line = search(f"rafaga-{i + 1:03d}", body_of(capability), measure=False)
        line["capacidad"] = capability
        return line

    with concurrent.futures.ThreadPoolExecutor(max_workers=at_once) as pool:
        for line in pool.map(one, range(n)):
            emit(line)


def series(stop, capabilities):
    i = 0
    while not Path(stop).exists():
        capability = capabilities[i % len(capabilities)]
        line = search(f"serie-{i + 1:03d}", body_of(capability))
        line["capacidad"] = capability
        emit(line)
        i += 1


def contact(home, agent=None):
    import sqlite3
    db = sqlite3.connect(f"file:{Path(home, 'pals.db')}?mode=ro", uri=True)
    if agent is None:
        row = db.execute("SELECT seq, record FROM own_records WHERE kind = 'contact'").fetchone()
    else:
        row = db.execute("SELECT seq, record FROM peer_records WHERE agent_id = ? AND kind = 'contact'",
                         (agent,)).fetchone()
    if row is None or row[1] is None:
        return None
    record = json.loads(row[1])["record"]
    return {"seq": row[0], "direct_addrs": record["iroh"]["direct_addrs"], "issued_at": record["issued_at"]}


# --- on the host ------------------------------------------------------------

def pct(values, q):
    values = sorted(v for v in values if v is not None)
    if not values:
        return None
    return values[max(1, math.ceil(q * len(values))) - 1]


def spread(values):
    """p50, p95, min and max of the values, None when there are none."""
    values = [v for v in values if v is not None]
    if not values:
        return None
    return {"p50": pct(values, 0.5), "p95": pct(values, 0.95), "min": min(values), "max": max(values),
            "n": len(values)}


def idle_rates(reg):
    """Per node, from the idle measurement: bytes per second through its
    interfaces (both ways), the mean and peak of its RSS, and the mean of its
    open agentnet-kad/0 connections."""
    path = reg / "reposo.jsonl"
    if not path.exists():
        return {}
    samples = {}
    for line in path.read_text().splitlines():
        if line.strip():
            doc = json.loads(line)
            samples.setdefault(doc["nodo"], []).append(doc["muestra"])
    out = {}
    for node, rows in samples.items():
        rows.sort(key=lambda r: r["t"])
        first, last = rows[0], rows[-1]
        seconds = last["t"] - first["t"]
        moved = delta(first["net"], last["net"], "rx_bytes") + delta(first["net"], last["net"], "tx_bytes")
        rss = [r["memoria"]["vmrss_kib"] for r in rows if r.get("memoria")]
        conns = [r["dht"]["connections"] for r in rows if r.get("dht")]
        out[node] = {
            "segundos": round(seconds),
            "bytes_por_s": moved / seconds if seconds > 0 else None,
            "bytes_por_min": round(60 * moved / seconds) if seconds > 0 else None,
            "rss_media_kib": round(statistics.mean(rss)) if rss else None,
            "rss_max_kib": max(rss) if rss else None,
            "conexiones_media": round(statistics.mean(conns), 1) if conns else None,
            "conexiones_max": max(conns) if conns else None,
            "muestras": len(rows),
        }
    return out


def report(reg):
    reg = Path(reg)
    searches = []
    for f in sorted((reg / "busquedas").glob("caso2-*.json")):
        searches.append(json.loads(f.read_text()))
    answered = [s for s in searches if s["http"] == 200 and isinstance(s.get("respuesta"), dict)]
    outcomes = [s["respuesta"] for s in answered]
    idle = idle_rates(reg)
    net_bytes = []
    for s in answered:
        node = s["nombre"].split("-")[1]
        rate = (idle.get(node) or {}).get("bytes_por_s")
        if rate is not None and s.get("rx") is not None:
            net_bytes.append(s["rx"] + s["tx"] - rate * (s["t1"] - s["t0"]))

    def metric(key):
        return spread([o["metrics"].get(key) for o in outcomes])

    with_candidate = sum(1 for o in outcomes if o["ranked"])
    out = {
        "busquedas_caso2": len(searches),
        "respondidas_caso2": len(answered),
        "con_candidato_caso2": with_candidate,
        "con_candidato_pct_caso2": round(100 * with_candidate / len(searches), 1) if searches else None,
        "primer_valido_ms": metric("time_to_first_valid_ms"),
        "elapsed_ms": metric("elapsed_ms"),
        # Omitted when 0: a search whose lookup sent nothing.
        "dht_requests": spread([o["metrics"].get("dht_requests", 0) for o in outcomes]),
        "contactados": metric("contacted"),
        "mensajes": metric("messages"),
        "bytes_enviados_busqueda": metric("bytes_sent"),
        "bytes_recibidos_busqueda": metric("bytes_received"),
        "bytes_interfaz_brutos": spread([s["rx"] + s["tx"] for s in answered if s.get("rx") is not None]),
        "bytes_interfaz_netos": spread([round(b) for b in net_bytes]),
        "matching_us": metric("matching_us"),
        "coberturas": sorted({json.dumps(o["coverage"], sort_keys=True) for o in outcomes}),
    }
    roles = {}
    for role, nodes in ROLES.items():
        rows = [idle[n] for n in nodes if n in idle]
        if not rows:
            continue

        def agg(key, rows=rows):
            values = [r[key] for r in rows if r.get(key) is not None]
            return {"media": round(statistics.mean(values), 1), "min": min(values), "max": max(values)} if values else None

        roles[role] = {"nodos": len(rows), "segundos": rows[0]["segundos"], "bytes_por_min": agg("bytes_por_min"),
                       "rss_media_kib": agg("rss_media_kib"), "rss_max_kib": agg("rss_max_kib"),
                       "conexiones_media": agg("conexiones_media"), "conexiones_max": agg("conexiones_max")}
    out["reposo_por_papel"] = roles
    out["reposo_por_nodo"] = idle
    cases = {}
    for f in sorted((reg / "casos").glob("*.json")):
        cases[f.stem] = json.loads(f.read_text())
    out["casos"] = cases
    (reg / "metricas.json").write_text(json.dumps(out, indent=1, sort_keys=True) + "\n")

    def line(label, value):
        print(f"{label}: {json.dumps(value, sort_keys=True, ensure_ascii=False)}")

    print(f"caso 2: {out['busquedas_caso2']} búsquedas, {out['respondidas_caso2']} respondidas, "
          f"{with_candidate} con candidato ({out['con_candidato_pct_caso2']} %)")
    for key in ("primer_valido_ms", "elapsed_ms", "dht_requests", "contactados", "mensajes",
                "bytes_enviados_busqueda", "bytes_recibidos_busqueda", "bytes_interfaz_brutos",
                "bytes_interfaz_netos", "matching_us", "coberturas"):
        line(key, out[key])
    for role, values in roles.items():
        line(f"reposo {role}", values)
    for name, values in cases.items():
        line(name, values)


def main(argv):
    if len(argv) < 2:
        raise SystemExit(__doc__)
    command, args = argv[1], argv[2:]
    if command == "muestra" and len(args) == 1:
        emit(sample(args[0]))
    elif command == "busca" and len(args) == 2:
        emit(search(args[0], args[1]))
    elif command == "rafaga" and len(args) >= 3:
        burst(int(args[0]), int(args[1]), args[2:])
    elif command == "serie" and len(args) >= 2:
        series(args[0], args[1:])
    elif command == "contacto" and len(args) in (1, 2):
        emit(contact(*args))
    elif command == "informe" and len(args) == 1:
        report(args[0])
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
