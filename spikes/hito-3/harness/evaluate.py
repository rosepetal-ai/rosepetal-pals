"""The scenarios' judgement, from the observations harness.sh made.

Pure: it reads the run's directory of observations (<raw>/<scenario>.jsonl
and <raw>/healthz-<node>.json) and params.env, and never touches Docker;
scenarios.py makes the observations and test_evaluate.py checks this on
synthetic ones.

A search counts as answered only with HTTP 200; a 500 is the backend's own
error, an answer with no candidates; anything else (no answer, 400, 422,
503) is the harness's failure and puts the scenario in `error`. The
scenarios a backend declares on /healthz take the status it declares:
`not-applicable` ones are not run, and a `by-design` S1 runs and must find
no valid candidate. A scenario that `harness.sh --only` left out is
`not-applicable` too, by the `not-run` event the harness writes for it;
one with no observations at all is still `error`. Percentiles are
nearest-rank.
"""

import json
import math
from pathlib import Path

SCENARIOS = [f"S{i}" for i in range(8)]
ROLES = ("router", "provider", "client")
ATTRIBUTED = ("contact_records", "announce_records", "other_records")


def read_params(path, *overrides):
    """params.env as a dict: integers where the value is all digits. Each
    file of `overrides` (harness.sh --params) is read after it, in the same
    form, and may only change parameters that params.env has."""
    params = {}
    for index, source in enumerate((path, *overrides)):
        for number, raw in enumerate(Path(source).read_text(encoding="utf-8").splitlines(), 1):
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            key, sep, value = line.partition("=")
            if not sep or not key.isidentifier() or "$" in value or "`" in value:
                raise SystemExit(f"{source}:{number}: not a plain NAME=value line")
            if index and key not in params:
                raise SystemExit(f"{source}:{number}: {key} is not a parameter of {Path(path).name}")
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
    """A run's observations and parameters, and the environment its nodes
    ran with (harness.sh's NODE_ENV, "" when none)."""

    def __init__(self, raw, params, node_env=""):
        self.raw = Path(raw)
        self.p = params
        self.node_env = node_env

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


def outcome(status, metrics, notes, reached=False):
    return {"status": status, "metrics": metrics, "notes": notes, "reached": reached}


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
    """Whether the harness got no answer it can use: anything but 200 and the
    backend's own 500."""
    return search["http"] not in (200, 500)


def backend_errors(searches):
    return sum(1 for s in searches if s["http"] == 500)


def complete(ctx, search):
    return ctx.expected(search["capability"]) <= valid_ids(search)


def coverage(ctx, searches):
    expected = sum(len(ctx.expected(s["capability"])) for s in searches)
    found = sum(len(ctx.expected(s["capability"]) & valid_ids(s)) for s in searches)
    return found / expected if expected else None


def backend_pct(answers):
    """Of the answers with a valid candidate, the percentage whose first one
    the backend timed (first_valid_source "backend") rather than the end of
    the search ("completion"); None when none has a valid candidate."""
    with_valid = [a for a in answers if a.get("first_valid_ms") is not None]
    if not with_valid:
        return None
    return rnd(100 * sum(1 for a in with_valid if a.get("first_valid_source") == "backend") / len(with_valid))


def harness_failures(searches):
    return [f"{s['node']}: {s['error'] or 'HTTP ' + str(s['http'])}" for s in searches if failed(s)]


def rounds(searches):
    """The searches grouped by tag, in the order the tags first appear."""
    groups = {}
    for s in searches:
        groups.setdefault(s["tag"], []).append(s)
    return list(groups.values())


def records(stats, agent, fields):
    entry = stats.get("stored_by_agent", {}).get(agent, {})
    return sum(entry.get(f, 0) for f in fields)


def stored(stats_line, agent, nodes, fields=ATTRIBUTED + ("provider_records",)):
    """What `nodes` store about `agent` in a stats line; None if one did not answer."""
    total = 0
    for node in nodes:
        if node not in stats_line["nodes"]:
            return None
        total += records(stats_line["nodes"][node], agent, fields)
    return total


