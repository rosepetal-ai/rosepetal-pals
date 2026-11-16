#!/usr/bin/env python3
"""The JSON side of harness.sh: observations, waits and the scenarios' outcome.

harness.sh runs the containers and the order of things; everything that
reads or writes JSON is here, so that the shell never parses it:

- observations: a search, the stats of some nodes, the interface counters
  or an announce, each through `docker exec <container> curl` on the node's
  control API (127.0.0.1:7600), appended as one JSON line to
  <raw>/<scenario>.jsonl, with the times of the container's own clock;
- `reached <scenario>` and `settled <tag>`: whether what a polling loop of
  harness.sh waits for has happened, from the lines so far (exit 0 or 1);
- `results`: every scenario's status, metrics and notes from those lines,
  written as the results JSON of the candidate.

The scenarios a backend declares on /healthz (`declared`) take the status
it declares: `not-applicable` ones are not run, and a `by-design` S1 runs
and must find no valid candidate. Percentiles are nearest-rank.
"""

import argparse
import json
import math
import subprocess
import sys
import time
from pathlib import Path

SCENARIOS = [f"S{i}" for i in range(8)]
STATUSES = ("pass", "fail", "by-design", "not-applicable", "error", "measured")
ROLES = ("router", "provider", "client")
API = "http://127.0.0.1:7600"

# The interface counters of the container, every interface but loopback:
# "<unix ns> <rx bytes> <tx bytes>".
COUNTERS_SH = r"""
rx=0; tx=0
for d in /sys/class/net/*; do
    [ "${d##*/}" = lo ] && continue
    read -r r < "$d/statistics/rx_bytes"; read -r t < "$d/statistics/tx_bytes"
    rx=$((rx + r)); tx=$((tx + t))
done
echo "$(date +%s%N) $rx $tx"
"""
# A search between two readings of the counters: the counters, the answer's
# body, its HTTP code, the counters again, one per line. $1 is the request,
# $2 the timeout.
SEARCH_SH = (
    "cnt() {" + COUNTERS_SH + "}\n"
    "cnt\n"
    'curl -sS -m "$2" -X POST -H "content-type: application/json" --data-binary "$1"'
    ' -w "\\n%{http_code}\\n" ' + API + "/search\n"
    "cnt\n"
)


def read_params(path):
    """params.env as a dict: integers where the value is all digits."""
    params = {}
    for number, raw in enumerate(Path(path).read_text(encoding="utf-8").splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep or not key.isidentifier() or "$" in value or "`" in value:
            raise SystemExit(f"{path}:{number}: not a plain NAME=value line")
        if len(value) >= 2 and value[0] == value[-1] == '"':
            value = value[1:-1]
        params[key] = int(value) if value.isdigit() else value
    return params


def pct(values, q):
    values = sorted(v for v in values if v is not None)
    if not values:
        return None
    return values[max(1, math.ceil(q * len(values))) - 1]


def rnd(value, digits=1):
    if value is None:
        return None
    return round(value) if digits == 0 else round(value, digits)


class Ctx:
    def __init__(self, raw, params, prefix):
        self.raw = Path(raw)
        self.p = read_params(params)
        self.prefix = prefix

    def words(self, key):
        return str(self.p.get(key, "")).split()

    @property
    def routers(self):
        return self.words("ROUTERS")

    @property
    def providers(self):
        return self.words("PROVIDERS_A") + self.words("PROVIDERS_B")

    @property
    def clients(self):
        return self.words("CLIENTS_A") + self.words("CLIENTS_B")

    def role(self, node):
        if node in self.routers:
            return "router"
        return "provider" if node in self.providers else "client"

    def cap(self, short):
        return self.p["CAPABILITY_PREFIX"] + short

    def health(self, node):
        path = self.raw / f"healthz-{node}.json"
        return json.loads(path.read_text()) if path.exists() else None

    def agent(self, node):
        health = self.health(node)
        return health.get("agent_id") if health else None

    def expected(self, capability):
        """The agent ids of the providers that announce `capability`."""
        return {
            self.agent(p)
            for p in self.providers
            if capability in [self.cap(c) for c in self.words(f"CAPS_{p}")]
        }

    def declared(self, scenario):
        for node in self.routers:
            health = self.health(node)
            if health is not None:
                return next((d for d in health.get("declared", []) if d["scenario"] == scenario), None)
        return None

    def path(self, name):
        return self.raw / f"{name}.jsonl"

    def lines(self, name):
        path = self.path(name)
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]

    def append(self, name, line):
        with self.path(name).open("a") as out:
            out.write(json.dumps(line, sort_keys=True) + "\n")


