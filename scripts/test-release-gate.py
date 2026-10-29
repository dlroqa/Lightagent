#!/usr/bin/env python3
"""Tests for release-gate.sh and the shape of .github/workflows/release.yml.

The decision tests run the real gate script with every event, ref and
`dry_run` value a run can present: a manual dry run, an omitted input, a dry
run explicitly turned off, the tag push a release is cut from, and events that
never release. The shape tests parse release.yml and check that nothing but the
gated `publish` job can write: it alone has write permission, it alone uses a
release-writing action, and it runs only when the gate says so on a tag.

Needs PyYAML (check.yml installs a pinned version). Nothing here contacts
GitHub.

Usage: test-release-gate.py [unittest options]
"""

import os
import pathlib
import re
import subprocess
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parent.parent
GATE = ROOT / "scripts" / "release-gate.sh"
WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"


def workspace_version():
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)[1]
    return re.search(r'^version\s*=\s*"([^"]+)"', section, re.M).group(1)


TAG = "v" + workspace_version()


def gate(event, ref_type, ref_name, dry_run):
    env = {k: v for k, v in os.environ.items() if k not in ("EVENT", "REF_TYPE", "REF_NAME", "DRY_RUN")}
    env.update(EVENT=event, REF_TYPE=ref_type, REF_NAME=ref_name, DRY_RUN=dry_run)
    done = subprocess.run(["bash", str(GATE)], env=env, capture_output=True, text=True)
    outputs = dict(line.split("=", 1) for line in done.stdout.splitlines() if "=" in line)
    return done.returncode, outputs


class GateDecisionTests(unittest.TestCase):
    def assert_dry(self, *args):
        code, out = gate(*args)
        self.assertEqual(code, 0, out)
        self.assertEqual(out.get("publish"), "false", out)

    def assert_publishes(self, *args):
        code, out = gate(*args)
        self.assertEqual(code, 0, out)
        self.assertEqual(out.get("publish"), "true", out)

    def assert_refused(self, *args):
        code, out = gate(*args)
        self.assertNotEqual(code, 0, out)
        self.assertNotEqual(out.get("publish"), "true", out)

    # A manual dry run, on a branch or on the release tag itself.
    def test_manual_dry_run_on_a_branch(self):
        self.assert_dry("workflow_dispatch", "branch", "release/v0.5.0-dry-run", "true")

    def test_manual_dry_run_on_the_tag(self):
        self.assert_dry("workflow_dispatch", "tag", TAG, "true")

    # An omitted input takes the default `true`; if GitHub ever passed no
    # value at all, toJSON gives `null` and the run must still be dry.
    def test_manual_input_omitted(self):
        self.assert_dry("workflow_dispatch", "branch", "master", "null")
        self.assert_dry("workflow_dispatch", "tag", TAG, "null")
        self.assert_dry("workflow_dispatch", "tag", TAG, "")

    # Only the JSON boolean `false` counts: no string truthiness.
    def test_only_boolean_false_turns_dry_run_off(self):
        for value in ('"false"', "False", "0", "no", " false"):
            with self.subTest(value=value):
                self.assert_dry("workflow_dispatch", "tag", TAG, value)

    # Dry run explicitly off: only the tag naming this version may publish.
    def test_manual_dry_run_off_on_the_tag(self):
        self.assert_publishes("workflow_dispatch", "tag", TAG, "false")

    def test_manual_dry_run_off_on_a_branch_is_refused(self):
        self.assert_refused("workflow_dispatch", "branch", "master", "false")
        self.assert_refused("workflow_dispatch", "branch", TAG, "false")

    def test_manual_dry_run_off_on_another_tag_is_refused(self):
        self.assert_refused("workflow_dispatch", "tag", "v0.0.0", "false")
        self.assert_refused("workflow_dispatch", "tag", TAG + "-rc1", "false")

    # The tag push a release is cut from, and a tag that does not match.
    def test_tag_push_publishes(self):
        self.assert_publishes("push", "tag", TAG, "null")

    def test_tag_push_ignores_dry_run(self):
        # A push has no inputs; whatever DRY_RUN holds cannot change it.
        self.assert_publishes("push", "tag", TAG, "true")

    def test_mismatched_tag_push_is_refused(self):
        self.assert_refused("push", "tag", "v0.0.0", "null")

    # Events that never release.
    def test_non_release_events(self):
        for event, ref_type, ref_name in [
            ("push", "branch", "master"),
            ("pull_request", "branch", "42/merge"),
            ("pull_request_target", "branch", "master"),
            ("schedule", "branch", "master"),
            ("workflow_run", "branch", "master"),
            ("release", "tag", TAG),
            ("", "", ""),
        ]:
            for dry_run in ("false", "true", "null"):
                with self.subTest(event=event, dry_run=dry_run):
                    self.assert_dry(event, ref_type, ref_name, dry_run)


