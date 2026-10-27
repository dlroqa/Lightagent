#!/usr/bin/env python3
"""Tests for jev-selection-guard.py. Standard library only; no Jev key.

The pin tests answer GitHub's API with stubs, both in-process and through a
stand-in `gh` on PATH. The verdict tests build complete and broken runs from
fixtures, scoring them with the harness's formulas written out again here.

With JEV_GUARD_LIVE_REPO set (and GH_TOKEN), a few pin cases also run
read-only against the real API on fixed commits of this repository, and
JEV_GUARD_LIVE_PR_HEAD names an open pull request head to accept.

Usage: test-jev-selection-guard.py [unittest options]
"""

import hashlib
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "jev-selection-guard.py"
spec = importlib.util.spec_from_file_location("jev_selection_guard", SCRIPT)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

REPO = "dlroqa/Lightagent"
CODE = "1" * 40
MASTER = "2" * 40
OTHER = "3" * 40


def pr(number, state="open", sha=CODE, repo=REPO):
    return {"number": number, "state": state, "head": {"sha": sha, "repo": {"full_name": repo}}}


def compare(status, base=CODE, merge_base=None, ahead=3, behind=0):
    return {
        "status": status,
        "ahead_by": ahead,
        "behind_by": behind,
        "base_commit": {"sha": base},
        "merge_base_commit": {"sha": merge_base or base},
    }


def stub(pulls=None, comparison=None):
    """An API answering the pull-request listing and the comparison of CODE with MASTER."""
    answers = {
        f"repos/{REPO}/commits/{CODE}/pulls": pulls if pulls is not None else [],
        f"repos/{REPO}/compare/{CODE}...{MASTER}": comparison,
    }

    def api(path):
        answer = answers.get(path)
        if answer is None:
            raise AssertionError(f"unexpected API call {path}")
        if isinstance(answer, Exception):
            raise answer
        return answer

    return api


def dispatch(api, sha=CODE, ref="refs/heads/master", workflow_sha=MASTER):
    return guard.pin("workflow_dispatch", ref, sha, workflow_sha, "", REPO, api=api)


class PinTests(unittest.TestCase):
    def refused(self, api, **kwargs):
        with self.assertRaises(guard.Refused) as caught:
            dispatch(api, **kwargs)
        return str(caught.exception)

    def test_a_pull_request_run_tests_its_own_head(self):
        pinned = guard.pin("pull_request", "refs/pull/9/merge", "", MASTER, CODE, REPO, api=stub())
        self.assertEqual((pinned["sha"], pinned["pinned_by"]), (CODE, "pull_request"))

    def test_the_head_of_an_open_pull_request_is_accepted_without_comparing(self):
        pinned = dispatch(stub(pulls=[pr(7, state="closed"), pr(9)]))
        self.assertEqual(pinned["pinned_by"], "open_pull_request")
        self.assertEqual(pinned["pull_requests"], [9])

    def test_an_ancestor_of_the_dispatch_commit_is_accepted(self):
        pinned = dispatch(stub(pulls=[pr(50, state="closed")], comparison=compare("ahead", ahead=6)))
        self.assertEqual((pinned["sha"], pinned["workflow_sha"]), (CODE, MASTER))
        self.assertEqual((pinned["pinned_by"], pinned["ahead_by"]), ("master_ancestor", 6))

    def test_the_dispatch_commit_itself_is_accepted(self):
        def api(path):
            if path == f"repos/{REPO}/commits/{MASTER}/pulls":
                return []
            self.assertEqual(path, f"repos/{REPO}/compare/{MASTER}...{MASTER}")
            return compare("identical", base=MASTER, ahead=0)

        pinned = dispatch(api, sha=MASTER)
        self.assertEqual(pinned["pinned_by"], "master")

    def test_a_commit_ahead_of_master_is_refused(self):
        message = self.refused(stub(comparison=compare("behind", ahead=0, behind=2)))
        self.assertIn("ahead of master", message)

    def test_a_diverged_commit_is_refused(self):
        message = self.refused(stub(comparison=compare("diverged", merge_base=OTHER, behind=1)))
        self.assertIn("diverged", message)

    def test_a_fork_or_stale_head_is_not_an_open_pull_request_head(self):
        pulls = [pr(3, repo="someone/fork"), pr(4, sha=OTHER)]
        message = self.refused(stub(pulls=pulls, comparison=compare("diverged", merge_base=OTHER, behind=1)))
        self.assertIn("diverged", message)

    def test_api_failures_refuse(self):
        failure = guard.ApiError("HTTP 502")
        self.assertIn("cannot list", self.refused(stub(pulls=failure)))
        self.assertIn("could not be checked", self.refused(stub(comparison=failure)))

    def test_missing_or_inconsistent_comparisons_refuse(self):
        broken = [
            {"status": "ahead"},
            {},
            [],
            "ahead",
            compare("ahead", base=OTHER),
            compare("ahead", merge_base=OTHER),
            compare("ahead", behind=1),
            compare("ahead", ahead=0),
            compare("ahead", ahead="3"),
            compare("identical", ahead=0),
            compare("unknown"),
        ]
        for answer in broken:
            with self.subTest(answer=answer):
                self.refused(stub(comparison=answer))

    def test_an_unexpected_pull_request_listing_refuses(self):
        self.assertIn("unexpected answer", self.refused(stub(pulls={"message": "Not Found"})))

    def test_dispatch_is_from_master_with_a_full_commit_id_only(self):
        api = stub(comparison=compare("ahead"))
        self.assertIn("master only", self.refused(api, ref="refs/heads/feature"))
        for sha in ("", CODE[:12], "A" * 40, CODE + "0", CODE + "\n", "g" * 40):
            with self.subTest(sha=sha):
                self.assertIn("full 40-character", self.refused(api, sha=sha))
        self.assertIn("dispatch commit", self.refused(api, workflow_sha="master"))