# --- observations ---------------------------------------------------------


def exec_in(ctx, node, argv, timeout):
    try:
        done = subprocess.run(
            ["docker", "exec", f"{ctx.prefix}-{node}", *argv],
            capture_output=True, text=True, timeout=timeout,
        )
        return done.returncode, done.stdout, done.stderr
    except subprocess.TimeoutExpired:
        return None, "", f"docker exec gave no answer in {timeout} s"


def counters_of(text):
    """(seconds, bytes received and sent, bytes sent) of a COUNTERS_SH line."""
    t_ns, rx, tx = (int(x) for x in text.split())
    return t_ns / 1e9, rx + tx, tx


def api(ctx, node, method, path, body=None, timeout=30):
    """(http code or None, the answer's JSON or None, error text or None)."""
    argv = ["curl", "-sS", "-m", str(timeout), "-X", method, "-w", "\n%{http_code}"]
    if body is not None:
        argv += ["-H", "content-type: application/json", "--data-binary", body]
    rc, out, err = exec_in(ctx, node, argv + [API + path], timeout + 30)
    text, _, code = out.rpartition("\n")
    if not code.isdigit() or code == "000":
        return None, None, f"no answer (exit {rc}): {err.strip()[:300]}"
    try:
        return int(code), json.loads(text) if text else None, None
    except json.JSONDecodeError:
        return int(code), None, f"not JSON: {text[:300]}"


def observe_search(ctx, file, node, capability, tag):
    body = json.dumps({"capability": capability, "limit": ctx.p["SEARCH_LIMIT"]})
    limit = ctx.p["SEARCH_TIMEOUT_S"]
    rc, out, err = exec_in(ctx, node, ["sh", "-c", SEARCH_SH, "sh", body, str(limit)], limit + 60)
    line = {"kind": "search", "tag": tag, "node": node, "capability": capability,
            "t0": None, "t1": None, "bytes": None, "http": None, "answer": None, "error": None}
    parts = out.split("\n")
    try:
        (t0, b0, _), (t1, b1, _) = counters_of(parts[0]), counters_of(parts[3])
        line.update(t0=t0, t1=t1, bytes=b1 - b0)
        code = int(parts[2])
        line["http"] = code or None
        if code == 200:
            line["answer"] = json.loads(parts[1])
        elif code:
            line["error"] = f"HTTP {code}: {parts[1][:300]}"
        else:
            line["error"] = f"no answer from the control API: {err.strip()[:300]}"
    except (IndexError, ValueError):
        line["error"] = f"the search did not run (exit {rc}): {err.strip()[:300]}"
    ctx.append(file, line)
    return line


def observe_stats(ctx, nodes):
    stats, errors = {}, {}
    for node in nodes:
        code, answer, error = api(ctx, node, "GET", "/stats")
        if code == 200 and isinstance(answer, dict):
            stats[node] = answer
        else:
            errors[node] = error or f"HTTP {code}"
    return {"kind": "stats", "t": time.time(), "nodes": stats, "errors": errors}


# --- outcomes ---------------------------------------------------------------


def outcome(status, metrics, notes, done=False):
    return {"status": status, "metrics": metrics, "notes": notes, "done": done}


def of(lines, kind, tag=None):
    return [l for l in lines if l["kind"] == kind and (tag is None or l.get("tag") == tag)]


def event(lines, name):
    return next((l for l in lines if l["kind"] == "event" and l["name"] == name), None)


def cands(search):
    return (search.get("answer") or {}).get("candidates") or []


def valid_ids(search):
    return {c["agent_id"] for c in cands(search) if c.get("valid")}


