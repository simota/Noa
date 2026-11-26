"""Tests for scripts/check-widget-registration.py (docs/specs/scaffold-widget.md, I2-I4).

Run: python3 -I -B -m unittest discover -s scripts/tests -p 'test_check_widget_registration.py'

Each case under fixtures/widget-registration/ is a trimmed repository (repo/) with
the checker's exact output (expected.json, expected.txt). The Rust port in
widgets/scaffold is tested against the same expected.json, so a rule changed in
one place fails in the other.
"""

import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
CHECKER = os.path.join(os.path.dirname(HERE), "check-widget-registration.py")
FIXTURES = os.path.join(HERE, "fixtures", "widget-registration")
SCOPE = "Checked: registration consistency only. Not checked: the contents of built artifacts."

# I4: the cases and the exit code each must give.
CASES = {
    "clean": 0,
    "missing-launch": 1,
    "missing-verify": 1,
    "missing-packaging": 1,
    "os-difference-explained": 0,
    "os-difference-unexplained": 1,
    "unparseable-script": 2,
    "unparseable-registry": 2,
    "mismatches": 1,
}

# I3: every rule id.
RULES = {
    "missing-registration",
    "incomplete-registration",
    "name-mismatch",
    "repo-arg-mismatch",
    "no-intent",
    "stale-intent",
    "ship-missing",
    "ship-unintended",
    "os-difference-unexplained",
    "os-difference",
}

# What each case must report, written out here so that regenerating an
# expected.json cannot quietly change the meaning of a case: (widget, rule, target).
FINDINGS = {
    "clean": set(),
    "missing-launch": {("gamma", "missing-registration", "launch")},
    "missing-verify": {("gamma", "missing-registration", "verify")},
    "missing-packaging": {("gamma", "ship-missing", "linux"), ("gamma", "ship-missing", "macos")},
    "os-difference-explained": set(),
    "os-difference-unexplained": {("beta", "os-difference-unexplained", "linux")},
    "unparseable-script": set(),
    "unparseable-registry": set(),
    "mismatches": {
        ("beta", "ship-unintended", "linux"),
        ("delta", "name-mismatch", "crate"),
        ("delta", "name-mismatch", "linux_package"),
        ("delta", "name-mismatch", "macos_bundle"),
        ("epsilon", "repo-arg-mismatch", "launch"),
        ("kappa", "no-intent", "registry"),
        ("nu", "missing-registration", "menu"),
        ("nu", "missing-registration", "palette"),
        ("omega", "stale-intent", "registry"),
        ("theta", "incomplete-registration", "command"),
        ("theta", "incomplete-registration", "palette"),
        ("xi", "missing-registration", "command"),
        ("zeta", "incomplete-registration", "macos_bundle"),
        ("zeta", "incomplete-registration", "verify"),
    },
}
EXPLAINED = {
    "os-difference-explained": {("beta", "os-difference", "linux")},
    "mismatches": {("beta", "os-difference", "linux")},
}
UNKNOWN_SOURCE = {
    "unparseable-script": ("scripts/package-linux.sh", "anchor not found: WIDGETS=("),
    "unparseable-registry": ("dev/scaffold/registry.json", "invalid JSON"),
}

FORBIDDEN = [
    "subprocess", "os.system", "os.popen", "os.exec", "exec(", "eval(", "compile(",
    "runpy", "importlib", "__import__",
]