def s0(ctx, lines):
    searches = of(lines, "search")
    errors = harness_failures(searches)
    found = sum(len(cands(s)) for s in searches)
    end = of(lines, "stats")
    node_stats = end[-1]["nodes"].get(ctx.p["S0_NODE"], {}) if end else {}
    metrics = {
        "searches": len(searches),
        "candidates": found,
        "backend_errors": backend_errors(searches),
        "routing_peers": node_stats.get("routing_peers"),
        "connections": node_stats.get("connections"),
    }
    if errors or not searches:
        return outcome("error", metrics, "Sin respuesta útil en búsquedas de S0: " + "; ".join(errors))
    notes = (f"{ctx.p['S0_NODE']}, sin lista de arranque, buscó cada capacidad tras "
             f"{ctx.p['S0_GRACE_S']} s en la red A con los proveedores anunciados: ")
    if found:
        return outcome("fail", metrics, notes + f"encontró {found} candidatos.")
    n = metrics["backend_errors"]
    extra = f" ({n} búsquedas terminaron en error del backend)" if n else ""
    return outcome("pass", metrics, notes + "no encontró ningún candidato" + extra + ".")


def s1(ctx, lines, by_design=None):
    searches = of(lines, "search")
    errors = harness_failures(searches)
    answered = [s for s in searches if s["answer"] is not None]
    with_valid = [s for s in answered if s["answer"].get("first_valid_ms") is not None]
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
        "first_valid_ms_p50": pct([s["answer"]["first_valid_ms"] for s in with_valid], 0.5),
        "first_valid_ms_p95": pct([s["answer"]["first_valid_ms"] for s in with_valid], 0.95),
        "first_valid_backend_pct": backend_pct([s["answer"] for s in answered]),
        "elapsed_ms_p50": pct([s["answer"]["elapsed_ms"] for s in answered], 0.5),
        "elapsed_ms_p95": pct([s["answer"]["elapsed_ms"] for s in answered], 0.95),
        "requests_p50": pct([s["answer"].get("requests") for s in answered], 0.5),
        "requests_p95": pct([s["answer"].get("requests") for s in answered], 0.95),
        "candidates_p50": pct([len(cands(s)) for s in answered], 0.5),
        "candidates_max": max((len(cands(s)) for s in answered), default=None),
        "bytes_raw_p50": pct([s["bytes"] for s in searches], 0.5),
        "bytes_p50": rnd(pct(net_bytes, 0.5), 0),
        "bytes_p95": rnd(pct(net_bytes, 0.95), 0),
        "backend_errors": backend_errors(searches),
    }
    if errors or not searches:
        return outcome("error", metrics, "Sin respuesta útil en búsquedas de S1: " + "; ".join(errors[:5]))
    clients, caps = len({s["node"] for s in searches}), len({s["capability"] for s in searches})
    notes = (f"{n} búsquedas: {clients} clientes, {caps} capacidades, {ctx.p['SEARCHES']} veces cada una. "
             "first_valid_ms es el found_ms más temprano de un candidato válido, si el backend lo da "
             "(first_valid_backend_pct), o el final de la búsqueda si no. "
             "Bytes por búsqueda: los de la interfaz del cliente menos su tráfico en reposo de S6 en el mismo tiempo.")
    if not idle:
        notes += " Sin S6, los bytes netos faltan."
    if by_design is not None:
        if any(valid_ids(s) for s in searches):
            return outcome("error", metrics, "Declarado by-design, pero hubo candidatos válidos: " + notes)
        return outcome("by-design", metrics, by_design["notes"] + " " + notes)
    if all(complete(ctx, s) for s in searches):
        return outcome("pass", metrics, "Cada búsqueda devolvió los dos proveedores, verificados. " + notes)
    return outcome("fail", metrics,
                   f"Solo el {metrics['complete_pct']} % de las búsquedas devolvió los dos proveedores. " + notes)