def seen_ids(search):
    return {c["agent_id"] for c in cands(search) if c.get("agent_id")}


def failed(search):
    """Whether the harness got no answer at all (a backend error is an answer)."""
    return search["http"] is None


def complete(ctx, search):
    return ctx.expected(search["capability"]) <= valid_ids(search)


def coverage(ctx, searches):
    expected = sum(len(ctx.expected(s["capability"])) for s in searches)
    found = sum(len(ctx.expected(s["capability"]) & valid_ids(s)) for s in searches)
    return found / expected if expected else None


def harness_failures(searches):
    return [f"{s['node']}: {s['error']}" for s in searches if failed(s)]


def rounds(searches):
    """The searches grouped by tag, in the order the tags first appear."""
    groups = {}
    for s in searches:
        groups.setdefault(s["tag"], []).append(s)
    return list(groups.values())


def stored(stats_line, agent, nodes, fields=("contact_records", "announce_records", "provider_records")):
    """What `nodes` store about `agent` in a stats line; None if one did not answer."""
    total = 0
    for node in nodes:
        if node not in stats_line["nodes"]:
            return None
        records = stats_line["nodes"][node].get("stored_by_agent", {}).get(agent, {})
        total += sum(records.get(f, 0) for f in fields)
    return total


def s0(ctx, lines):
    searches = of(lines, "search")
    errors = harness_failures(searches)
    found = sum(len(cands(s)) for s in searches)
    backend_errors = sum(1 for s in searches if s["http"] not in (None, 200))
    end = of(lines, "stats")
    node_stats = end[-1]["nodes"].get(ctx.p["S0_NODE"], {}) if end else {}
    metrics = {
        "searches": len(searches),
        "candidates": found,
        "backend_errors": backend_errors,
        "routing_peers": node_stats.get("routing_peers"),
        "connections": node_stats.get("connections"),
    }
    if errors or not searches:
        return outcome("error", metrics, "Sin respuesta en búsquedas de S0: " + "; ".join(errors))
    notes = (f"{ctx.p['S0_NODE']}, sin lista de arranque, buscó cada capacidad tras "
             f"{ctx.p['S0_GRACE_S']} s en la red A con los proveedores anunciados: ")
    if found:
        return outcome("fail", metrics, notes + f"encontró {found} candidatos.")
    extra = f" ({backend_errors} búsquedas terminaron en error del backend)" if backend_errors else ""
    return outcome("pass", metrics, notes + "no encontró ningún candidato" + extra + ".")


def s1(ctx, lines, by_design=None):
    searches = of(lines, "search")
    errors = harness_failures(searches)
    answered = [s for s in searches if s["answer"] is not None]
    idle = idle_rates(ctx)
    net_bytes = [
        s["bytes"] - idle[s["node"]] * (s["t1"] - s["t0"])
        for s in searches if s["bytes"] is not None and s["node"] in idle
    ]
    n = len(searches)
    metrics = {
        "searches": n,
        "with_valid_pct": rnd(100 * sum(1 for s in searches if valid_ids(s)) / n) if n else None,
        "complete_pct": rnd(100 * sum(1 for s in searches if complete(ctx, s)) / n) if n else None,
        "first_valid_ms_p50": pct([s["answer"]["first_valid_ms"] for s in answered], 0.5),
        "first_valid_ms_p95": pct([s["answer"]["first_valid_ms"] for s in answered], 0.95),
        "elapsed_ms_p50": pct([s["answer"]["elapsed_ms"] for s in answered], 0.5),
        "elapsed_ms_p95": pct([s["answer"]["elapsed_ms"] for s in answered], 0.95),
        "requests_p50": pct([s["answer"].get("requests") for s in answered], 0.5),
        "requests_p95": pct([s["answer"].get("requests") for s in answered], 0.95),
        "candidates_p50": pct([len(cands(s)) for s in answered], 0.5),
        "candidates_max": max((len(cands(s)) for s in answered), default=None),
        "bytes_raw_p50": pct([s["bytes"] for s in searches], 0.5),
        "bytes_p50": rnd(pct(net_bytes, 0.5), 0),
        "bytes_p95": rnd(pct(net_bytes, 0.95), 0),
        "backend_errors": sum(1 for s in searches if s["http"] not in (None, 200)),
    }
    if errors or not searches:
        return outcome("error", metrics, "Sin respuesta en búsquedas de S1: " + "; ".join(errors[:5]))
    clients, caps = len({s["node"] for s in searches}), len({s["capability"] for s in searches})
    notes = (f"{n} búsquedas: {clients} clientes, {caps} capacidades, {ctx.p['SEARCHES']} veces cada una. "
             "Bytes por búsqueda: los de la interfaz del cliente menos su tráfico en reposo de S6 en el mismo tiempo.")
    if not idle:
        notes += " Sin S6, los bytes netos faltan."
    if by_design is not None:
        if any(valid_ids(s) for s in searches):
            return outcome("error", metrics, "Declarado by-design, pero hubo candidatos válidos: " + notes)
        return outcome("by-design", metrics, by_design["notes"] + " " + notes)
    if all(complete(ctx, s) for s in searches):
        return outcome("pass", metrics, "Cada búsqueda devolvió los dos proveedores, verificados. " + notes)
    return outcome("fail", metrics, f"Solo el {metrics['complete_pct']} % de las búsquedas devolvió los dos proveedores. " + notes)


