#!/usr/bin/env python3
"""Guards for jev-selection-pinned.yml: which commit may be evaluated, and
whether a finished evaluation is complete enough to report a result.

Standard library only. The workflow runs this file from its own commit, the
reviewed workflow source on master, never from the commit under test, and
never in the job that holds the Jev key.

  pin       Decide the commit under test. A pull-request run tests its own
            head. A manual dispatch, from master only, takes an explicit full
            commit id that is either the current head of an open pull request
            from this repository, or master's dispatch commit or one of its
            ancestors. A commit ahead of master or diverged from it is
            refused, and so is any API error or unexpected answer.

  verdict   The authoritative status of a dispatched evaluation. PASSED only
            when each of the 24 prompts was sent once and answered, the send
            record, the exported requests, the responses and the harness
            report all agree, and the pass rule, recomputed here, holds. A
            failed, missing, duplicate or malformed response, a stopped or
            partial run, or records that disagree make the run INCOMPLETE,
            which fails. An incomplete run's ratios are kept apart, as
            diagnostics only. Without a credential the run is NOT_EVALUATED.

Usage:
  jev-selection-guard.py pin --event E --ref R --sha S --workflow-sha W
      --pr-sha P --repo OWNER/NAME [--output FILE] [--summary FILE]
  jev-selection-guard.py verdict --set FILE --requests DIR --responses DIR
      --reports DIR --digest D --code-sha S --workflow-sha W --out DIR
      [--output FILE] [--summary FILE]
"""

import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys

SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"[0-9a-f]{64}")
PROMPT_ID = re.compile(r"sel-[0-9]{3}")
EXPECTED_PROMPTS = 24
MODEL_FIELDS = ("model", "model_id", "model_version", "version")


class Refused(Exception):
    pass


class ApiError(Exception):
    pass


def field(value, *keys):
    """`value[k1][k2]...`, or None when any step is missing or not an object."""
    for key in keys:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value