# What a step may use or run only in the publish job.
RELEASE_WRITERS = re.compile(
    r"action-gh-release|create-release|upload-release-asset|release-action"
    r"|\bgh\s+release\b|\bgh\s+api\b|\bgit\s+tag\b|\bgit\s+push\b|/releases\b"
)


class WorkflowShapeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.doc = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
        # YAML 1.1 reads the bare key `on` as true.
        cls.on = cls.doc.get("on", cls.doc.get(True))
        cls.jobs = cls.doc["jobs"]

    def steps_text(self, job):
        return yaml.safe_dump(self.jobs[job].get("steps", []))

    def test_triggers(self):
        self.assertEqual(set(self.on), {"push", "workflow_dispatch"})
        self.assertEqual(self.on["push"], {"tags": ["v*"]})
        dry_run = self.on["workflow_dispatch"]["inputs"]["dry_run"]
        self.assertEqual(dry_run["type"], "boolean")
        self.assertIs(dry_run["default"], True)

    def test_workflow_is_read_only_by_default(self):
        self.assertEqual(self.doc["permissions"], {"contents": "read"})

    def test_only_publish_can_write(self):
        for name, job in self.jobs.items():
            perms = job.get("permissions")
            if name == "publish":
                self.assertEqual(perms, {"contents": "write"})
            else:
                self.assertIn(perms, (None, {"contents": "read"}), name)

    def test_only_publish_writes_releases_or_tags(self):
        for name in self.jobs:
            found = RELEASE_WRITERS.findall(self.steps_text(name))
            if name == "publish":
                self.assertTrue(found)
            else:
                self.assertEqual(found, [], name)

    def test_publish_is_gated(self):
        publish = self.jobs["publish"]
        self.assertEqual(
            publish["if"], "needs.gate.outputs.publish == 'true' && github.ref_type == 'tag'"
        )
        self.assertIn("gate", publish["needs"])
        self.assertIn("build", publish["needs"])
        self.assertIn("checksums", publish["needs"])

    def test_no_other_job_depends_on_publish(self):
        for name, job in self.jobs.items():
            needs = job.get("needs", [])
            needs = [needs] if isinstance(needs, str) else needs
            self.assertNotIn("publish", needs, name)

    def test_gate_passes_the_typed_input_exactly(self):
        gate_job = self.jobs["gate"]
        self.assertEqual(gate_job["outputs"]["publish"], "${{ steps.decide.outputs.publish }}")
        (step,) = [s for s in gate_job["steps"] if s.get("id") == "decide"]
        self.assertEqual(step["env"]["DRY_RUN"], "${{ toJSON(inputs.dry_run) }}")
        self.assertEqual(step["env"]["EVENT"], "${{ github.event_name }}")
        self.assertEqual(step["env"]["REF_TYPE"], "${{ github.ref_type }}")
        self.assertEqual(step["env"]["REF_NAME"], "${{ github.ref_name }}")
        self.assertEqual(step["shell"], "bash")
        self.assertIn("scripts/release-gate.sh", step["run"])
        self.assertIn('"$GITHUB_OUTPUT"', step["run"])

    def test_dry_runs_still_build_and_checksum(self):
        # Neither depends on the gate's answer, only on the gate succeeding.
        for name in ("build", "checksums"):
            self.assertNotIn("if", self.jobs[name], name)
        self.assertIn("sha256sum", self.steps_text("checksums"))
        self.assertIn("release-SHA256SUMS", self.steps_text("checksums"))
        # The release attaches the dry run's checksums; it does not make its own.
        self.assertNotIn("sha256sum", self.steps_text("publish"))
        self.assertIn("release-SHA256SUMS", self.steps_text("publish"))

    def matrix(self):
        return self.jobs["build"]["strategy"]["matrix"]["include"]

    def test_every_platform_is_still_built(self):
        triples = {e["triple"] for e in self.matrix()}
        self.assertEqual(
            triples,
            {
                "aarch64-apple-darwin",
                "x86_64-apple-darwin",
                "x86_64-pc-windows-msvc",
                "x86_64-unknown-linux-gnu",
            },
        )
        self.assertIs(self.jobs["build"]["strategy"]["fail-fast"], False)

    def test_package_and_artifact_names_are_distinct(self):
        for key in ("triple", "label"):
            values = [e[key] for e in self.matrix()]
            self.assertEqual(len(values), len(set(values)), key)

    def test_each_package_is_built_natively_on_a_checked_architecture(self):
        expected = {
            # triple: (runner label from GitHub's documented hosted runners, uname -m)
            "aarch64-apple-darwin": ("macos-15", "arm64"),
            "x86_64-apple-darwin": ("macos-15-intel", "x86_64"),
            "x86_64-pc-windows-msvc": ("windows-latest", "x86_64"),
            "x86_64-unknown-linux-gnu": ("ubuntu-22.04", "x86_64"),
        }
        for entry in self.matrix():
            self.assertEqual((entry["os"], entry["arch"]), expected[entry["triple"]], entry)
        (step,) = [s for s in self.jobs["build"]["steps"] if s.get("name") == "Runner architecture"]
        self.assertEqual(step["env"], {"WANT_ARCH": "${{ matrix.arch }}", "WANT_TRIPLE": "${{ matrix.triple }}"})
        for check in ('"$arch" = "$WANT_ARCH"', '"$host" = "$WANT_TRIPLE"', "sysctl.proc_translated", "hw.optional.arm64"):
            self.assertIn(check, step["run"])
        # The check runs before anything is built.
        names = [s.get("name") or s.get("run") or s.get("uses") for s in self.jobs["build"]["steps"]]
        self.assertLess(names.index("Runner architecture"), names.index("cargo build --release -p lightagent --target ${{ matrix.triple }}"))

    def test_macos_packages_declare_their_minimum(self):
        floors = {e["triple"]: e.get("macos_min") for e in self.matrix()}
        self.assertEqual(floors["aarch64-apple-darwin"], "11.0")
        self.assertEqual(floors["x86_64-apple-darwin"], "10.14")
        self.assertIsNone(floors["x86_64-pc-windows-msvc"])
        self.assertIsNone(floors["x86_64-unknown-linux-gnu"])
        (step,) = [s for s in self.jobs["build"]["steps"] if s.get("name") == "macOS deployment target"]
        self.assertEqual(step["if"], "matrix.macos_min")
        self.assertIn("MACOSX_DEPLOYMENT_TARGET=${{ matrix.macos_min }}", step["run"])
        self.assertIn('"$GITHUB_ENV"', step["run"])

    def test_checksums_require_every_package(self):
        (step,) = [s for s in self.jobs["checksums"]["steps"] if s.get("name") == "Checksums"]
        self.assertEqual(int(step["env"]["PACKAGES"]), len(self.matrix()))
        self.assertIn('"$count" = "$PACKAGES"', step["run"])

    def test_smoke_checks_macos_architecture_and_minimum(self):
        smoke = (ROOT / "scripts" / "smoke-lightagent-package.sh").read_text(encoding="utf-8")
        for check in ("lipo -archs", "vtool -show-build", "MACOSX_DEPLOYMENT_TARGET"):
            self.assertIn(check, smoke)


if __name__ == "__main__":
    unittest.main()