def s2(ctx, lines):
    stop, start = event(lines, "stop"), event(lines, "start")
    connected = of(lines, "search", "connected")
    newcomer = of(lines, "search", "newcomer")
    errors = harness_failures(connected + newcomer)
    first = next((s for s in newcomer if valid_ids(s)), None)
    caps = [ctx.cap(c) for c in ctx.words("CAPABILITIES")]
    newcomer_complete = all(any(complete(ctx, s) for s in newcomer if s["capability"] == c) for c in caps)
    metrics = {
        "connected_complete_pct": rnd(100 * sum(1 for s in connected if complete(ctx, s)) / len(connected)) if connected else None,
        "connected_first_valid_ms_p50": pct([s["answer"]["first_valid_ms"] for s in connected if s["answer"]], 0.5),
        "newcomer_first_valid_s": rnd(first["t1"] - start["t"]) if first and start else None,
        "newcomer_complete": newcomer_complete,
        "newcomer_searches": len(newcomer),
    }
    done = bool(newcomer) and newcomer_complete
    if errors or not stop or not start or not connected:
        return outcome("error", metrics, "S2 incompleto: " + "; ".join(errors[:5]), done)
    node = ctx.p["S2_NODE"]
    notes = (f"Parado {ctx.p['BOOTSTRAP']}, cada cliente buscó cada capacidad una vez; después arrancó {node} "
             f"con [{ctx.p['BOOTSTRAP']}, {ctx.p['SAVED_CONTACT']}] y buscó hasta encontrar a los dos proveedores "
             f"de cada capacidad, {2 * ctx.p['REPUBLISH_S']} s como mucho. "
             f"newcomer_first_valid_s cuenta desde que arrancó su contenedor.")
    ok = all(complete(ctx, s) for s in connected) and newcomer_complete
    return outcome("pass" if ok else "fail", metrics, notes, done)


