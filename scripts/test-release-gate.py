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


def gate(event, ref_type, ref_name, dry_run, signing="", unsigned=None):
    """`unsigned` replaces the gate's list of tags approved to publish unsigned; None keeps the shipped list."""
    env = {k: v for k, v in os.environ.items()
           if k not in ("EVENT", "REF_TYPE", "REF_NAME", "DRY_RUN", "SIGNING", "RELEASE_GATE_UNSIGNED_RELEASES")}
    env.update(EVENT=event, REF_TYPE=ref_type, REF_NAME=ref_name, DRY_RUN=dry_run, SIGNING=signing)
    if unsigned is not None:
        env["RELEASE_GATE_UNSIGNED_RELEASES"] = unsigned
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
        # Signed, and only for a tag not approved to publish unsigned.
        self.assert_publishes("workflow_dispatch", "tag", TAG, "false", "signed", "")

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


    # Signing: a manual run is unsigned unless it asks for `signed`, a tag
    # push always signs, and any other value is refused before anything else.
    def test_manual_runs_are_unsigned_by_default(self):
        for signing in ("", "unsigned"):
            with self.subTest(signing=signing):
                code, out = gate("workflow_dispatch", "branch", "release/v0.6.0", "true", signing)
                self.assertEqual((code, out.get("signing"), out.get("publish")), (0, "unsigned", "false"), out)

    def test_a_signed_dry_run_stays_dry(self):
        code, out = gate("workflow_dispatch", "branch", "release/v0.6.0", "true", "signed")
        self.assertEqual((code, out.get("signing"), out.get("publish")), (0, "signed", "false"), out)

    def test_an_unknown_signing_request_is_refused(self):
        for signing in ("Signed", "yes", "true", " signed", "null"):
            with self.subTest(signing=signing):
                self.assert_refused("workflow_dispatch", "tag", TAG, "false", signing)

    def test_tag_push_signs(self):
        code, out = gate("push", "tag", TAG, "null", "unsigned", "")
        self.assertEqual((code, out.get("signing"), out.get("publish")), (0, "signed", "true"), out)

    # The owner's unsigned approval: exactly the listed tag, from its tag push.
    def test_an_approved_tag_push_publishes_unsigned(self):
        code, out = gate("push", "tag", TAG, "null", "", TAG)
        self.assertEqual((code, out.get("signing"), out.get("publish")), (0, "unsigned", "true"), out)

    def test_the_shipped_approval_is_v0_6_0_only(self):
        self.assertIn('UNSIGNED_RELEASES="${RELEASE_GATE_UNSIGNED_RELEASES-v0.6.0}"', GATE.read_text(encoding="utf-8"))
        code, out = gate("push", "tag", "v0.6.0", "null")
        if TAG == "v0.6.0":
            self.assertEqual((code, out.get("signing"), out.get("publish")), (0, "unsigned", "true"), out)
        else:
            self.assertNotEqual(code, 0, out)

    def test_the_approval_matches_whole_tags_only(self):
        for listed in (TAG[:-1], TAG + "0", "v" + TAG, TAG.upper(), "*", "v*"):
            with self.subTest(listed=listed):
                code, out = gate("push", "tag", TAG, "null", "", listed)
                self.assertEqual((code, out.get("signing"), out.get("publish")), (0, "signed", "true"), out)

    def test_an_approved_tag_cannot_publish_from_a_manual_run(self):
        for signing in ("unsigned", "signed", ""):
            with self.subTest(signing=signing):
                self.assert_refused("workflow_dispatch", "tag", TAG, "false", signing, TAG)

    def test_unauthorized_unsigned_publication_is_refused(self):
        for signing in ("unsigned", ""):
            with self.subTest(signing=signing):
                self.assert_refused("workflow_dispatch", "tag", TAG, "false", signing, "")
                self.assert_refused("workflow_dispatch", "tag", TAG, "false", signing, "v0.0.1")

    def test_the_approval_never_changes_dry_runs(self):
        for signing in ("unsigned", "signed"):
            with self.subTest(signing=signing):
                code, out = gate("workflow_dispatch", "branch", "release/v0.6.0", "true", signing, TAG)
                self.assertEqual((code, out.get("signing"), out.get("publish")), (0, signing, "false"), out)

    def test_other_events_never_sign(self):
        for event in ("push", "pull_request", "schedule", ""):
            with self.subTest(event=event):
                code, out = gate(event, "branch", "master", "true", "signed")
                self.assertEqual((code, out.get("signing")), (0, "unsigned"), out)


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
        signing = self.on["workflow_dispatch"]["inputs"]["signing"]
        self.assertEqual((signing["type"], signing["options"], signing["default"]), ("choice", ["unsigned", "signed"], "unsigned"))

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
        self.assertEqual(gate_job["outputs"]["signing"], "${{ steps.decide.outputs.signing }}")
        (step,) = [s for s in gate_job["steps"] if s.get("id") == "decide"]
        self.assertEqual(step["env"]["DRY_RUN"], "${{ toJSON(inputs.dry_run) }}")
        self.assertEqual(step["env"]["SIGNING"], "${{ inputs.signing }}")
        self.assertEqual(step["env"]["EVENT"], "${{ github.event_name }}")
        self.assertEqual(step["env"]["REF_TYPE"], "${{ github.ref_type }}")
        self.assertEqual(step["env"]["REF_NAME"], "${{ github.ref_name }}")
        self.assertEqual(step["shell"], "bash")
        self.assertIn("scripts/release-gate.sh", step["run"])
        self.assertIn('"$GITHUB_OUTPUT"', step["run"])

    def test_one_release_per_tag(self):
        self.assertEqual(self.doc["concurrency"], {"group": "release-${{ github.ref }}", "cancel-in-progress": False})
        steps = self.jobs["publish"]["steps"]
        names = [s.get("name") for s in steps]
        guard = names.index("No release exists for this tag yet")
        drafting = next(i for i, s in enumerate(steps) if "action-gh-release" in str(s.get("uses", "")))
        self.assertLess(guard, drafting)
        for needed in ("already exists", "sha256sum -c SHA256SUMS", "docs/releases/"):
            self.assertIn(needed, steps[guard]["run"])
        self.assertEqual(steps[drafting]["with"]["draft"], True)
        self.assertIn("docs/releases/", steps[drafting]["with"]["body_path"])

    def test_the_workflow_cannot_widen_the_unsigned_approval(self):
        self.assertNotIn("RELEASE_GATE_UNSIGNED_RELEASES", WORKFLOW.read_text(encoding="utf-8"))
        (step,) = [s for s in self.jobs["gate"]["steps"] if s.get("id") == "decide"]
        self.assertEqual(set(step["env"]), {"EVENT", "REF_TYPE", "REF_NAME", "DRY_RUN", "SIGNING"})

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

    def test_every_released_platform_runs_the_full_check(self):
        # check.yml's `check` job (fmt, clippy, workspace tests, web build,
        # provisioning contract, CLI smoke) runs natively on every platform a
        # release ships, on the same runner and with the same macOS floor.
        check = yaml.safe_load((ROOT / ".github" / "workflows" / "check.yml").read_text(encoding="utf-8"))
        job = check["jobs"]["check"]
        tested = {e["label"]: e for e in job["strategy"]["matrix"]["include"]}
        for entry in self.matrix():
            ran = tested.get(entry["label"])
            self.assertIsNotNone(ran, entry["label"])
            self.assertEqual(
                (ran["arch"], ran["host"], ran.get("macos_min")),
                (entry["arch"], entry["triple"], entry.get("macos_min")),
                entry["label"],
            )
            if entry["os"].startswith("macos"):
                self.assertEqual(ran["os"], entry["os"], entry["label"])
        self.assertEqual(set(tested), {e["label"] for e in self.matrix()})
        self.assertIs(job["strategy"]["fail-fast"], False)
        (step,) = [s for s in job["steps"] if s.get("name") == "Runner architecture"]
        for needed in ('"$arch" = "$WANT_ARCH"', '"$host" = "$WANT_HOST"', "sysctl.proc_translated", "hw.optional.arm64"):
            self.assertIn(needed, step["run"])
        self.assertIn("bash ./scripts/check.sh", [s.get("run") for s in job["steps"]])

    def desktop_matrix(self):
        return self.jobs["desktop"]["strategy"]["matrix"]["include"]

    def test_desktop_is_built_natively_on_every_platform(self):
        expected = {
            "aarch64-apple-darwin": ("macos-15", "arm64", "11.0"),
            "x86_64-apple-darwin": ("macos-15-intel", "x86_64", "10.14"),
            "x86_64-pc-windows-msvc": ("windows-latest", "x86_64", None),
            "x86_64-unknown-linux-gnu": ("ubuntu-22.04", "x86_64", None),
        }
        self.assertEqual({e["triple"] for e in self.desktop_matrix()}, set(expected))
        for entry in self.desktop_matrix():
            self.assertEqual((entry["os"], entry["arch"], entry.get("macos_min")), expected[entry["triple"]], entry)
        self.assertIs(self.jobs["desktop"]["strategy"]["fail-fast"], False)
        labels = [e["label"] for e in self.desktop_matrix()]
        self.assertEqual(len(labels), len(set(labels)))
        self.assertIn("bash ./scripts/check-versions.sh", self.steps_text("desktop"))

    def test_desktop_dry_runs_build_like_releases(self):
        for name in ("desktop", "desktop-older-os", "desktop-sources"):
            self.assertNotIn("if", self.jobs[name], name)
        self.assertEqual(self.jobs["desktop"]["needs"], "gate")
        self.assertEqual(self.jobs["desktop-sources"]["needs"], "gate")
        self.assertEqual(self.jobs["desktop-older-os"]["needs"], "desktop")

    def test_releases_wait_for_the_desktop(self):
        for name in ("desktop", "desktop-older-os", "desktop-sources"):
            self.assertIn(name, self.jobs["publish"]["needs"])
        for name in ("build", "desktop", "desktop-sources"):
            self.assertIn(name, self.jobs["checksums"]["needs"])
        self.assertIn("release-desktop-*", self.steps_text("publish"))
        self.assertIn("release-desktop-*", self.steps_text("checksums"))

    def test_checksums_require_every_desktop_installer_and_the_sources(self):
        (step,) = [s for s in self.jobs["checksums"]["steps"] if s.get("name") == "Checksums"]
        self.assertEqual(int(step["env"]["DESKTOP_INSTALLERS"]), sum(int(e["installers"]) for e in self.desktop_matrix()))
        for needed in ('"$installers" = "$DESKTOP_INSTALLERS"', "desktop-mpl-sources-", "Lightagent_*"):
            self.assertIn(needed, step["run"])

    def test_signing_comes_only_from_secrets_and_is_reported(self):
        steps = self.jobs["desktop"]["steps"]
        (signing,) = [s for s in steps if s.get("id") == "signing"]
        env = dict(signing["env"])
        self.assertEqual(env.pop("REQUESTED"), "${{ needs.gate.outputs.signing }}")
        for name, value in env.items():
            self.assertTrue(value.startswith("${{ secrets.") or value.startswith("${{ vars."), name)
        for needed in ("missing ", "APPLE_API_KEY_P8", "WINDOWS_SIGN_COMMAND"):
            self.assertIn(needed, signing["run"])
        names = [s.get("name") for s in steps]
        self.assertLess(names.index(signing["name"]), names.index("Build and bundle"))
        self.assertIn("Signature state of what ships", names)
        self.assertIn("The notices name every crate version found in the shipped binaries", names)
        (state,) = [s for s in steps if s.get("name") == "Signature state of what ships"]
        for needed in ("com.apple.quarantine", "source=Notarized Developer ID", "stapler validate", "TimeStamperCertificate",
                       "lightagent-sidecar.exe", "dmg_stapled"):
            self.assertIn(needed, state["run"])

    def test_only_the_signing_jobs_use_the_protected_environment(self):
        for name, job in self.jobs.items():
            if name != "desktop":
                self.assertNotIn("environment", job, name)
        self.assertEqual(
            self.jobs["desktop"]["environment"],
            "${{ needs.gate.outputs.signing == 'signed' && matrix.label != 'linux-x64' && 'release-signing' || '' }}",
        )
        steps = self.jobs["desktop"]["steps"]
        names = [s.get("name") for s in steps]
        notarize = names.index("Notarize and staple the disk images (signed runs)")
        self.assertLess(names.index("Build and bundle"), notarize)
        self.assertLess(notarize, names.index("Signature state of what ships"))
        (install,) = [s for s in steps if "install_smoke.py" in (s.get("run") or "")]
        self.assertIn("LIGHTAGENT_SIGNING_REQUIRED", install.get("env", {}))

    def test_licenses_are_complete_and_the_sources_checked(self):
        self.assertIn("scripts/desktop-upstream-licenses.py", self.steps_text("desktop-sources"))
        (step,) = [s for s in self.jobs["checksums"]["steps"] if s.get("name") == "Checksums"]
        for needed in ("sha256sum -c SHA256SUMS", "mpl-check"):
            self.assertIn(needed, step["run"])

    def test_smoke_checks_macos_architecture_and_minimum(self):
        smoke = (ROOT / "scripts" / "smoke-lightagent-package.sh").read_text(encoding="utf-8")
        for check in ("lipo -archs", "vtool -show-build", "MACOSX_DEPLOYMENT_TARGET"):
            self.assertIn(check, smoke)


if __name__ == "__main__":
    unittest.main()
