#!/usr/bin/env python3
"""The JSON side of harness.sh: observations, waits and results.

harness.sh runs the containers and the order of things; everything that
reads or writes JSON is here and in evaluate.py, so that the shell never
parses it:

- observations: a search, the stats of some nodes, the interface counters
  or an announce, each through `docker exec <container> curl` on the node's
  control API (127.0.0.1:7600), appended as one JSON line to
  <raw>/<scenario>.jsonl, with the times of the container's own clock;
- `reached <scenario>` and `settled <tag>`: whether what a polling loop of
  harness.sh waits for has happened, from the lines so far (exit 0 or 1);
- `results`: every scenario's status, metrics and notes (evaluate.py),
  written as the results JSON of the candidate.
"""

import argparse
import ipaddress
import json
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evaluate import SCENARIOS, Ctx, evaluate, failed, read_params, reached, settled  # noqa: E402

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


def overlaps(subnets, taken):
    """Exit 1, printing each clash, if `subnets` overlap each other or a
    subnet of `taken` ("<network> <subnet>…" lines, as
    `docker network inspect` gives them); 0 if they are free."""
    try:
        ours = [ipaddress.ip_network(s) for s in subnets]
    except ValueError as error:
        print(f"not a subnet: {error}")
        return 1
    clashes = [f"{a} and {b}" for i, a in enumerate(ours) for b in ours[i + 1:] if a.overlaps(b)]
    for line in taken:
        name, *theirs = line.split() or [""]
        for subnet in theirs:
            try:
                other = ipaddress.ip_network(subnet, strict=False)
            except ValueError:
                continue
            clashes += [f"{o} and {name}'s {other}" for o in ours if o.version == other.version and o.overlaps(other)]
    for clash in clashes:
        print(clash)
    return 1 if clashes else 0


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
    p = sub.add_parser("overlaps", help="exit 1, naming them, if Docker's subnets on stdin overlap these")
    p.add_argument("subnets", nargs="+")
    p = sub.add_parser("results", help="writes the results JSON")
    p.add_argument("--backend", required=True), p.add_argument("--commit", required=True)
    p.add_argument("--date", required=True), p.add_argument("--out", required=True)
    args = parser.parse_args()
    ctx = Ctx(args.raw, read_params(args.params))
    ctx.prefix = args.prefix

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
        return 0 if reached(ctx, args.scenario) else 1
    if args.command == "settled":
        return 0 if settled(ctx, args.tag) else 1
    if args.command == "overlaps":
        return overlaps(args.subnets, sys.stdin)
    if args.command == "results":
        scenarios = {s: evaluate(ctx, s) for s in SCENARIOS}
        results = {"backend": args.backend, "commit": args.commit, "date": args.date,
                   "params": ctx.p, "scenarios": scenarios}
        Path(args.out).write_text(json.dumps(results, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        for s in SCENARIOS:
            print(f"{s}: {scenarios[s]['status']}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