def s3(ctx, lines):
    stop = event(lines, "stop")
    victim = ctx.p["S3_NODE"]
    agent = ctx.agent(victim)
    before = of(lines, "stats", "before")
    polls = of(lines, "stats", "poll")
    searches = of(lines, "search")
    errors = harness_failures(searches)
    pre = stored(before[0], agent, ctx.routers) if before else None
    limit = ctx.p["TTL_S"] + ctx.p["REPUBLISH_S"]
    routers_gone = next((p["t"] for p in polls if stored(p, agent, ctx.routers) == 0), None)
    # Gone from the searches once a whole round returns it nowhere; the time
    # is that of the first search after the last one that returned it.
    gone_round = next((g for g in rounds(searches) if all(not failed(s) for s in g)
                       and not any(agent in seen_ids(s) for s in g)), None)
    search_gone = None
    if gone_round:
        returned = [s["t1"] for s in searches if agent in seen_ids(s) and s["t1"] < gone_round[0]["t1"]]
        after = [s["t1"] for s in searches if not returned or s["t1"] > max(returned)]
        search_gone = min(after)
    t = stop["t"] if stop else None
    metrics = {
        "stored_before": pre,
        "routers_forget_s": rnd(routers_gone - t) if routers_gone and t else None,
        "searches_forget_s": rnd(search_gone - t) if search_gone and t else None,
        "limit_s": limit,
        "polls": len(polls),
    }
    done = routers_gone is not None and search_gone is not None
    if errors or not stop or pre is None:
        return outcome("error", metrics, "S3 incompleto: " + "; ".join(errors[:5]), done)
    notes = (f"Parado {victim} sin aviso (docker kill). Cada {ctx.p['POLL_S']} s, las estadísticas de los routers "
             f"(lo que guardan de {victim}) y una búsqueda por cliente de una capacidad de {victim}, hasta una ronda "
             f"en que ninguna lo devuelve; searches_forget_s cuenta en la primera búsqueda después de la última que "
             "lo devolvió.")
    if pre == 0:
        return outcome("fail", metrics, f"Los routers no guardaban nada de {victim} antes de pararlo: "
                       "la caducidad en los routers no se puede comprobar. " + notes, done)
    ok = done and metrics["routers_forget_s"] < limit and metrics["searches_forget_s"] < limit
    return outcome("pass" if ok else "fail", metrics, notes, done)


def s4(ctx, lines):
    stop = event(lines, "stop")
    searches = of(lines, "search")
    errors = harness_failures(searches)
    groups = rounds(searches)
    limit = 2 * ctx.p["REPUBLISH_S"]
    done = bool(groups) and coverage(ctx, groups[-1]) == 1
    t = stop["t"] if stop else None
    # Recovered once a whole round is complete; the time is that of the
    # first complete search, of the same client and capability, after the
    # last incomplete one: 0 when no search was ever incomplete.
    full = None
    if done and t:
        short = [s for s in searches if not complete(ctx, s)]
        if not short:
            full = t
        else:
            last = max(short, key=lambda s: s["t1"])
            full = min(s["t1"] for s in searches if s["t1"] > last["t1"] and complete(ctx, s)
                       and (s["node"], s["capability"]) == (last["node"], last["capability"]))
    metrics = {
        "immediate_coverage": rnd(coverage(ctx, groups[0]), 3) if groups else None,
        "full_coverage_s": rnd(full - t) if full is not None and t else None,
        "rounds": len(groups),
        "limit_s": limit,
    }
    if errors or not stop or not groups:
        return outcome("error", metrics, "S4 incompleto: " + "; ".join(errors[:5]), done)
    notes = (f"Parados {ctx.p['S4_NODES']}. Rondas seguidas en que cada cliente busca cada capacidad, hasta "
             "una ronda completa; la cobertura es la fracción de los proveedores esperados que se encuentran. "
             "La cobertura completa cuenta en la primera búsqueda completa, del mismo cliente y capacidad, "
             "después de la última incompleta, y es 0 si ninguna lo fue.")
    ok = metrics["full_coverage_s"] is not None and metrics["full_coverage_s"] < limit
    return outcome("pass" if ok else "fail", metrics, notes, done)


