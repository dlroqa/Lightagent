#!/usr/bin/env python3
"""Build the blinded label-review package for the Jev benchmark.

Standard library only, and deterministic: the same dataset always yields the
same package. CI runs it and uploads the result; nothing here calls Jev or
reads any prediction.

Files written to the output directory:

  blind-worksheet.csv   For reviewers. Neutral review ids in a shuffled
                        order, the task text and capability fixture, and
                        empty label columns. No case ids (their prefixes
                        name categories), no groups, no author labels and no
                        model predictions.
  author-labels.csv     The current labels. Released to reviewers only after
                        every reviewer has submitted.
  review-key.csv        review id -> case id, group and split. For the
                        coordinator only.
  manifest.json         Dataset version, a SHA-256 of the dataset file, the
                        generating commit and the file list.

Usage: jev-review-package.py <benchmark.json> <output-dir>
"""

import csv
import hashlib
import json
import os
import pathlib
import sys

PERMITTED_PROFILE = "permitted"

LABEL_COLUMNS = [
    "primary_category",
    "acceptable_alternatives",
    "planning",
    "retrieval",
    "tools",
    "privacy",
    "refusal_reason",
    "label_confidence",
    "notes",
]


def yes_no(value):
    if value is None:
        return "either"
    return "yes" if value else "no"


def task_text(case):
    text = case["text"]
    repeat = case.get("repeat")
    if repeat:
        total = len(text) * repeat
        return (
            f"{text}\n[This text is repeated {repeat} times in the test input, "
            f"{total} characters in all.]"
        )
    return text


def main(argv):
    if len(argv) != 3:
        sys.stderr.write(__doc__)
        return 2
    source = pathlib.Path(argv[1])
    out = pathlib.Path(argv[2])
    raw = source.read_bytes()
    dataset = json.loads(raw)
    version = dataset["version"]
    capabilities = dataset["capabilities"]
    cases = dataset["cases"]

    # A stable shuffle keyed on the dataset version: the order hides the
    # groups, and the same version always gives the same review ids.
    def order(case):
        return hashlib.sha256(f"{version}:{case['id']}".encode()).hexdigest()

    shuffled = sorted(cases, key=order)
    review_ids = {case["id"]: f"R{index:03d}" for index, case in enumerate(shuffled, 1)}

    out.mkdir(parents=True, exist_ok=True)
    with open(out / "blind-worksheet.csv", "w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(
            ["review_id", "capability_fixture", "capability_description",
             "attachment", "profile", "task_text"] + LABEL_COLUMNS
        )
        for case in shuffled:
            fixture = case.get("capability", "full")
            profile = case.get("profile", PERMITTED_PROFILE)
            writer.writerow(
                [
                    review_ids[case["id"]],
                    fixture,
                    capabilities[fixture]["description"],
                    "yes" if case.get("attachment") else "no",
                    "permitted" if profile == PERMITTED_PROFILE
                    else "not permitted to send task descriptions",
                    task_text(case),
                ]
                + [""] * len(LABEL_COLUMNS)
            )

    with open(out / "author-labels.csv", "w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(
            ["review_id", "case_id", "primary_category", "acceptable_alternatives",
             "planning", "retrieval", "tools", "workflow", "privacy",
             "refusal_reason", "known_gaps", "notes"]
        )
        for case in shuffled:
            categories = case["categories"]
            writer.writerow(
                [
                    review_ids[case["id"]],
                    case["id"],
                    categories[0] if categories else "",
                    "|".join(categories[1:]),
                    yes_no(case["planning"]),
                    yes_no(case["retrieval"]),
                    yes_no(case["tools"]),
                    case.get("workflow") or "",
                    case.get("privacy", "send"),
                    case.get("fallback") or "",
                    "|".join(case.get("known_gaps", [])),
                    case.get("notes") or "",
                ]
            )

    with open(out / "review-key.csv", "w", newline="", encoding="utf-8") as handle:
        writer = csv.writer(handle)
        writer.writerow(["review_id", "case_id", "group", "split"])
        for case in shuffled:
            writer.writerow(
                [review_ids[case["id"]], case["id"], case["group"],
                 case.get("split", "benchmark")]
            )

    splits = {}
    for case in cases:
        split = case.get("split", "benchmark")
        splits[split] = splits.get(split, 0) + 1
    manifest = {
        "package": "jev-label-review",
        "dataset": dataset["dataset"],
        "dataset_version": version,
        "dataset_sha256": hashlib.sha256(raw).hexdigest(),
        "generated_from_commit": os.environ.get("GITHUB_SHA", "unknown"),
        "cases": len(cases),
        "splits": splits,
        "contains_model_predictions": False,
        "procedure": "evaluation/jev/review/README.md",
        "annotation_schema": "evaluation/jev/review/annotation.schema.json",
        "files": {
            "blind-worksheet.csv": "reviewers, phase 1",
            "author-labels.csv": "reviewers, phase 2 only, after every submission",
            "review-key.csv": "coordinator only",
        },
    }
    with open(out / "manifest.json", "w", encoding="utf-8") as handle:
        json.dump(manifest, handle, indent=2, sort_keys=True)
        handle.write("\n")

    if len(review_ids) != len(cases):
        sys.stderr.write("duplicate case ids\n")
        return 1
    print(f"wrote the review package for {dataset['dataset']} v{version}: {len(cases)} cases")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