def s2(ctx, lines):
    stop, start = event(lines, "stop"), event(lines, "start")
    connected = of(lines, "search", "connected")
    newcomer = of(lines, "search", "newcomer")
    errors = harness_failures(connected + newcomer)
    first = next((s for s in newcomer if valid_ids(s)), None)
    caps = [ctx.cap(c) for c in ctx.words("CAPABILITIES")]
    newcomer_complete = all(any(complete(ctx, s) for s in newcomer if s["capability"] == c) for c in caps)
    connected_answers = [s["answer"] for s in connected if s["answer"]]
    metrics = {
        "connected_complete_pct": rnd(100 * sum(1 for s in connected if complete(ctx, s)) / len(connected))
        if connected else None,
        "connected_first_valid_ms_p50": pct([a["first_valid_ms"] for a in connected_answers], 0.5),
        "connected_first_valid_ms_p95": pct([a["first_valid_ms"] for a in connected_answers], 0.95),
        "connected_first_valid_backend_pct": backend_pct(connected_answers),
        "connected_elapsed_ms_p50": pct([a["elapsed_ms"] for a in connected_answers], 0.5),
        "connected_elapsed_ms_p95": pct([a["elapsed_ms"] for a in connected_answers], 0.95),
        "newcomer_first_valid_s": rnd(first["t1"] - start["t"]) if first is not None and start else None,
        "newcomer_complete": newcomer_complete,
        "newcomer_searches": len(newcomer),
    }
    reached = bool(newcomer) and newcomer_complete
    if errors or stop is None or start is None or not connected:
        return outcome("error", metrics, "S2 incompleto: " + "; ".join(errors[:5]), reached)
    node = ctx.p["S2_NODE"]
    notes = (f"Parado {ctx.p['BOOTSTRAP']}, cada cliente buscó cada capacidad una vez; después arrancó {node} "
             f"con [{ctx.p['BOOTSTRAP']}, {ctx.p['SAVED_CONTACT']}] y buscó hasta encontrar a los dos proveedores "
             f"de cada capacidad, {2 * ctx.p['REPUBLISH_S']} s como mucho. "
             f"newcomer_first_valid_s cuenta desde que arrancó su contenedor. connected_first_valid_ms y "
             f"connected_elapsed_ms son de las búsquedas de los clientes con {ctx.p['BOOTSTRAP']} parado: cuándo "
             "tuvo el backend el primer candidato válido y cuándo terminó la búsqueda. "
             "connected_first_valid_backend_pct es la parte de las que tienen candidato válido cuyo primero "
             "fechó el backend (found_ms), y no el final de la búsqueda.")
    ok = all(complete(ctx, s) for s in connected) and newcomer_complete
    return outcome("pass" if ok else "fail", metrics, notes, reached)


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

    # Routers: forgotten at the first poll at 0 after the last poll that
    # still had something, and only if the latest poll is at 0.
    known = [(p["t"], stored(p, agent, ctx.routers)) for p in polls]
    known = [(t, n) for t, n in known if n is not None]
    routers_gone = None
    latest = stored(polls[-1], agent, ctx.routers) if polls else None
    if latest == 0:
        last_held = max((t for t, n in known if n > 0), default=None)
        routers_gone = min(t for t, n in known if n == 0 and (last_held is None or t > last_held))

    # Searches: forgotten at the first answered search after the last one
    # that returned it, valid or not, and only if the latest round is clean:
    # every search answered with 200 and none returning it.
    groups = rounds(searches)
    last_clean = bool(groups) and all(s["http"] == 200 and agent not in seen_ids(s) for s in groups[-1])
    search_gone = None
    if last_clean:
        last_returned = max((s["t1"] for s in searches if agent in seen_ids(s)), default=None)
        after = [s["t1"] for s in searches if s["http"] == 200 and (last_returned is None or s["t1"] > last_returned)]
        search_gone = min(after) if after else None

    t = stop["t"] if stop else None
    metrics = {
        "stored_before": pre,
        "routers_forget_s": rnd(routers_gone - t) if routers_gone is not None and t is not None else None,
        "searches_forget_s": rnd(search_gone - t) if search_gone is not None and t is not None else None,
        "limit_s": limit,
        "polls": len(polls),
    }
    reached = routers_gone is not None and search_gone is not None
    if errors or stop is None or pre is None:
        return outcome("error", metrics, "S3 incompleto: " + "; ".join(errors[:5]), reached)
    notes = (f"Parado {victim} sin aviso (docker kill). Cada {ctx.p['POLL_S']} s, las estadísticas de los routers "
             f"(lo que guardan de {victim}) y una búsqueda por cliente de una capacidad de {victim}, hasta que los "
             f"routers no guardan nada de {victim} y una ronda entera contesta 200 sin devolverlo. routers_forget_s "
             "cuenta en el primer sondeo a 0 después del último que guardaba algo, y searches_forget_s en la primera "
             "búsqueda contestada después de la última que lo devolvió.")
    if pre == 0:
        return outcome("fail", metrics, f"Los routers no guardaban nada de {victim} antes de pararlo: "
                       "la caducidad en los routers no se puede comprobar. " + notes, reached)
    ok = reached and metrics["routers_forget_s"] < limit and metrics["searches_forget_s"] < limit
    return outcome("pass" if ok else "fail", metrics, notes, reached)