class PinCommandTests(unittest.TestCase):
    """The command as the workflow runs it, with a stand-in `gh` on PATH."""

    def run_pin(self, answers, sha=CODE):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            (temp / "answers.json").write_text(json.dumps(answers))
            fake = temp / "gh"
            fake.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "answers = json.load(open(os.environ['FAKE_GH_ANSWERS']))\n"
                "answer = answers.get(sys.argv[2])\n"
                "if answer is None or 'exit' in answer:\n"
                "    sys.stderr.write('gh: HTTP 502\\n'); sys.exit((answer or {}).get('exit', 1))\n"
                "print(json.dumps(answer['json']))\n"
            )
            fake.chmod(0o755)
            output = temp / "output"
            env = dict(os.environ, PATH=f"{temp}{os.pathsep}{os.environ['PATH']}")
            env["FAKE_GH_ANSWERS"] = str(temp / "answers.json")
            done = subprocess.run(
                [sys.executable, str(SCRIPT), "pin", "--event", "workflow_dispatch",
                 "--ref", "refs/heads/master", "--sha", sha, "--workflow-sha", MASTER,
                 "--pr-sha", "", "--repo", REPO, "--output", str(output)],
                capture_output=True, text=True, env=env,
            )
            written = output.read_text() if output.exists() else ""
            return done.returncode, done.stdout, written

    def test_an_ancestor_is_pinned_with_both_commits_recorded(self):
        code, stdout, written = self.run_pin({
            f"repos/{REPO}/commits/{CODE}/pulls": {"json": []},
            f"repos/{REPO}/compare/{CODE}...{MASTER}": {"json": compare("ahead")},
        })
        self.assertEqual(code, 0, stdout)
        self.assertIn(f"sha={CODE}\nworkflow_sha={MASTER}\npinned_by=master_ancestor\n", written)
        self.assertIn(f"Workflow source: `{MASTER}`. Code under test: `{CODE}`.", stdout)

    def test_a_failing_api_refuses_and_pins_nothing(self):
        code, stdout, written = self.run_pin({
            f"repos/{REPO}/commits/{CODE}/pulls": {"json": []},
            f"repos/{REPO}/compare/{CODE}...{MASTER}": {"exit": 1},
        })
        self.assertEqual(code, 1)
        self.assertIn("Refused", stdout)
        self.assertEqual(written, "")

    def test_a_non_json_answer_refuses(self):
        code, stdout, _ = self.run_pin({f"repos/{REPO}/commits/{CODE}/pulls": {"json": "x"}})
        self.assertEqual(code, 1)