def gh_api(path):
    try:
        done = subprocess.run(
            ["gh", "api", path], capture_output=True, text=True, timeout=60
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ApiError(f"{path}: {error}") from error
    if done.returncode != 0:
        raise ApiError(f"{path}: gh api exited {done.returncode}: {done.stderr.strip()[:300]}")
    try:
        return json.loads(done.stdout)
    except ValueError as error:
        raise ApiError(f"{path}: the answer is not JSON") from error


def pin(event, ref, sha, workflow_sha, pr_sha, repo, api=gh_api):
    """The commit to evaluate, and why it may be; raises Refused otherwise."""
    if event != "workflow_dispatch":
        if not SHA.fullmatch(pr_sha or ""):
            raise Refused("the pull request head is not a full commit id")
        return {"sha": pr_sha, "workflow_sha": workflow_sha, "pinned_by": "pull_request"}
    if ref != "refs/heads/master":
        raise Refused("Dispatch from master only: the workflow definition must be the reviewed one.")
    if not SHA.fullmatch(sha or ""):
        raise Refused("sha must be a full 40-character commit id")
    if not SHA.fullmatch(workflow_sha or ""):
        raise Refused("the dispatch commit is not a full commit id")

    # The current head of an open pull request from this repository.
    try:
        pulls = api(f"repos/{repo}/commits/{sha}/pulls")
    except ApiError as error:
        raise Refused(f"cannot list the pull requests of {sha}: {error}") from error
    if not isinstance(pulls, list):
        raise Refused(f"unexpected answer listing the pull requests of {sha}")
    open_prs = sorted(
        pr["number"]
        for pr in pulls
        if field(pr, "state") == "open"
        and field(pr, "head", "sha") == sha
        and field(pr, "head", "repo", "full_name") == repo
        and isinstance(field(pr, "number"), int)
    )
    if open_prs:
        return {
            "sha": sha,
            "workflow_sha": workflow_sha,
            "pinned_by": "open_pull_request",
            "pull_requests": open_prs,
        }

    # Or master's dispatch commit, or one of its ancestors: comparing the
    # commit (base) with the dispatch commit (head) must find the dispatch
    # commit identical or strictly ahead, with the commit itself as the
    # merge base.
    try:
        compare = api(f"repos/{repo}/compare/{sha}...{workflow_sha}")
    except ApiError as error:
        raise Refused(
            f"{sha} is not the head of an open pull request from this repository, "
            f"and its ancestry on master could not be checked: {error}"
        ) from error
    status = field(compare, "status")
    if status == "behind":
        raise Refused(f"{sha} is ahead of master's dispatch commit {workflow_sha}")
    if status == "diverged":
        raise Refused(f"{sha} has diverged from master's dispatch commit {workflow_sha}")
    if status not in ("identical", "ahead"):
        raise Refused(f"unexpected comparison status {status!r} for {sha}")
    if field(compare, "base_commit", "sha") != sha or field(compare, "merge_base_commit", "sha") != sha:
        raise Refused(f"the comparison does not show {sha} as an ancestor of {workflow_sha}")
    ahead, behind = field(compare, "ahead_by"), field(compare, "behind_by")
    if behind != 0 or not isinstance(ahead, int) or (ahead == 0) != (status == "identical"):
        raise Refused(f"inconsistent comparison counts for {sha}: ahead {ahead!r}, behind {behind!r}")
    if status == "identical" and sha != workflow_sha:
        raise Refused(f"the comparison calls {sha} identical to {workflow_sha}")
    return {
        "sha": sha,
        "workflow_sha": workflow_sha,
        "pinned_by": "master" if status == "identical" else "master_ancestor",
        "ahead_by": ahead,
    }


def request_digest(directory, names):
    """The digest `sha256sum sel-*.json | sha256sum` gives in `directory`."""
    lines = "".join(
        f"{hashlib.sha256((directory / name).read_bytes()).hexdigest()}  {name}\n"
        for name in sorted(names)
    )
    return hashlib.sha256(lines.encode()).hexdigest()


def read_json(path, problems, what):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        problems.append(f"{what} is missing")
    except (OSError, ValueError) as error:
        problems.append(f"{what} is not readable JSON: {error}")
    return None


def files_in(directory):
    if not directory.is_dir():
        return None
    return sorted(path.name for path in directory.iterdir())


def ratio(numerator, denominator):
    return {
        "numerator": numerator,
        "denominator": denominator,
        "value": numerator / denominator if denominator else None,
    }


def measure(cases, labels, pass_rule):
    """Ratios over the scored cases that have a label, as the harness defines them."""
    known = [case for case in cases if field(case, "id") in labels]
    lead_cases = sum(1 for delegate in labels.values() if not delegate)

    def planned(case):
        return field(case, "planned") is True

    decisions = sum(1 for case in known if planned(case) == labels[case["id"]])
    unnecessary = sum(1 for case in known if planned(case) and not labels[case["id"]])
    missed = sum(1 for case in known if not planned(case) and labels[case["id"]])
    roles = [field(case, "roles_ok") for case in known if field(case, "roles_ok") is not None]
    passed = (
        decisions / max(len(known), 1) >= pass_rule["decision_accuracy_min"]
        and unnecessary / max(lead_cases, 1) <= pass_rule["unnecessary_delegation_max"]
    )
    return {
        "cases": len(known),
        "decision_accuracy": ratio(decisions, len(known)),
        "unnecessary_delegation": ratio(unnecessary, lead_cases),
        "missed_delegation": ratio(missed, len(known) - min(lead_cases, len(known))),
        "role_match": ratio(sum(1 for ok in roles if ok is True), len(roles)),
        "meets_pass_rule": passed,
    }


def check_set(path, problems):
    """Prompt ids in order, their labels, and the pass rule."""
    selection = read_json(path, problems, "the prompt set")
    cases = field(selection, "cases")
    pass_rule = field(selection, "pass")
    if not isinstance(cases, list):
        if selection is not None:
            problems.append("the prompt set has no case list")
        return [], {}, None
    ids = [field(case, "id") for case in cases]
    labels = {}
    for case_id, case in zip(ids, cases):
        if not isinstance(case_id, str) or not PROMPT_ID.fullmatch(case_id):
            problems.append(f"prompt id {case_id!r} is not sel-NNN")
        elif case_id in labels:
            problems.append(f"duplicate prompt id {case_id}")
        elif not isinstance(field(case, "delegate"), bool):
            problems.append(f"{case_id} has no delegate label")
        else:
            labels[case_id] = case["delegate"]
    if len(cases) != EXPECTED_PROMPTS:
        problems.append(f"the prompt set has {len(cases)} prompts, expected {EXPECTED_PROMPTS}")
    rule_ok = isinstance(pass_rule, dict) and all(
        isinstance(pass_rule.get(key), (int, float)) and not isinstance(pass_rule.get(key), bool)
        for key in ("decision_accuracy_min", "unnecessary_delegation_max")
    )
    if not rule_ok:
        problems.append("the prompt set has no numeric pass rule")
        pass_rule = None
    return [case_id for case_id in ids if isinstance(case_id, str)], labels, pass_rule


def check_requests(directory, ids, digest, code_sha, problems):
    names = files_in(directory)
    if names is None:
        problems.append("the exported requests are missing")
        return
    expected = {f"{case_id}.json" for case_id in ids}
    request_names = [name for name in names if name != "manifest.json"]
    for name in sorted(set(request_names) - expected):
        problems.append(f"unexpected exported file {name}")
    for name in sorted(expected - set(request_names)):
        problems.append(f"no exported request {name}")
    if not DIGEST.fullmatch(digest or ""):
        problems.append("prepare published no request digest")
    elif set(request_names) == expected and request_digest(directory, request_names) != digest:
        problems.append("the exported requests do not match the digest prepare published")
    manifest = read_json(directory / "manifest.json", problems, "the request manifest")
    if manifest is not None:
        if field(manifest, "cases") != ids:
            problems.append("the request manifest does not list the prompt set's ids in order")
        if field(manifest, "commit") != code_sha:
            problems.append("the request manifest names another commit")


def check_send(responses, ids, problems):
    """Send's own record, and one successful response per prompt."""
    names = files_in(responses)
    if names is None:
        problems.append("the send record and responses are missing")
        return {}, None
    send = read_json(responses / "send.json", problems, "the send record")
    if send is not None:
        expected = {"sent": len(ids), "ceiling": EXPECTED_PROMPTS, "retries": 0, "stopped": "no"}
        for key, want in expected.items():
            if field(send, key) != want:
                problems.append(f"send record {key} is {field(send, key)!r}, expected {want!r}")
    expected_names = {f"{case_id}.json" for case_id in ids}
    for name in sorted(set(names) - expected_names - {"send.json"}):
        problems.append(f"unexpected response file {name}")
    models = set()
    for case_id in ids:
        path = responses / f"{case_id}.json"
        if not path.is_file():
            problems.append(f"no successful response for {case_id}")
            continue
        body = read_json(path, problems, f"the response to {case_id}")
        if body is None:
            continue
        if not isinstance(body, dict):
            problems.append(f"the response to {case_id} is not a JSON object")
            continue
        for key in MODEL_FIELDS:
            if isinstance(body.get(key), str):
                models.add(f"{key}={body[key]}")
    return send or {}, sorted(models)


def check_report(reports, ids, labels, code_sha, problems):
    report = read_json(reports / "specialist-selection.json", problems, "the harness report")
    if report is None:
        return None, []
    expected = {
        "commit": code_sha,
        "evaluated": True,
        "requests": len(ids),
        "request_ceiling": EXPECTED_PROMPTS,
        "retries": 0,
        "failures": {},
        "stopped": None,
    }
    for key, want in expected.items():
        if field(report, key) != want:
            problems.append(f"harness report {key} is {field(report, key)!r}, expected {want!r}")
    cases = field(report, "cases")
    if not isinstance(cases, list):
        problems.append("the harness report has no case list")
        return report, []
    case_ids = [field(case, "id") for case in cases]
    seen = set()
    for case_id in case_ids:
        if case_id in seen:
            problems.append(f"duplicate scored prompt id {case_id}")
        seen.add(case_id)
    if case_ids != ids:
        problems.append("the harness did not score each prompt exactly once, in order")
    for case in cases:
        case_id = field(case, "id")
        if case_id not in labels:
            continue
        planned, decision_ok, roles_ok = (field(case, k) for k in ("planned", "decision_ok", "roles_ok"))
        if not isinstance(planned, bool) or not isinstance(decision_ok, bool):
            problems.append(f"{case_id} is scored without a decision")
        elif decision_ok != (planned == labels[case_id]):
            problems.append(f"{case_id}'s decision disagrees with its label")
        elif not (roles_ok is None or isinstance(roles_ok, bool)) or (
            (roles_ok is not None) != (planned and labels[case_id])
        ):
            problems.append(f"{case_id}'s role match is recorded where it cannot apply")
    return report, cases


def verdict(set_path, requests, responses, reports, digest, code_sha, workflow_sha):
    problems = []
    status_record = read_json(responses / "status.json", [], "") if responses.is_dir() else None
    if field(status_record, "status") == "BLOCKED":
        if files_in(responses) == ["status.json"]:
            return {
                "status": "NOT_EVALUATED",
                "complete": False,
                "passed": False,
                "reason": field(status_record, "reason"),
            }
        problems.append("send reported BLOCKED but also recorded responses")

    ids, labels, pass_rule = check_set(set_path, problems)
    check_requests(requests, ids, digest, code_sha, problems)
    send, models = check_send(responses, ids, problems)
    report, cases = check_report(reports, ids, labels, code_sha, problems)

    measured = measure(cases, labels, pass_rule) if pass_rule else None
    if report is not None and measured is not None and not problems:
        totals = field(report, "totals")
        for key in ("decision_accuracy", "unnecessary_delegation", "missed_delegation", "role_match"):
            for part in ("numerator", "denominator"):
                if field(totals, key, part) != measured[key][part]:
                    problems.append(f"harness {key} {part} disagrees with the recomputed value")
        if field(totals, "cases") != measured["cases"] or field(totals, "passed") != measured["meets_pass_rule"]:
            problems.append("the harness totals disagree with the recomputed pass rule")

    complete = not problems
    passed = complete and measured is not None and measured["meets_pass_rule"]
    result = {
        "status": "PASSED" if passed else ("BELOW_THRESHOLD" if complete else "INCOMPLETE"),
        "complete": complete,
        "passed": passed,
        "problems": problems,
        "send": send,
        "model_identifiers": models,
        "usage": {
            key: field(report, key)
            for key in ("requests", "input_tokens", "output_tokens", "latency_ms", "failures", "stopped")
        }
        if report is not None
        else None,
    }
    # An incomplete run's ratios are not a result: keep them under their own
    # name, never beside the status, and without a pass-rule outcome.
    if not complete and measured is not None:
        measured.pop("meets_pass_rule")
    result["metrics" if complete else "diagnostic_metrics"] = measured
    return result


def fmt_ratio(value):
    if not isinstance(value, dict) or value.get("value") is None:
        return "n/a"
    return f"{value['value'] * 100:.1f}% ({value['numerator']}/{value['denominator']})"


def summary(result, code_sha, workflow_sha):
    status = result["status"]
    lines = [f"## Jev specialist selection: {status}", ""]
    lines.append(f"Workflow source (master): `{workflow_sha}`. Code under test: `{code_sha}`.")
    if status == "NOT_EVALUATED":
        lines.append("")
        lines.append(f"NOT EVALUATED: {result.get('reason') or 'no credential'}.")
        return "\n".join(lines) + "\n"
    if status == "INCOMPLETE":
        lines += ["", "The run is incomplete and is not a result. Problems:"]
        lines += [f"- {problem}" for problem in result["problems"]]
    models = result["model_identifiers"]
    lines += [
        "",
        f"Model identifier returned: {', '.join(models) if models else 'none in the responses'}.",
    ]
    metrics = result.get("metrics") or result.get("diagnostic_metrics")
    if metrics:
        title = "Measured" if result["complete"] else "Diagnostic only (incomplete run, not a result)"
        lines += [
            "",
            f"### {title}",
            "",
            "| Measure | Value |",
            "| --- | --- |",
            f"| Decision accuracy | {fmt_ratio(metrics['decision_accuracy'])} |",
            f"| Unnecessary delegation | {fmt_ratio(metrics['unnecessary_delegation'])} |",
            f"| Missed delegation | {fmt_ratio(metrics['missed_delegation'])} |",
            f"| Exact role-set match | {fmt_ratio(metrics['role_match'])} |",
        ]
    usage = result.get("usage")
    if usage:
        lines += [
            "",
            f"Requests {usage['requests']}; tokens in {usage['input_tokens']}, out {usage['output_tokens']}; "
            f"failures {json.dumps(usage['failures'])}; stopped: {usage['stopped'] or 'no'}.",
        ]
    lines += [
        "",
        "This measures Jev's selection on these 24 prompts only, not specialist competence or efficiency.",
    ]
    return "\n".join(lines) + "\n"


def append(path, text):
    if path:
        with open(path, "a", encoding="utf-8") as handle:
            handle.write(text)


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    pin_args = commands.add_parser("pin")
    for name in ("--event", "--ref", "--sha", "--workflow-sha", "--pr-sha", "--repo"):
        pin_args.add_argument(name, default="")
    verdict_args = commands.add_parser("verdict")
    for name in ("--set", "--requests", "--responses", "--reports", "--out"):
        verdict_args.add_argument(name, type=pathlib.Path, required=True)
    for name in ("--digest", "--code-sha", "--workflow-sha"):
        verdict_args.add_argument(name, default="")
    for command in (pin_args, verdict_args):
        command.add_argument("--output")
        command.add_argument("--summary")
    args = parser.parse_args(argv[1:])

    if args.command == "pin":
        try:
            pinned = pin(args.event, args.ref, args.sha, args.workflow_sha, args.pr_sha, args.repo)
        except Refused as error:
            print(f"Refused: {error}")
            return 1
        how = {
            "pull_request": "the pull request's own head",
            "open_pull_request": f"head of open pull request(s) {pinned.get('pull_requests')}",
            "master": "master's dispatch commit itself",
            "master_ancestor": f"on master, {pinned.get('ahead_by')} commit(s) before the dispatch commit",
        }[pinned["pinned_by"]]
        text = (
            f"Pinned `{pinned['sha']}` ({how}). "
            f"Workflow source: `{pinned['workflow_sha']}`. Code under test: `{pinned['sha']}`.\n"
        )
        print(text, end="")
        append(args.summary, text)
        append(
            args.output,
            f"sha={pinned['sha']}\nworkflow_sha={pinned['workflow_sha']}\npinned_by={pinned['pinned_by']}\n",
        )
        return 0

    result = verdict(
        args.set, args.requests, args.responses, args.reports, args.digest, args.code_sha, args.workflow_sha
    )
    record = {
        "schema": "lightagent.specialist_selection_verdict.v1",
        "evaluation": "jev_specialist_selection",
        "authoritative": True,
        "counts_as_competence_evidence": False,
        "workflow_sha": args.workflow_sha,
        "code_sha": args.code_sha,
        "request_digest": args.digest,
        "expected_prompts": EXPECTED_PROMPTS,
        **result,
    }
    text = summary(result, args.code_sha, args.workflow_sha)
    args.out.mkdir(parents=True, exist_ok=True)
    with open(args.out / "verdict.json", "w", encoding="utf-8") as handle:
        json.dump(record, handle, indent=2, sort_keys=True)
        handle.write("\n")
    (args.out / "verdict.md").write_text(text, encoding="utf-8")
    print(text, end="")
    append(args.summary, text)
    append(args.output, f"status={result['status']}\n")
    return 0 if result["status"] in ("PASSED", "NOT_EVALUATED") else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