def s4(ctx, lines):
    stop = event(lines, "stop")
    searches = of(lines, "search")
    errors = harness_failures(searches)
    groups = rounds(searches)
    limit = 2 * ctx.p["REPUBLISH_S"]
    reached = bool(groups) and coverage(ctx, groups[-1]) == 1
    t = stop["t"] if stop else None
    # Recovered once a whole round is complete; the time is that of the
    # first complete search, of the same client and capability, after the
    # last incomplete one: 0 when no search was ever incomplete.
    full = None
    if reached and t is not None:
        short = [s for s in searches if not complete(ctx, s)]
        if not short:
            full = t
        else:
            last = max(short, key=lambda s: s["t1"])
            full = min(s["t1"] for s in searches if s["t1"] > last["t1"] and complete(ctx, s)
                       and (s["node"], s["capability"]) == (last["node"], last["capability"]))
    # Every search of S4 runs with the stopped routers down: the harness
    # starts them again only after its loop.
    answers = [s["answer"] for s in searches if s["answer"]]
    metrics = {
        "immediate_coverage": rnd(coverage(ctx, groups[0]), 3) if groups else None,
        "full_coverage_s": rnd(full - t) if full is not None and t is not None else None,
        "degraded_first_valid_ms_p50": pct([a["first_valid_ms"] for a in answers], 0.5),
        "degraded_first_valid_ms_p95": pct([a["first_valid_ms"] for a in answers], 0.95),
        "degraded_first_valid_backend_pct": backend_pct(answers),
        "degraded_elapsed_ms_p50": pct([a["elapsed_ms"] for a in answers], 0.5),
        "degraded_elapsed_ms_p95": pct([a["elapsed_ms"] for a in answers], 0.95),
        "rounds": len(groups),
        "limit_s": limit,
    }
    if errors or stop is None or not groups:
        return outcome("error", metrics, "S4 incompleto: " + "; ".join(errors[:5]), reached)
    notes = (f"Parados {ctx.p['S4_NODES']}. Rondas seguidas en que cada cliente busca cada capacidad, hasta "
             "una ronda completa; la cobertura es la fracción de los proveedores esperados que se encuentran. "
             "La cobertura completa cuenta en la primera búsqueda completa, del mismo cliente y capacidad, "
             "después de la última incompleta, y es 0 si ninguna lo fue. degraded_first_valid_ms y "
             "degraded_elapsed_ms son de todas las búsquedas de S4, hechas con los routers parados: cuándo tuvo "
             "el backend el primer candidato válido y cuándo terminó la búsqueda. "
             "degraded_first_valid_backend_pct es la parte de las que tienen candidato válido cuyo primero "
             "fechó el backend (found_ms), y no el final de la búsqueda.")
    ok = metrics["full_coverage_s"] is not None and metrics["full_coverage_s"] < limit
    return outcome("pass" if ok else "fail", metrics, notes, reached)