IDS = [f"sel-{n:03d}" for n in range(1, 25)]
# Twelve kept with the lead, twelve that should be delegated.
LABELS = {case_id: n > 12 for n, case_id in enumerate(IDS, start=1)}
DIGEST_OF = lambda directory, names: hashlib.sha256(
    "".join(
        f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n"
        for name in sorted(names)
    ).encode()
).hexdigest()


def harness_report(cases, failures=None, stopped=None, requests=24, commit=CODE):
    """What the harness at the code under test writes, by its own formulas."""
    lead = sum(1 for delegate in LABELS.values() if not delegate)
    decisions = sum(1 for case in cases if case["decision_ok"])
    unnecessary = sum(1 for case in cases if case["planned"] and not LABELS[case["id"]])
    missed = sum(1 for case in cases if not case["planned"] and LABELS[case["id"]])
    roles = [case["roles_ok"] for case in cases if case["roles_ok"] is not None]
    passed = (
        len(cases) == 24
        and decisions / max(len(cases), 1) >= 0.8
        and unnecessary / max(lead, 1) <= 0.1
    )
    ratio = lambda n, d: {"numerator": n, "denominator": d, "value": n / d if d else None}
    return {
        "status": "FAILED" if stopped == "endpoint_failures" else ("PASSED" if passed else "BELOW_THRESHOLD"),
        "evaluated": True,
        "commit": commit,
        "requests": requests,
        "request_ceiling": 24,
        "retries": 0,
        "input_tokens": 600 * requests,
        "output_tokens": 90 * requests,
        "latency_ms": [800] * requests,
        "failures": failures or {},
        "stopped": stopped,
        "totals": {
            "cases": len(cases),
            "decision_accuracy": ratio(decisions, len(cases)),
            "unnecessary_delegation": ratio(unnecessary, lead),
            "missed_delegation": ratio(missed, len(cases) - min(lead, len(cases))),
            "role_match": ratio(sum(1 for ok in roles if ok), len(roles)),
            "passed": passed,
        },
        "cases": cases,
    }


def scored(planned):
    """Scored cases for the given set of planned ids."""
    return [
        {
            "id": case_id,
            "planned": case_id in planned,
            "decision_ok": (case_id in planned) == LABELS[case_id],
            "roles_ok": True if case_id in planned and LABELS[case_id] else None,
            "fallback": None,
        }
        for case_id in IDS
    ]


class Run:
    """A dispatched run's records on disk: complete and passing until changed."""

    def __init__(self, root, ids=IDS):
        self.root = root
        self.set = root / "selection.v1.json"
        self.requests = root / "requests"
        self.responses = root / "responses"
        self.reports = root / "reports"
        for directory in (self.requests, self.responses, self.reports):
            directory.mkdir()
        cases = [{"id": case_id, "delegate": LABELS.get(case_id, False), "prompt": "p"} for case_id in ids]
        self.set.write_text(json.dumps(
            {"pass": {"decision_accuracy_min": 0.8, "unnecessary_delegation_max": 0.1}, "cases": cases}
        ))
        for case_id in IDS:
            (self.requests / f"{case_id}.json").write_text(
                json.dumps({"model": "jev", "questions": {}, "state": case_id})
            )
            (self.responses / f"{case_id}.json").write_text(
                json.dumps({"answers": {}, "model": "jev-2026-10"})
            )
        (self.requests / "manifest.json").write_text(json.dumps({"cases": IDS, "commit": CODE}))
        self.digest = DIGEST_OF(self.requests, [f"{case_id}.json" for case_id in IDS])
        self.send({"sent": 24, "ceiling": 24, "retries": 0, "stopped": "no"})
        self.report(harness_report(scored({i for i in IDS if LABELS[i]})))

    def send(self, record):
        (self.responses / "send.json").write_text(json.dumps(record))

    def report(self, report):
        (self.reports / "specialist-selection.json").write_text(json.dumps(report))

    def verdict(self):
        return guard.verdict(self.set, self.requests, self.responses, self.reports, self.digest, CODE, MASTER)


class VerdictTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.run_ = Run(pathlib.Path(temp.name))

    def incomplete(self, *expected):
        result = self.run_.verdict()
        self.assertEqual(result["status"], "INCOMPLETE", result["problems"])
        self.assertFalse(result["passed"] or result["complete"])
        self.assertNotIn("metrics", result)
        for text in expected:
            self.assertTrue(any(text in problem for problem in result["problems"]), (text, result["problems"]))
        return result

    def test_a_complete_run_meeting_the_rule_passes(self):
        result = self.run_.verdict()
        self.assertEqual((result["status"], result["problems"]), ("PASSED", []))
        self.assertTrue(result["complete"] and result["passed"])
        self.assertNotIn("diagnostic_metrics", result)
        metrics = result["metrics"]
        self.assertEqual(metrics["decision_accuracy"]["numerator"], 24)
        self.assertEqual(metrics["unnecessary_delegation"]["numerator"], 0)
        self.assertEqual(metrics["missed_delegation"]["denominator"], 12)
        self.assertEqual(metrics["role_match"], {"numerator": 12, "denominator": 12, "value": 1.0})
        self.assertEqual(result["model_identifiers"], ["model=jev-2026-10"])
        self.assertEqual(result["usage"]["requests"], 24)

    def test_a_complete_run_below_the_rule_is_below_threshold(self):
        # Two unnecessary delegations: 2/12 is over the 0.1 limit.
        planned = {i for i in IDS if LABELS[i]} | {"sel-001", "sel-002"}
        self.run_.report(harness_report(scored(planned)))
        result = self.run_.verdict()
        self.assertEqual(result["status"], "BELOW_THRESHOLD", result["problems"])
        self.assertTrue(result["complete"])
        self.assertFalse(result["passed"])
        self.assertEqual(result["metrics"]["unnecessary_delegation"]["numerator"], 2)

    def test_scattered_failures_are_incomplete_though_the_harness_passed(self):
        # Two lone failures on keep-with-lead prompts: send discarded their
        # responses and did not stop, and the harness scored both as the lead
        # keeping the task, which is correct for them, so it reported PASSED.
        for case_id in ("sel-003", "sel-010"):
            (self.run_.responses / f"{case_id}.json").unlink()
        report = harness_report(scored({i for i in IDS if LABELS[i]}), failures={"http_502": 2})
        self.assertEqual(report["status"], "PASSED")
        self.run_.report(report)
        result = self.incomplete(
            "no successful response for sel-003", "no successful response for sel-010", "harness report failures"
        )
        self.assertEqual(result["diagnostic_metrics"]["decision_accuracy"]["numerator"], 24)
        self.assertNotIn("meets_pass_rule", result["diagnostic_metrics"])

    def test_one_missing_response_is_incomplete(self):
        (self.run_.responses / "sel-017.json").unlink()
        self.incomplete("no successful response for sel-017")

    def test_duplicate_prompt_ids_in_the_set_are_incomplete(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        ids = list(IDS)
        ids[5] = ids[4]
        self.run_ = Run(pathlib.Path(temp.name), ids=ids)
        self.incomplete("duplicate prompt id sel-005")

    def test_a_prompt_scored_twice_is_incomplete(self):
        cases = scored({i for i in IDS if LABELS[i]})
        cases[2] = dict(cases[1])
        self.run_.report(harness_report(cases))
        self.incomplete("duplicate scored prompt id sel-002", "exactly once")

    def test_malformed_responses_are_incomplete(self):
        (self.run_.responses / "sel-004.json").write_text("[1, 2]")
        (self.run_.responses / "sel-005.json").write_text("{not json")
        self.incomplete("sel-004 is not a JSON object", "the response to sel-005 is not readable JSON")

    def test_a_stopped_run_is_incomplete_without_a_report(self):
        for case_id in IDS[4:]:
            (self.run_.responses / f"{case_id}.json").unlink()
        (self.run_.reports / "specialist-selection.json").unlink()
        self.run_.send({"sent": 5, "ceiling": 24, "retries": 0, "stopped": "credential"})
        result = self.incomplete("send record stopped is 'credential'", "the harness report is missing")
        self.assertIsNone(result["usage"])

    def test_a_partial_ceiling_is_incomplete(self):
        for case_id in IDS[12:]:
            (self.run_.responses / f"{case_id}.json").unlink()
        self.run_.send({"sent": 12, "ceiling": 12, "retries": 0, "stopped": "request_ceiling"})
        self.run_.report(harness_report(scored({i for i in IDS if LABELS[i]})[:12], requests=12,
                                        stopped="request_ceiling"))
        self.incomplete("send record ceiling is 12", "harness report requests is 12")

    def test_send_and_scoring_records_must_agree(self):
        self.run_.send({"sent": 23, "ceiling": 24, "retries": 0, "stopped": "no"})
        self.incomplete("send record sent is 23")

    def test_requests_changed_after_the_digest_are_incomplete(self):
        (self.run_.requests / "sel-009.json").write_text(json.dumps({"state": "changed"}))
        self.incomplete("do not match the digest")

    def test_unexpected_files_are_incomplete(self):
        (self.run_.responses / "sel-099.json").write_text("{}")
        (self.run_.requests / "sel-098.json").write_text("{}")
        self.incomplete("unexpected response file sel-099.json", "unexpected exported file sel-098.json")

    def test_a_report_for_another_commit_is_incomplete(self):
        self.run_.report(harness_report(scored({i for i in IDS if LABELS[i]}), commit=OTHER))
        self.incomplete("harness report commit")

    def test_harness_totals_that_disagree_with_the_cases_are_incomplete(self):
        report = harness_report(scored({i for i in IDS if LABELS[i]} | {"sel-001", "sel-002"}))
        report["totals"]["passed"] = True
        report["totals"]["unnecessary_delegation"]["numerator"] = 0
        self.run_.report(report)
        self.incomplete("unnecessary_delegation numerator disagrees")

    def test_a_set_without_24_prompts_is_incomplete(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.run_ = Run(pathlib.Path(temp.name), ids=IDS[:23])
        self.incomplete("has 23 prompts, expected 24")

    def test_nothing_downloaded_is_incomplete(self):
        temp = pathlib.Path(self.run_.root) / "empty"
        result = guard.verdict(self.run_.set, temp / "q", temp / "r", temp / "s", "", CODE, MASTER)
        self.assertEqual(result["status"], "INCOMPLETE")
        self.assertIn("the exported requests are missing", result["problems"])

    def test_no_credential_is_not_evaluated(self):
        for path in self.run_.responses.iterdir():
            path.unlink()
        (self.run_.responses / "status.json").write_text(json.dumps({"status": "BLOCKED", "reason": "no key"}))
        self.assertEqual(self.run_.verdict()["status"], "NOT_EVALUATED")
        (self.run_.responses / "sel-001.json").write_text("{}")
        self.incomplete("BLOCKED but also recorded responses")

    def test_the_digest_matches_coreutils_as_send_computes_it(self):
        done = subprocess.run(
            ["bash", "-c", "sha256sum sel-*.json | sha256sum | cut -d' ' -f1"],
            cwd=self.run_.requests, capture_output=True, text=True, check=True,
        )
        names = [path.name for path in self.run_.requests.glob("sel-*.json")]
        self.assertEqual(guard.request_digest(self.run_.requests, names), done.stdout.strip())


class VerdictCommandTests(unittest.TestCase):
    def run_verdict(self, run, out):
        output = run.root / "output"
        done = subprocess.run(
            [sys.executable, str(SCRIPT), "verdict", "--set", str(run.set), "--requests", str(run.requests),
             "--responses", str(run.responses), "--reports", str(run.reports), "--digest", run.digest,
             "--code-sha", CODE, "--workflow-sha", MASTER, "--out", str(out), "--output", str(output)],
            capture_output=True, text=True,
        )
        return done.returncode, json.loads((out / "verdict.json").read_text()), (out / "verdict.md").read_text(), output.read_text()

    def test_an_incomplete_run_fails_and_never_says_passed(self):
        with tempfile.TemporaryDirectory() as temp:
            run = Run(pathlib.Path(temp))
            (run.responses / "sel-003.json").unlink()
            run.report(harness_report(scored({i for i in IDS if LABELS[i]}), failures={"timeout": 1}))
            code, record, markdown, output = self.run_verdict(run, pathlib.Path(temp) / "out")
            self.assertEqual(code, 1)
            self.assertEqual(output, "status=INCOMPLETE\n")
            self.assertEqual((record["status"], record["passed"]), ("INCOMPLETE", False))
            self.assertEqual((record["code_sha"], record["workflow_sha"]), (CODE, MASTER))
            self.assertNotIn("PASSED", json.dumps(record))
            self.assertNotIn("PASSED", markdown)
            self.assertIn("Diagnostic only (incomplete run, not a result)", markdown)
            self.assertIn(f"Workflow source (master): `{MASTER}`. Code under test: `{CODE}`.", markdown)

    def test_a_passing_run_succeeds_and_reports_each_measure(self):
        with tempfile.TemporaryDirectory() as temp:
            run = Run(pathlib.Path(temp))
            code, record, markdown, _ = self.run_verdict(run, pathlib.Path(temp) / "out")
            self.assertEqual((code, record["status"]), (0, "PASSED"))
            self.assertFalse(record["counts_as_competence_evidence"])
            for measure in ("Decision accuracy", "Unnecessary delegation", "Missed delegation", "Exact role-set match"):
                self.assertIn(measure, markdown)
            self.assertIn("not specialist competence or efficiency", markdown)


LIVE_REPO = os.environ.get("JEV_GUARD_LIVE_REPO")
LIVE_PR_HEAD = os.environ.get("JEV_GUARD_LIVE_PR_HEAD")
# Fixed commits of this repository: master after the specialist stack, its
# first parent, and the closed, unmerged #51 head (diverged from master).
LIVE_MASTER = "b1ffd79c33bc39a13cf26ddf94126b6cb83bfd1c"
LIVE_PARENT = "f6632d8d26ea0b83a1c2a7d6e78daa7c2a8dde4a"
LIVE_DIVERGED = "e4a306c6d52603976aebadafc9c0c9d1748d77e4"


@unittest.skipUnless(LIVE_REPO, "JEV_GUARD_LIVE_REPO is not set")
class LivePinTests(unittest.TestCase):
    """Read-only calls to the real GitHub API; no Jev key is involved."""

    def pin(self, sha, workflow_sha):
        return guard.pin("workflow_dispatch", "refs/heads/master", sha, workflow_sha, "", LIVE_REPO)

    def test_the_dispatch_commit_itself(self):
        self.assertEqual(self.pin(LIVE_MASTER, LIVE_MASTER)["pinned_by"], "master")

    def test_an_ancestor(self):
        pinned = self.pin(LIVE_PARENT, LIVE_MASTER)
        self.assertEqual(pinned["pinned_by"], "master_ancestor")
        self.assertGreaterEqual(pinned["ahead_by"], 1)

    def test_a_commit_ahead_of_the_dispatch_commit(self):
        with self.assertRaisesRegex(guard.Refused, "ahead of master"):
            self.pin(LIVE_MASTER, LIVE_PARENT)

    def test_a_diverged_commit(self):
        with self.assertRaisesRegex(guard.Refused, "diverged"):
            self.pin(LIVE_DIVERGED, LIVE_MASTER)

    def test_an_unknown_commit(self):
        with self.assertRaisesRegex(guard.Refused, "cannot list|could not be checked"):
            self.pin("0123456789abcdef0123456789abcdef01234567", LIVE_MASTER)

    @unittest.skipUnless(LIVE_PR_HEAD, "JEV_GUARD_LIVE_PR_HEAD is not set")
    def test_an_open_pull_request_head_that_is_not_on_master(self):
        pinned = self.pin(LIVE_PR_HEAD, LIVE_MASTER)
        self.assertEqual(pinned["pinned_by"], "open_pull_request")


if __name__ == "__main__":
    unittest.main()