def s5(ctx, lines):
    rogue = ctx.agent(ctx.p["S5_NODE"])
    before = of(lines, "stats", "before")
    after = of(lines, "stats", "poll") + of(lines, "stats", "after")
    searches = of(lines, "search")
    errors = harness_failures(searches)
    base = before[0]["nodes"] if before else {}

    def rejecting(line):
        return [r for r in ctx.routers if r in line["nodes"] and r in base
                and line["nodes"][r]["records_rejected"] > base[r]["records_rejected"]]

    rejected = max((len(rejecting(l)) for l in after), default=0)
    delta = max((sum(l["nodes"][r]["records_rejected"] - base[r]["records_rejected"]
                     for r in ctx.routers if r in l["nodes"] and r in base) for l in after), default=0)
    storing = max((sum(1 for r in ctx.routers if r in l["nodes"]
                       and l["nodes"][r].get("stored_by_agent", {}).get(rogue, {}).get("contact_records", 0))
                   for l in after), default=0)
    valid = sum(1 for s in searches if rogue in valid_ids(s))
    seen = [c.get("reason") for s in searches for c in cands(s) if c.get("agent_id") == rogue]
    metrics = {
        "routers_rejecting": rejected,
        "records_rejected_delta": delta,
        "routers_storing_rogue_contact": storing,
        "clients_rogue_valid": valid,
        "clients_rogue_returned": len(seen),
        "searches": len(searches),
    }
    done = rejected > 0
    if errors or not before or rogue is None or not searches:
        return outcome("error", metrics, "S5 incompleto: " + "; ".join(errors[:5]), done)
    reasons = ", ".join(sorted(set(r or "válido" for r in seen))) or "ninguno"
    notes = (f"El nodo {ctx.p['S5_NODE']} anunció {ctx.p['CAPS_rogue']} con un contacto de firma alterada en un byte. "
             f"Motivos con que lo devolvieron los clientes: {reasons}.")
    ok = rejected > 0 and storing == 0 and valid == 0
    return outcome("pass" if ok else "fail", metrics, notes, done)


def idle_rates(ctx, key="bytes"):
    """Bytes per second through each node's interfaces during S6: received
    and sent (`bytes`), or sent only (`sent`)."""
    lines = ctx.lines("S6")
    start = {l["node"]: l for l in of(lines, "counters", "start")}
    end = {l["node"]: l for l in of(lines, "counters", "end")}
    rates = {}
    for node, a in start.items():
        b = end.get(node)
        if b and a.get(key) is not None and b.get(key) is not None and b["t"] > a["t"]:
            rates[node] = (b[key] - a[key]) / (b["t"] - a["t"])
    return rates


def s6(ctx, lines):
    rates, sent = idle_rates(ctx), idle_rates(ctx, "sent")
    samples = of(lines, "stats", "sample")
    counters = of(lines, "counters")
    errors = [f"{l['node']}: {l['error']}" for l in counters if l.get("error")]
    errors += [f"{n}: {e}" for l in samples for n, e in l["errors"].items()]
    metrics = {}
    topology = ctx.routers + ctx.providers + ctx.clients
    for role in ROLES:
        nodes = [n for n in topology if ctx.role(n) == role]
        for key in ("rss_kib", "connections", "routing_peers"):
            values = [l["nodes"][n].get(key) for l in samples for n in nodes if n in l["nodes"]]
            metrics[f"{role}_{key}_p50"] = pct(values, 0.5)
            metrics[f"{role}_{key}_max"] = max((v for v in values if v is not None), default=None)
        for name, per_s in (("bytes", rates), ("sent_bytes", sent)):
            per_min = [per_s[n] * 60 for n in nodes if n in per_s]
            metrics[f"{role}_{name}_per_min_p50"] = rnd(pct(per_min, 0.5), 0)
            metrics[f"{role}_{name}_per_min_max"] = rnd(max(per_min), 0) if per_min else None
    notes = (f"{ctx.p['IDLE_S']} s sin búsquedas, con los {len(topology)} nodos de la topología. Memoria, conexiones "
             f"y tabla de rutas muestreadas cada {ctx.p['IDLE_SAMPLE_S']} s; bytes por minuto, de los contadores de "
             "todas las interfaces del contenedor menos lo, al empezar y al acabar: recibidos y enviados (bytes), "
             "y solo enviados (sent_bytes), porque un contenedor que no envía nada también recibe el tráfico de "
             "fondo del puente de Docker. p50 y máximo por papel.")
    if errors or len(rates) < len(topology) or not samples:
        return outcome("error", metrics, "S6 incompleto: " + "; ".join(errors[:5]) + " " + notes)
    return outcome("measured", metrics, notes)


