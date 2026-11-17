"""Tests of repeat.py on synthetic results JSONs.

    cd spikes/hito-3/harness && python3 -m unittest -v

Each test writes runs of kad-iroh and kad-libp2p with the parameters of the
real params.env, and the two production S6 runs with the production
intervals, into a temporary directory, and checks the verdict's exit code
and what it prints.
"""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

import check_results
import repeat
from evaluate import read_params

PARAMS = read_params(Path(__file__).resolve().parent / "params.env")
PRODUCTION = dict(PARAMS, TTL_S=3600, REPUBLISH_S=1200, IDLE_S=2400)
SCENARIOS = [f"S{i}" for i in range(8)]


def run(backend, statuses=None, params=PARAMS, router_bytes=300_000, commit="448dede"):
    """A results JSON of a whole run: every scenario passes (S6 measured)
    unless `statuses` says otherwise."""
    statuses = statuses or {}
    scenarios = {}
    for s in SCENARIOS:
        status = statuses.get(s, "measured" if s == "S6" else "pass")
        scenarios[s] = {"status": status, "metrics": {}, "notes": "sintético"}
    scenarios["S1"]["metrics"] = {"first_valid_ms_p50": 2}
    scenarios["S3"]["metrics"] = {"routers_forget_s": 106.6}
    scenarios["S7"]["metrics"] = {"resolved_s_max": 1.8}
    scenarios["S6"]["metrics"] = {"router_bytes_per_min_p50": router_bytes}
    return {"backend": backend, "commit": commit, "date": "2026-11-16T15:00:00Z", "params": params,
            "scenarios": scenarios}


def production(backend, router_bytes, params=PRODUCTION, s6="measured"):
    """A production S6 run: S6 only, the rest not run."""
    doc = run(backend, {s: "not-applicable" for s in SCENARIOS}, params, router_bytes)
    doc["scenarios"]["S6"]["status"] = s6
    return doc


class Repeat(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.n = 0

    def file(self, doc, name=None):
        self.n += 1
        path = Path(self.dir.name) / f"{name or doc['backend'] + '-' + str(self.n)}.json"
        path.write_text(json.dumps(doc), encoding="utf-8")
        return str(path)

    def verdict(self, iroh_runs=None, libp2p_runs=None, iroh_prod=None, libp2p_prod=None):
        """repeat.py's exit code and output, with four runs of each backend
        (all passing unless given) and the two production runs."""
        iroh_runs = iroh_runs or [run("kad-iroh") for _ in range(4)]
        libp2p_runs = libp2p_runs or [run("kad-libp2p") for _ in range(4)]
        runs = [self.file(d) for d in iroh_runs + libp2p_runs]
        prod = [self.file(iroh_prod or production("kad-iroh", 300_000), "kad-iroh-production"),
                self.file(libp2p_prod or production("kad-libp2p", 250_000), "kad-libp2p-production")]
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = repeat.main(["--runs", *runs, "--production", *prod])
        return code, out.getvalue()

    def test_s3_fail_only_in_kad_iroh_switches(self):
        iroh = [run("kad-iroh") for _ in range(3)] + [run("kad-iroh", {"S3": "fail"})]
        code, out = self.verdict(iroh_runs=iroh)
        self.assertEqual(code, 3, out)
        self.assertIn("S3", out.split("## Veredicto")[1])

    def test_a_fail_in_both_does_not_switch(self):
        iroh = [run("kad-iroh", {"S7": "fail"})] + [run("kad-iroh") for _ in range(3)]
        libp2p = [run("kad-libp2p") for _ in range(3)] + [run("kad-libp2p", {"S7": "fail"})]
        code, out = self.verdict(iroh_runs=iroh, libp2p_runs=libp2p)
        self.assertEqual(code, 0, out)

    def test_idle_traffic_over_double_switches(self):
        code, out = self.verdict(iroh_prod=production("kad-iroh", 500_001),
                                 libp2p_prod=production("kad-libp2p", 250_000))
        self.assertEqual(code, 3, out)
        # "Pasa del doble" is strictly more than double.
        code, out = self.verdict(iroh_prod=production("kad-iroh", 500_000),
                                 libp2p_prod=production("kad-libp2p", 250_000))
        self.assertEqual(code, 0, out)

    def test_a_run_with_an_error_is_refused(self):
        iroh = [run("kad-iroh") for _ in range(3)] + [run("kad-iroh", {"S4": "error"})]
        code, out = self.verdict(iroh_runs=iroh)
        self.assertEqual(code, 1, out)
        self.assertIn("se repite", out)

    def test_production_runs_need_production_params(self):
        other = dict(PRODUCTION, REPUBLISH_S=40)
        code, out = self.verdict(iroh_prod=production("kad-iroh", 300_000, other),
                                 libp2p_prod=production("kad-libp2p", 250_000, other))
        self.assertEqual(code, 1, out)
        self.assertIn("REPUBLISH_S", out)
        code, out = self.verdict(libp2p_prod=production("kad-libp2p", 250_000, s6="not-applicable"))
        self.assertEqual(code, 1, out)
        self.assertIn("S6", out)

    def test_runs_with_other_params_are_refused(self):
        iroh = [run("kad-iroh") for _ in range(3)] + [run("kad-iroh", params=dict(PARAMS, TTL_S=60))]
        code, out = self.verdict(iroh_runs=iroh)
        self.assertEqual(code, 1, out)
        self.assertIn("TTL_S", out)

    def test_a_production_run_among_the_runs_is_refused(self):
        # It did not run S3 or S7: it cannot count for the first condition.
        iroh = [run("kad-iroh") for _ in range(3)] + [production("kad-iroh", 300_000, params=PARAMS)]
        code, out = self.verdict(iroh_runs=iroh)
        self.assertEqual(code, 1, out)

    def test_the_tables_name_no_host_path(self):
        code, out = self.verdict()
        self.assertEqual(code, 0, out)
        self.assertNotIn(self.dir.name, out)
        self.assertIn("kad-iroh-1", out)

    def test_check_results_takes_a_repeated_backend_only_with_repeated(self):
        paths = [self.file(run("kad-iroh")), self.file(run("kad-iroh"))]
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(check_results.main(paths), 1)
        self.assertIn("again", out.getvalue())
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(check_results.main(["--repeated", *paths]), 0)


if __name__ == "__main__":
    unittest.main()
