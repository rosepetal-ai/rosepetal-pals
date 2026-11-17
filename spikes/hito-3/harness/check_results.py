#!/usr/bin/env python3
"""Checks results JSONs of the hito 3 backend comparison.

    check_results.py [--repeated] <results.json>...

Each file must follow the schema harness.sh writes:

    {"backend", "commit", "date", "params": {...params.env...},
     "scenarios": {"S0".."S7": {"status", "metrics": {...}, "notes"}}}

with a status of pass, fail, by-design, not-applicable, error or measured,
metrics whose values are numbers, booleans or null, and no host path. All
the files must have the same `params`, and no two the same backend: results
measured with another topology, other times or other limits are not
comparable. With --repeated, the files may repeat a backend: repeated runs
of the same candidates (task 7 of phase 2 of the hito 3 plan, repeat.py),
each checked as any other. Exits 1 if any file breaks this or has a
scenario in `error`, and 0 otherwise.
"""

import json
import re
import sys

KEYS = {"backend", "commit", "date", "params", "scenarios"}
SCENARIOS = [f"S{i}" for i in range(8)]
STATUSES = {"pass", "fail", "by-design", "not-applicable", "error", "measured"}
# The plan's values, which every run must carry as integers.
PLAN_PARAMS = ("TTL_S", "REPUBLISH_S", "IDLE_S", "SEARCHES", "SEARCH_LIMIT")
HOST_PATHS = re.compile(r"/(root|home|workspace|tmp|Users)/")


def schema_problems(doc):
    if not isinstance(doc, dict) or set(doc) != KEYS:
        return [f"top-level keys must be exactly {sorted(KEYS)}"]
    problems = []
    if not isinstance(doc["backend"], str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", doc["backend"]):
        problems.append("backend must be a candidate id")
    if not isinstance(doc["commit"], str) or not re.fullmatch(r"[0-9a-f]{7,40}(-dirty)?", doc["commit"]):
        problems.append("commit must be a short commit hash, maybe with -dirty")
    if not isinstance(doc["date"], str) or not re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", doc["date"]):
        problems.append("date must be UTC, YYYY-MM-DDTHH:MM:SSZ")
    params = doc["params"]
    if not isinstance(params, dict) or not params:
        problems.append("params must be a non-empty object")
    else:
        for key, value in params.items():
            if isinstance(value, bool) or not isinstance(value, (int, str)):
                problems.append(f"params.{key} must be an integer or a string")
        for key in PLAN_PARAMS:
            if not isinstance(params.get(key), int) or isinstance(params.get(key), bool):
                problems.append(f"params.{key} is missing or not an integer")
    scenarios = doc["scenarios"]
    if not isinstance(scenarios, dict) or sorted(scenarios) != SCENARIOS:
        return problems + [f"scenarios must be exactly {SCENARIOS}"]
    for name in SCENARIOS:
        s = scenarios[name]
        if not isinstance(s, dict) or set(s) != {"status", "metrics", "notes"}:
            problems.append(f"{name} must have exactly status, metrics and notes")
            continue
        if s["status"] not in STATUSES:
            problems.append(f"{name}.status {s['status']!r} is not one of {sorted(STATUSES)}")
        if not isinstance(s["notes"], str):
            problems.append(f"{name}.notes must be a string")
        if not isinstance(s["metrics"], dict):
            problems.append(f"{name}.metrics must be an object")
            continue
        for key, value in s["metrics"].items():
            if value is not None and not isinstance(value, (int, float, bool)):
                problems.append(f"{name}.metrics.{key} must be a number, a boolean or null")
    return problems


def main(paths):
    repeated = bool(paths) and paths[0] == "--repeated"
    if repeated:
        paths = paths[1:]
    if not paths:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    failed = False
    docs = {}
    for path in paths:
        try:
            with open(path, encoding="utf-8") as f:
                text = f.read()
            doc = json.loads(text)
        except (OSError, json.JSONDecodeError) as error:
            print(f"{path}: cannot read: {error}")
            failed = True
            continue
        problems = schema_problems(doc)
        if HOST_PATHS.search(text):
            problems.append("it names a host path; results keep relative paths")
        if problems:
            failed = True
            for problem in problems:
                print(f"{path}: {problem}")
            continue
        docs[path] = doc

    backends = {}
    for path, doc in docs.items():
        if doc["backend"] in backends and not repeated:
            print(f"{path}: backend {doc['backend']} again, as in {backends[doc['backend']]}")
            failed = True
        backends.setdefault(doc["backend"], path)

    if docs:
        first, reference = next(iter(docs.items()))
        for path, doc in docs.items():
            params = doc["params"]
            differ = sorted(k for k in set(params) | set(reference["params"])
                            if params.get(k) != reference["params"].get(k))
            if differ:
                failed = True
                shown = ", ".join(f"{k}: {reference['params'].get(k)!r} vs {params.get(k)!r}" for k in differ)
                print(f"{path}: params differ from {first}'s: {shown}")

    for path, doc in docs.items():
        statuses = {name: s["status"] for name, s in doc["scenarios"].items()}
        errors = [name for name, status in statuses.items() if status == "error"]
        summary = ", ".join(f"{name} {status}" for name, status in statuses.items())
        print(f"{path}: {doc['backend']}: {summary}")
        for name in errors:
            failed = True
            print(f"{path}: {name} is error: {doc['scenarios'][name]['notes']}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