def s7(ctx, lines):
    move, moved = event(lines, "move"), event(lines, "moved")
    announce = of(lines, "announce")
    searches = of(lines, "search")
    errors = harness_failures(searches)
    mover = ctx.p["S7_NODE"]
    agent = ctx.agent(mover)
    ip = moved.get("ip") if moved else None
    limit = 2 * ctx.p["REPUBLISH_S"]
    resolved = {}
    for s in searches:
        for c in cands(s):
            if (c.get("agent_id") == agent and c.get("valid") and s["node"] not in resolved
                    and any(a.startswith(f"{ip}:") for a in c.get("direct_addrs", []))):
                resolved[s["node"]] = s["t1"]
    t = move["t"] if move else None
    times = [v - t for v in resolved.values()] if t else []
    clients = ctx.clients
    metrics = {
        "clients_resolved": len(resolved),
        "resolved_s_p50": rnd(pct(times, 0.5)),
        "resolved_s_max": rnd(max(times)) if len(resolved) == len(clients) and times else None,
        "limit_s": limit,
        "searches": len(searches),
    }
    done = len(resolved) == len(clients)
    if errors or not move or not ip or not announce or announce[0].get("http") is None:
        return outcome("error", metrics, "S7 incompleto: " + "; ".join(errors[:5]), done)
    notes = (f"{mover} salió de la red B y entró en la A, con la IP {ip}, y anunció de nuevo sus registros "
             "(POST /announce). Cada cliente busca una capacidad suya por ronda; se resuelve cuando un candidato "
             "válido de " + mover + " lleva la IP nueva. El tiempo cuenta desde la desconexión.")
    if announce[0].get("http") != 200:
        return outcome("fail", metrics, "El nuevo anuncio falló: " + str(announce[0].get("answer")) + ". " + notes, done)
    ok = done and metrics["resolved_s_max"] < limit
    return outcome("pass" if ok else "fail", metrics, notes, done)


EVALUATE = {"S0": s0, "S1": s1, "S2": s2, "S3": s3, "S4": s4, "S5": s5, "S6": s6, "S7": s7}


def evaluate(ctx, scenario):
    declared = ctx.declared(scenario)
    if declared and declared["status"] == "not-applicable":
        return outcome("not-applicable", {}, declared["notes"])
    lines = ctx.lines(scenario)
    if not lines:
        return outcome("error", {}, "El arnés no ejecutó el escenario.")
    if scenario == "S1" and declared and declared["status"] == "by-design":
        result = s1(ctx, lines, by_design=declared)
    else:
        result = EVALUATE[scenario](ctx, lines)
    errors = [l["msg"] for l in lines if l["kind"] == "error"]
    if errors:
        result["status"] = "error"
        result["notes"] = "Error del arnés: " + "; ".join(errors) + ". " + result["notes"]
    return result


def settled(ctx, tag):
    """Whether what the routers store stopped changing over the last polls
    and, unless the backend declares S1 by-design (it stores no one's
    announcements), every provider's records reached some router: a total
    that stays at what the routers had before a provider's first put is not
    settled."""
    polls = of(ctx.lines("settle"), "stats", tag)
    n = ctx.p["SETTLE_STABLE_POLLS"]
    if len(polls) < n:
        return False

    def total(line):
        if len(line["nodes"]) < len(ctx.routers):
            return None
        return sum(s["records_stored"] + s["provider_records_stored"] for s in line["nodes"].values())

    totals = [total(l) for l in polls[-n:]]
    if None in totals or len(set(totals)) != 1:
        return False
    declared = ctx.declared("S1")
    if declared and declared["status"] == "by-design":
        return True
    return all(stored(polls[-1], ctx.agent(p), ctx.routers) for p in ctx.providers)


