"""Tests of evaluate.py on synthetic observations.

    cd spikes/hito-3/harness && python3 -m unittest -v

Each test writes the healthz files of the topology and a scenario's JSONL
into a temporary directory, as scenarios.py would, with the parameters of
the real params.env, and checks the scenario's judgement.
"""

import json
import tempfile
import unittest
from pathlib import Path

import evaluate
from evaluate import Ctx, read_params

PARAMS = read_params(Path(__file__).resolve().parent / "params.env")
NODES = (PARAMS["ROUTERS"].split() + PARAMS["PROVIDERS_A"].split() + PARAMS["PROVIDERS_B"].split()
         + PARAMS["CLIENTS_A"].split() + PARAMS["CLIENTS_B"].split()
         + [PARAMS["S0_NODE"], PARAMS["S2_NODE"], PARAMS["S5_NODE"]])
AGENT = {node: f"{i + 1:064x}" for i, node in enumerate(NODES)}
ROUTERS = PARAMS["ROUTERS"].split()
PROVIDERS = PARAMS["PROVIDERS_A"].split() + PARAMS["PROVIDERS_B"].split()
CLIENTS = PARAMS["CLIENTS_A"].split() + PARAMS["CLIENTS_B"].split()
CAPS = PARAMS["CAPABILITIES"].split()
PREFIX = PARAMS["CAPABILITY_PREFIX"]
BASELINE_S1 = [{"scenario": "S1", "status": "by-design", "notes": "por diseño"}]
# The tests give times in seconds from here, as the container's clock would.
BASE = 1_800_000_000


def providers_of(cap):
    return [p for p in PROVIDERS if cap in PARAMS[f"CAPS_{p}"].split()]


def search(node, cap, tag, t1, found=(), invalid=(), http=200, first_valid_ms=None, source=None, addrs=None):
    """A search line: `found` providers valid, `invalid` ones with a bad contact."""
    candidates = [{"agent_id": AGENT[p], "valid": True, "reason": None,
                   "direct_addrs": (addrs or {}).get(p, ["10.0.0.1:4433"]), "contact_seq": 1} for p in found]
    candidates += [{"agent_id": AGENT[p], "valid": False, "reason": "bad_contact", "direct_addrs": [],
                    "contact_seq": None} for p in invalid]
    answer = None
    if http == 200:
        answer = {"elapsed_ms": 30, "first_valid_ms": first_valid_ms if found else None,
                  "first_valid_source": source if found else None, "candidates": candidates, "requests": 2}
    return {"kind": "search", "tag": tag, "node": node, "capability": PREFIX + cap,
            "t0": BASE + t1 - 0.5, "t1": BASE + t1,
            "bytes": 1000, "http": http, "answer": answer, "error": None if http in (200, 500) else f"HTTP {http}"}


def node_stats(stored=None, records_stored=None, rejected=0):
    """One node's /stats: `stored` maps a node name to (contact, announce, provider) records."""
    by_agent = {AGENT[n]: {"contact_records": c, "announce_records": a, "other_records": 0, "provider_records": p}
                for n, (c, a, p) in (stored or {}).items()}
    total = records_stored if records_stored is not None else sum(c + a for c, a, _ in (stored or {}).values())
    return {"connections": 0, "routing_peers": 3, "records_stored": total,
            "provider_records_stored": sum(p for _, _, p in (stored or {}).values()),
            "records_rejected": rejected, "bootstrap_addr": "x", "stored_by_agent": by_agent, "rss_kib": 9000}


def stats(tag, t, per_router):
    return {"kind": "stats", "tag": tag, "t": BASE + t, "nodes": per_router, "errors": {}}


def event(name, t, **fields):
    return {"kind": "event", "name": name, "t": BASE + t, **fields}


class Case(unittest.TestCase):
    declared = []

    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        raw = Path(self.dir.name)
        for node in NODES:
            health = {"name": node, "agent_id": AGENT[node], "declared": self.declared}
            (raw / f"healthz-{node}.json").write_text(json.dumps(health))
        self.ctx = Ctx(raw, PARAMS)

    def write(self, scenario, lines):
        for line in lines:
            self.ctx.append(scenario, line)

    def judge(self, scenario, lines):
        self.write(scenario, lines)
        return evaluate.evaluate(self.ctx, scenario)


def everything_stored(extra=None):
    """Every router storing every honest provider's three kinds of record."""
    stored = {p: (1, 1, len(PARAMS[f"CAPS_{p}"].split())) for p in PROVIDERS}
    stored.update(extra or {})
    return stored