def run(repo, *flags):
    done = subprocess.run(
        [sys.executable, "-I", "-B", CHECKER, "--repo=" + repo, *flags],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    return done.returncode, done.stdout, done.stderr


def read(path):
    with open(path, "rb") as handle:
        return handle.read()


def triples(items):
    return {(i["widget"], i["rule"], i["target"]) for i in items}


class FixtureCases(unittest.TestCase):
    def test_the_cases_are_exactly_the_ones_of_the_spec(self):
        self.assertEqual(sorted(os.listdir(FIXTURES)), sorted(CASES))
        for case in CASES:
            self.assertEqual(
                sorted(os.listdir(os.path.join(FIXTURES, case))),
                ["expected.json", "expected.txt", "repo"],
                case,
            )

    def test_json_output_is_the_expected_bytes_and_stable(self):
        for case, exit_code in CASES.items():
            with self.subTest(case=case):
                repo = os.path.join(FIXTURES, case, "repo")
                first = run(repo, "--json")
                second = run(repo, "--json")
                self.assertEqual(first, second, "two runs differ")
                self.assertEqual(first[0], exit_code)
                self.assertEqual(first[2], b"")
                self.assertEqual(first[1], read(os.path.join(FIXTURES, case, "expected.json")))

    def test_text_output_is_the_expected_bytes_and_stable(self):
        for case, exit_code in CASES.items():
            with self.subTest(case=case):
                repo = os.path.join(FIXTURES, case, "repo")
                first = run(repo)
                second = run(repo)
                self.assertEqual(first, second, "two runs differ")
                self.assertEqual(first[0], exit_code)
                self.assertEqual(first[2], b"")
                self.assertEqual(first[1], read(os.path.join(FIXTURES, case, "expected.txt")))

    def test_scope_sentence_is_line_one_of_the_text_and_in_the_json(self):
        for case in CASES:
            with self.subTest(case=case):
                directory = os.path.join(FIXTURES, case)
                text = read(os.path.join(directory, "expected.txt")).decode("utf-8")
                self.assertEqual(text.split("\n", 1)[0], SCOPE)
                report = json.loads(read(os.path.join(directory, "expected.json")))
                self.assertEqual(report["scope"], SCOPE)

    def test_output_has_no_absolute_path_and_no_trailing_space(self):
        for case in CASES:
            with self.subTest(case=case):
                for name in ("expected.json", "expected.txt"):
                    text = read(os.path.join(FIXTURES, case, name)).decode("utf-8")
                    self.assertNotIn(FIXTURES, text)
                    self.assertNotIn(os.path.expanduser("~"), text)
                    self.assertFalse(text.startswith("/"))
                    if name == "expected.txt":
                        for line in text.split("\n"):
                            self.assertEqual(line, line.rstrip(" "), line)

    def test_status_and_exit_agree_with_the_findings_and_sources(self):
        for case, exit_code in CASES.items():
            with self.subTest(case=case):
                report = json.loads(read(os.path.join(FIXTURES, case, "expected.json")))
                self.assertEqual(report["exit"], exit_code)
                self.assertEqual(report["status"], ("consistent", "inconsistent", "unknown")[exit_code])
                unknown = [s for s in report["sources"] if s["state"] == "unknown"]
                self.assertEqual(bool(unknown), exit_code == 2)
                if exit_code == 0:
                    self.assertEqual(report["findings"], [])

    def test_each_case_reports_what_it_is_about(self):
        for case in CASES:
            with self.subTest(case=case):
                report = json.loads(read(os.path.join(FIXTURES, case, "expected.json")))
                self.assertEqual(triples(report["findings"]), FINDINGS[case])
                self.assertEqual(triples(report["explained"]), EXPLAINED.get(case, set()))
                unknown = {s["path"]: s["error"] for s in report["sources"] if s["state"] == "unknown"}
                want = UNKNOWN_SOURCE.get(case)
                self.assertEqual(unknown, {want[0]: want[1]} if want else {})

    def test_incomplete_registrations_name_their_missing_parts(self):
        report = json.loads(read(os.path.join(FIXTURES, "mismatches", "expected.json")))
        parts = {
            (f["widget"], f["target"]): f["parts"]
            for f in report["findings"]
            if f["rule"] == "incomplete-registration"
        }
        self.assertEqual(
            parts,
            {
                ("theta", "command"): ["menu-id-lookup"],
                ("theta", "palette"): ["title"],
                ("zeta", "macos_bundle"): ["sign"],
                ("zeta", "verify"): ["widget-uses"],
            },
        )
        for finding in report["findings"]:
            if finding["rule"] != "incomplete-registration":
                self.assertEqual(finding["parts"], [])

    def test_a_widget_without_a_launch_const_traces_nothing_further(self):
        report = json.loads(read(os.path.join(FIXTURES, "missing-launch", "expected.json")))
        gamma = next(w for w in report["widgets"] if w["widget"] == "gamma")
        self.assertEqual(gamma["points"]["launch"]["state"], "absent")
        for point in ("command", "menu", "palette"):
            self.assertEqual(gamma["points"][point]["state"], "unknown")
        self.assertEqual(report["exit"], 1)

    def test_every_widget_carries_the_eight_points_with_their_fact_keys(self):
        facts = {
            "crate": {"binary", "accepts_repo"},
            "launch": {"const", "bundled", "repo_arg"},
            "command": {"variant", "action"},
            "menu": set(),
            "palette": set(),
            "verify": {"check"},
            "macos_bundle": {"name"},
            "linux_package": {"name"},
        }
        for case in CASES:
            report = json.loads(read(os.path.join(FIXTURES, case, "expected.json")))
            for widget in report["widgets"]:
                with self.subTest(case=case, widget=widget["widget"]):
                    self.assertEqual(sorted(widget["points"]), sorted(facts))
                    for name, point in widget["points"].items():
                        self.assertEqual(
                            set(point), {"state", "missing", "evidence"} | facts[name], name
                        )
                        self.assertIn(point["state"], ("present", "partial", "absent", "unknown"))

    def test_every_rule_id_is_in_some_expected_json(self):
        seen = set()
        for case in CASES:
            report = json.loads(read(os.path.join(FIXTURES, case, "expected.json")))
            seen |= {f["rule"] for f in report["findings"]}
            seen |= {f["rule"] for f in report["explained"]}
        self.assertEqual(seen, RULES)

    def test_lists_are_ordered_as_the_spec_says(self):
        for case in CASES:
            with self.subTest(case=case):
                report = json.loads(read(os.path.join(FIXTURES, case, "expected.json")))
                paths = [s["path"] for s in report["sources"]]
                self.assertEqual(paths, sorted(paths))
                names = [w["widget"] for w in report["widgets"]]
                self.assertEqual(names, sorted(names))
                for key in ("findings", "explained"):
                    order = [(i["widget"], i["rule"], i["target"]) for i in report[key]]
                    self.assertEqual(order, sorted(order))
                    for item in report[key]:
                        pairs = [(e["path"], e["line"]) for e in item["evidence"]]
                        self.assertEqual(pairs, sorted(set(pairs)))


class Robustness(unittest.TestCase):
    def test_a_directory_with_none_of_the_sources_is_unknown_not_a_crash(self):
        with tempfile.TemporaryDirectory() as empty:
            code, out, err = run(empty, "--json")
        self.assertEqual(code, 2)
        self.assertEqual(err, b"")
        report = json.loads(out)
        self.assertEqual(report["status"], "unknown")
        self.assertEqual(report["widgets"], [])
        self.assertEqual(report["findings"], [])
        for source in report["sources"]:
            self.assertEqual((source["state"], source["error"]), ("unknown", "missing"))

    def test_a_source_that_is_a_symlink_is_not_followed(self):
        with tempfile.TemporaryDirectory() as scratch:
            repo = os.path.join(scratch, "repo")
            os.makedirs(os.path.join(repo, "scripts"))
            target = os.path.join(scratch, "elsewhere.sh")
            with open(target, "w", encoding="utf-8") as handle:
                handle.write("WIDGETS=(\n)\n")
            os.symlink(target, os.path.join(repo, "scripts", "package-linux.sh"))
            _, out, _ = run(repo, "--json")
        sources = {s["path"]: s for s in json.loads(out)["sources"]}
        self.assertEqual(sources["scripts/package-linux.sh"]["error"], "not a regular file")

    def test_an_oversized_or_non_utf8_source_is_unknown(self):
        with tempfile.TemporaryDirectory() as repo:
            os.makedirs(os.path.join(repo, "scripts"))
            with open(os.path.join(repo, "scripts", "verify.sh"), "wb") as handle:
                handle.write(b"x" * 1048577)
            with open(os.path.join(repo, "scripts", "bundle-macos.sh"), "wb") as handle:
                handle.write(b"\xff\xfe\n")
            _, out, _ = run(repo, "--json")
        sources = {s["path"]: s for s in json.loads(out)["sources"]}
        self.assertEqual(sources["scripts/verify.sh"]["error"], "too large")
        self.assertEqual(sources["scripts/bundle-macos.sh"]["error"], "not UTF-8")

    def test_an_invalid_registry_names_the_first_failing_check(self):
        cases = {
            '{"schema": 2, "widgets": {}, "assets": []}': "invalid registry: schema is not 1",
            '{"schema": 1, "widgets": [], "assets": []}': "invalid registry: widgets is not an object",
            '{"schema": 1, "widgets": {"a": {"ships": []}}, "assets": []}':
                "invalid registry: ships of a is not a non-empty list of linux/macos",
            '{"schema": 1, "widgets": {"a": {"ships": ["linux", "bsd"]}}, "assets": []}':
                "invalid registry: ships of a is not a non-empty list of linux/macos",
            '{"schema": 1, "widgets": {"a": {"ships": ["macos"], "reason": 3}}, "assets": []}':
                "invalid registry: reason of a is not a string",
            '{"schema": 1, "widgets": {}, "assets": {}}': "invalid registry: assets is not a list",
        }
        for text, error in cases.items():
            with self.subTest(error=error), tempfile.TemporaryDirectory() as repo:
                os.makedirs(os.path.join(repo, "dev", "scaffold"))
                with open(os.path.join(repo, "dev", "scaffold", "registry.json"), "w", encoding="utf-8") as handle:
                    handle.write(text + "\n")
                _, out, _ = run(repo, "--json")
                sources = {s["path"]: s for s in json.loads(out)["sources"]}
                self.assertEqual(sources["dev/scaffold/registry.json"]["error"], error)

    def test_an_unknown_argument_is_refused(self):
        done = subprocess.run(
            [sys.executable, "-I", "-B", CHECKER, "--nope"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(done.returncode, 2)
        self.assertEqual(done.stdout, b"")


class ReadsFilesOnly(unittest.TestCase):
    def test_the_checker_source_starts_nothing_and_runs_no_code(self):
        source = read(CHECKER).decode("utf-8")
        for word in FORBIDDEN:
            self.assertNotIn(word, source, word)


if __name__ == "__main__":
    unittest.main()