# --- command line ---------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--raw", required=True, help="the run's directory of observations")
    parser.add_argument("--params", required=True, help="params.env")
    parser.add_argument("--prefix", default="pals-spike", help="the containers' name prefix")
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("search", help="searches from each node for each capability, in that order")
    p.add_argument("file"), p.add_argument("tag")
    p.add_argument("--nodes", required=True), p.add_argument("--caps", required=True)
    p = sub.add_parser("stats", help="one line with the stats of the nodes")
    p.add_argument("file"), p.add_argument("tag"), p.add_argument("nodes", nargs="+")
    p = sub.add_parser("counters", help="one line per node with its interface counters")
    p.add_argument("file"), p.add_argument("tag"), p.add_argument("nodes", nargs="+")
    p = sub.add_parser("announce", help="POST /announce on a node")
    p.add_argument("file"), p.add_argument("node"), p.add_argument("caps", nargs="+")
    p = sub.add_parser("event", help="one event line, with key=value fields")
    p.add_argument("file"), p.add_argument("name"), p.add_argument("fields", nargs="*")
    p = sub.add_parser("error", help="one harness error line")
    p.add_argument("file"), p.add_argument("msg")
    p = sub.add_parser("get", help="a field of a node's GET answer")
    p.add_argument("node"), p.add_argument("path"), p.add_argument("field")
    p = sub.add_parser("declared", help="the status a scenario is declared with, if any")
    p.add_argument("scenario")
    p = sub.add_parser("reached", help="exit 0 once what the scenario waits for happened")
    p.add_argument("scenario")
    p = sub.add_parser("settled", help="exit 0 once the routers' counts stopped changing")
    p.add_argument("tag")
    p = sub.add_parser("results", help="writes the results JSON")
    p.add_argument("--backend", required=True), p.add_argument("--commit", required=True)
    p.add_argument("--date", required=True), p.add_argument("--out", required=True)
    args = parser.parse_args()
    ctx = Ctx(args.raw, args.params, args.prefix)

    if args.command == "search":
        answered = True
        for node in args.nodes.split():
            for cap in args.caps.split():
                answered &= not failed(observe_search(ctx, args.file, node, ctx.cap(cap), args.tag))
        return 0 if answered else 1
    if args.command == "stats":
        line = observe_stats(ctx, args.nodes)
        line["tag"] = args.tag
        ctx.append(args.file, line)
        return 0 if not line["errors"] else 1
    if args.command == "counters":
        ok = True
        for node in args.nodes:
            rc, out, err = exec_in(ctx, node, ["sh", "-c", COUNTERS_SH], 60)
            line = {"kind": "counters", "tag": args.tag, "node": node, "t": None, "bytes": None,
                    "sent": None, "error": None}
            try:
                line["t"], line["bytes"], line["sent"] = counters_of(out)
            except ValueError:
                line["error"] = f"no counters (exit {rc}): {err.strip()[:300]}"
                ok = False
            ctx.append(args.file, line)
        return 0 if ok else 1
    if args.command == "announce":
        body = json.dumps({"capabilities": [ctx.cap(c) for c in args.caps]})
        code, answer, error = api(ctx, args.node, "POST", "/announce", body)
        ctx.append(args.file, {"kind": "announce", "t": time.time(), "node": args.node,
                               "http": code, "answer": answer, "error": error})
        return 0 if code == 200 else 1
    if args.command == "event":
        line = {"kind": "event", "name": args.name, "t": time.time()}
        line.update(field.split("=", 1) for field in args.fields)
        ctx.append(args.file, line)
        return 0
    if args.command == "error":
        ctx.append(args.file, {"kind": "error", "t": time.time(), "msg": args.msg})
        return 0
    if args.command == "get":
        code, answer, _ = api(ctx, args.node, "GET", args.path)
        value = answer.get(args.field) if code == 200 and isinstance(answer, dict) else None
        if value is None:
            return 1
        print(value if isinstance(value, str) else json.dumps(value))
        return 0
    if args.command == "declared":
        declared = ctx.declared(args.scenario)
        if declared:
            print(declared["status"])
        return 0
    if args.command == "reached":
        return 0 if EVALUATE[args.scenario](ctx, ctx.lines(args.scenario))["done"] else 1
    if args.command == "settled":
        return 0 if settled(ctx, args.tag) else 1
    if args.command == "results":
        scenarios = {}
        for s in SCENARIOS:
            result = evaluate(ctx, s)
            del result["done"]
            scenarios[s] = result
        results = {"backend": args.backend, "commit": args.commit, "date": args.date,
                   "params": ctx.p, "scenarios": scenarios}
        Path(args.out).write_text(json.dumps(results, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        for s in SCENARIOS:
            print(f"{s}: {scenarios[s]['status']}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