class S0(Case):
    def test_harness_rejections_do_not_pass(self):
        lines = [event("begin", 0)] + [search("c0", c, "grace", 1 + i, http=400) for i, c in enumerate(CAPS)]
        self.assertEqual(self.judge("S0", lines)["status"], "error")

    def test_backend_errors_count_as_no_candidates(self):
        lines = [event("begin", 0)] + [search("c0", c, "grace", 1 + i, http=500) for i, c in enumerate(CAPS)]
        result = self.judge("S0", lines)
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["metrics"]["backend_errors"], len(CAPS))


class S1(Case):
    def test_first_valid_comes_from_found_ms(self):
        lines = [event("begin", 0)]
        for r in range(2):
            for c in CLIENTS:
                for cap in CAPS:
                    lines.append(search(c, cap, f"round={r}", 10 + r, found=providers_of(cap),
                                        first_valid_ms=7, source="backend"))
        result = self.judge("S1", lines)
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["metrics"]["first_valid_ms_p50"], 7)
        self.assertEqual(result["metrics"]["first_valid_backend_pct"], 100)

    def test_a_completion_time_is_told_apart(self):
        lines = [event("begin", 0), search("c1", CAPS[0], "round=1", 5, found=providers_of(CAPS[0]),
                                           first_valid_ms=30, source="completion")]
        self.assertEqual(self.judge("S1", lines)["metrics"]["first_valid_backend_pct"], 0)


class ByDesignS1(Case):
    declared = BASELINE_S1

    def test_by_design_finds_no_one(self):
        lines = [event("begin", 0), search("c1", CAPS[0], "round=1", 5)]
        self.assertEqual(self.judge("S1", lines)["status"], "by-design")

    def test_by_design_with_a_valid_candidate_is_an_error(self):
        lines = [event("begin", 0), search("c1", CAPS[0], "round=1", 5, found=providers_of(CAPS[0]))]
        self.assertEqual(self.judge("S1", lines)["status"], "error")


class S3(Case):
    victim = PARAMS["S3_NODE"]

    def held(self, t, n):
        record = {self.victim: (n, n, n)} if n else {}
        return stats("poll", t, {r: node_stats(record) for r in ROUTERS})

    def round(self, i, t, returned=False, http=200):
        cap = PARAMS[f"CAPS_{self.victim}"].split()[0]
        found = [self.victim] if returned else []
        return [search(c, cap, f"poll={i}", t + k, found=found, http=http) for k, c in enumerate(CLIENTS)]

    def start(self):
        return [event("begin", -1), stats("before", -1, {r: node_stats({self.victim: (1, 1, 2)}) for r in ROUTERS}),
                event("stop", 0)]

    def test_a_late_re_return_counts(self):
        lines = self.start()
        lines += [self.held(10, 1)] + self.round(0, 11, returned=True)
        lines += [self.held(50, 1)] + self.round(1, 51)  # clean, but early
        lines += [self.held(100, 0)] + self.round(2, 101, returned=True)  # returned again
        lines += [self.held(140, 0)] + self.round(3, 141)
        result = self.judge("S3", lines)
        self.assertEqual(result["metrics"]["routers_forget_s"], 100)
        self.assertEqual(result["metrics"]["searches_forget_s"], 141)
        self.assertEqual(result["status"], "pass")

    def test_a_round_of_500s_is_not_clean(self):
        lines = self.start()
        lines += [self.held(10, 1)] + self.round(0, 11, returned=True)
        lines += [self.held(12, 1)] + self.round(1, 12.1, http=500)
        lines += [self.held(125, 1)] + self.round(2, 126, returned=True)
        lines += [self.held(132, 0)] + self.round(3, 135)
        result = self.judge("S3", lines)
        self.assertEqual(result["metrics"]["searches_forget_s"], 135)
        self.assertEqual(result["metrics"]["routers_forget_s"], 132)

    def test_the_loop_waits_for_a_clean_last_round_and_routers_at_zero(self):
        lines = self.start() + [self.held(10, 0)] + self.round(0, 11, http=500)
        self.write("S3", lines)
        self.assertFalse(evaluate.reached(self.ctx, "S3"))
        self.write("S3", [self.held(20, 1)] + self.round(1, 21))
        self.assertFalse(evaluate.reached(self.ctx, "S3"))
        self.write("S3", [self.held(30, 0)] + self.round(2, 31))
        self.assertTrue(evaluate.reached(self.ctx, "S3"))
        self.assertEqual(evaluate.evaluate(self.ctx, "S3")["metrics"]["routers_forget_s"], 30)

    def test_routers_that_never_stored_it_fail(self):
        lines = [event("begin", -1), stats("before", -1, {r: node_stats() for r in ROUTERS}), event("stop", 0)]
        lines += [self.held(10, 0)] + self.round(0, 11)
        self.assertEqual(self.judge("S3", lines)["status"], "fail")