def s5(ctx, lines):
    rogue = ctx.agent(ctx.p["S5_NODE"])
    before = of(lines, "stats", "before")
    after = of(lines, "stats", "poll") + of(lines, "stats", "after")
    searches = of(lines, "search")
    errors = harness_failures(searches)
    routers = ctx.routers
    base = before[0]["nodes"] if before else {}
    honest = [ctx.agent(p) for p in ctx.providers]

    def attributed(line):
        """Every router answered, and every honest provider is attributed
        to some router: stored_by_agent is filled, so it can show what is
        not there."""
        return all(r in line["nodes"] for r in routers) and all(
            any(records(line["nodes"][r], a, ATTRIBUTED + ("provider_records",)) for r in routers)
            for a in honest)

    def unexplained(line, r):
        """Growth of records_stored at r since `before` that stored_by_agent
        does not explain: the other agents' records, and the rogue's other
        than a contact. Counted as the rogue's contact."""
        b, a = base[r], line["nodes"][r]
        agents = (set(b.get("stored_by_agent", {})) | set(a.get("stored_by_agent", {}))) - {rogue}
        explained = sum(records(a, x, ATTRIBUTED) - records(b, x, ATTRIBUTED) for x in agents)
        own = ("announce_records", "other_records")
        explained += records(a, rogue, own) - records(b, rogue, own)
        return (a["records_stored"] - b["records_stored"]) - explained

    def rejecting(line):
        return [r for r in routers if line["nodes"][r]["records_rejected"] > base[r]["records_rejected"]]

    checked = before[:1] + after
    evidence = bool(before) and bool(after) and all(attributed(l) for l in checked)
    rejected = max((len(rejecting(l)) for l in after), default=0) if evidence else 0
    delta = max((sum(l["nodes"][r]["records_rejected"] - base[r]["records_rejected"] for r in routers)
                 for l in after), default=0) if evidence else 0
    storing = max((sum(1 for r in routers if records(l["nodes"][r], rogue, ("contact_records",)))
                   for l in after), default=0) if evidence else None
    growth = max((unexplained(l, r) for l in after for r in routers), default=0) if evidence else None
    valid = sum(1 for s in searches if rogue in valid_ids(s))
    seen = [c.get("reason") for s in searches for c in cands(s) if c.get("agent_id") == rogue]
    metrics = {
        "routers_rejecting": rejected,
        "records_rejected_delta": delta,
        "routers_storing_rogue_contact": storing,
        "records_stored_unexplained_max": growth,
        "clients_rogue_valid": valid,
        "clients_rogue_returned": len(seen),
        "searches": len(searches),
    }
    reached = evidence and rejected > 0
    if errors or not before or rogue is None or not searches:
        return outcome("error", metrics, "S5 incompleto: " + "; ".join(errors[:5]), reached)
    if not evidence:
        return outcome("error", metrics, "Las estadísticas de los routers no atribuyen registros a todos los "
                       "proveedores honrados (stored_by_agent vacío o incompleto, o un router sin contestar): "
                       f"no prueban que ninguno guarde el contacto de {ctx.p['S5_NODE']}.", reached)
    reasons = ", ".join(sorted(set(r or "válido" for r in seen))) or "ninguno"
    notes = (f"El nodo {ctx.p['S5_NODE']} anunció {ctx.p['CAPS_rogue']} con un contacto de firma alterada en un byte. "
             "Ningún router debe atribuirle un contacto, y records_stored no puede crecer más de lo que explica "
             "stored_by_agent (los registros de los demás agentes y los de rogue que no son un contacto): lo que "
             f"sobra cuenta como su contacto. Motivos con que lo devolvieron los clientes: {reasons}.")
    ok = rejected > 0 and storing == 0 and growth <= 0 and valid == 0
    return outcome("pass" if ok else "fail", metrics, notes, reached)


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
    times = [v - t for v in resolved.values()] if t is not None else []
    clients = ctx.clients
    metrics = {
        "clients_resolved": len(resolved),
        "resolved_s_p50": rnd(pct(times, 0.5)),
        "resolved_s_max": rnd(max(times)) if len(resolved) == len(clients) and times else None,
        "limit_s": limit,
        "searches": len(searches),
    }
    reached = len(resolved) == len(clients)
    announce_failed = not announce or announce[0].get("http") not in (200, 500)
    if errors or move is None or not ip or announce_failed:
        return outcome("error", metrics, "S7 incompleto: " + "; ".join(errors[:5]), reached)
    notes = (f"{mover} salió de la red B y entró en la A, con la IP {ip}, y anunció de nuevo sus registros "
             "(POST /announce). Cada cliente busca una capacidad suya por ronda; se resuelve cuando un candidato "
             "válido de " + mover + " lleva la IP nueva. El tiempo cuenta desde la desconexión.")
    if announce[0].get("http") != 200:
        return outcome("fail", metrics, "El nuevo anuncio falló: " + str(announce[0].get("answer")) + ". " + notes,
                       reached)
    ok = reached and metrics["resolved_s_max"] < limit
    return outcome("pass" if ok else "fail", metrics, notes, reached)