class S4(Case):
    def round(self, i, t, short=None):
        lines = []
        for k, (c, cap) in enumerate((c, cap) for c in CLIENTS for cap in CAPS):
            found = providers_of(cap)[:1] if (c, cap) == short else providers_of(cap)
            lines.append(search(c, cap, f"round={i}", t + k, found=found))
        return lines

    def test_degrade_then_recover(self):
        lines = [event("begin", -1), event("stop", 0)]
        lines += self.round(0, 1, short=("c1", CAPS[0]))  # c1's first search, at t1 = 1
        lines += self.round(1, 40)  # c1's first search again, complete, at t1 = 40
        self.write("S4", lines)
        self.assertTrue(evaluate.reached(self.ctx, "S4"))
        result = evaluate.evaluate(self.ctx, "S4")
        self.assertEqual(result["metrics"]["full_coverage_s"], 40)
        self.assertEqual(result["metrics"]["immediate_coverage"], round(31 / 32, 3))
        self.assertEqual(result["status"], "pass")

    def test_never_degraded_is_zero(self):
        result = self.judge("S4", [event("begin", -1), event("stop", 0)] + self.round(0, 1))
        self.assertEqual(result["metrics"]["full_coverage_s"], 0)


class S5(Case):
    rogue = PARAMS["S5_NODE"]

    def lines(self, before, after, rejected=1):
        searches = [search(c, "vision.ocr", "clients", 60 + k, found=providers_of("vision.ocr"),
                           invalid=[self.rogue]) for k, c in enumerate(CLIENTS)]
        return ([event("begin", 0), stats("before", 1, {r: before() for r in ROUTERS}), event("start", 2)]
                + [stats("poll", 30, {r: after(rejected) for r in ROUTERS})] + searches
                + [stats("after", 70, {r: after(rejected) for r in ROUTERS})])

    def test_an_empty_stored_by_agent_is_an_error(self):
        result = self.judge("S5", self.lines(lambda: node_stats(records_stored=8),
                                             lambda n: node_stats(records_stored=8, rejected=n)))
        self.assertEqual(result["status"], "error")

    def test_rejected_and_not_stored_passes(self):
        # The rogue's valid announce is stored and attributed: it explains the growth.
        result = self.judge("S5", self.lines(
            lambda: node_stats(everything_stored()),
            lambda n: node_stats(everything_stored({self.rogue: (0, 1, 1)}), rejected=n)))
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["metrics"]["records_stored_unexplained_max"], 0)

    def test_unexplained_growth_counts_as_the_contact(self):
        stored = everything_stored()
        total = sum(c + a for c, a, _ in stored.values())
        result = self.judge("S5", self.lines(
            lambda: node_stats(stored),
            lambda n: node_stats(stored, records_stored=total + 1, rejected=n)))
        self.assertEqual(result["status"], "fail")
        self.assertEqual(result["metrics"]["records_stored_unexplained_max"], 1)

    def test_a_stored_contact_fails(self):
        result = self.judge("S5", self.lines(
            lambda: node_stats(everything_stored()),
            lambda n: node_stats(everything_stored({self.rogue: (1, 1, 1)}), rejected=n)))
        self.assertEqual(result["status"], "fail")
        self.assertEqual(result["metrics"]["routers_storing_rogue_contact"], len(ROUTERS))

    def test_no_rejection_fails(self):
        result = self.judge("S5", self.lines(lambda: node_stats(everything_stored()),
                                             lambda n: node_stats(everything_stored(), rejected=n), rejected=0))
        self.assertEqual(result["status"], "fail")


class Settle(Case):
    def polls(self, stored, n=3):
        for i in range(n):
            self.ctx.append("settle", stats("initial", i, {r: node_stats(stored) for r in ROUTERS}))

    def test_stable_counts_without_every_provider_are_not_settled(self):
        stored = everything_stored()
        del stored[PROVIDERS[-1]]
        self.polls(stored)
        self.assertFalse(evaluate.settled(self.ctx, "initial"))

    def test_stable_counts_with_every_provider_are_settled(self):
        self.polls(everything_stored())
        self.assertTrue(evaluate.settled(self.ctx, "initial"))

    def test_a_timeout_is_noted_in_the_next_scenario(self):
        lines = [event("begin", 0), event("settle-timeout", 0, tag="initial"),
                 search("c0", CAPS[0], "grace", 1)]
        self.assertIn("no se asentó", self.judge("S0", lines)["notes"])


class ByDesignSettle(Case):
    declared = BASELINE_S1

    def test_a_backend_that_stores_nothing_settles_on_stable_counts(self):
        for i in range(3):
            self.ctx.append("settle", stats("initial", i, {r: node_stats() for r in ROUTERS}))
        self.assertTrue(evaluate.settled(self.ctx, "initial"))


if __name__ == "__main__":
    unittest.main()