EVALUATE = {"S0": s0, "S1": s1, "S2": s2, "S3": s3, "S4": s4, "S5": s5, "S6": s6, "S7": s7}


def reached(ctx, scenario):
    """Whether what the harness's loop for `scenario` waits for happened."""
    return EVALUATE[scenario](ctx, ctx.lines(scenario))["reached"]


def evaluate(ctx, scenario):
    """The scenario's {status, metrics, notes}."""
    declared = ctx.declared(scenario)
    if declared and declared["status"] == "not-applicable":
        return {"status": "not-applicable", "metrics": {}, "notes": declared["notes"]}
    lines = ctx.lines(scenario)
    # Left out of a run by harness.sh --only, which says so in an event.
    left_out = event(lines, "not-run")
    if left_out is not None:
        return {"status": "not-applicable", "metrics": {},
                "notes": f"No ejecutado en esta ejecución: el arnés solo ejecutó {left_out.get('only')} (--only)."}
    if not lines:
        return {"status": "error", "metrics": {}, "notes": "El arnés no ejecutó el escenario."}
    if scenario == "S1" and declared and declared["status"] == "by-design":
        result = s1(ctx, lines, by_design=declared)
    else:
        result = EVALUATE[scenario](ctx, lines)
    del result["reached"]
    unsettled = [l.get("tag", "?") for l in lines if l["kind"] == "event" and l["name"] == "settle-timeout"]
    if unsettled:
        result["notes"] = (f"La topología no se asentó en {ctx.p['SETTLE_MAX_S']} s antes del escenario "
                           f"({', '.join(unsettled)}): sus cifras pueden ser de una topología a medio converger. "
                           + result["notes"])
    errors = [l["msg"] for l in lines if l["kind"] == "error"]
    if errors:
        result["status"] = "error"
        result["notes"] = "Error del arnés: " + "; ".join(errors) + ". " + result["notes"]
    if ctx.node_env:
        result["notes"] = f"Los nodos corrieron con {ctx.node_env} (NODE_ENV del arnés). " + result["notes"]
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
