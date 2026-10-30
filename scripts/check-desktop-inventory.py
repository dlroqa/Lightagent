#!/usr/bin/env python3
"""Keep the Lightagent Desktop capability inventory in step with the source.

docs/desktop/desktop-capabilities.json lists every user-facing capability the
desktop must map. The CLI command tree is pinned in docs/desktop/cli-tree.txt
and compared with clap itself by the `desktop_inventory` test in
crates/lightagent/src/lib.rs. This script checks everything else statically:

  * the inventory's own shape (unique ids, known enums, resolvable duplicates);
  * every cli-tree.txt line has exactly one inventory row, and no row names a
    line that is not in the tree;
  * slash-command words, chat key codes, ACP methods, MCP transports and their
    fields, extension manifest fields, config sections, HTTP routes, and
    literal LIGHTAGENT_* environment names found in the source all have rows,
    and no row names one the source no longer has;
  * DESKTOP_CLI_PARITY.md lists every row and states the current denominator.

What it cannot see is listed in DESKTOP_CLI_PARITY.md ("What the drift check
cannot detect automatically"). `--self-test` adds one item to each scanned
surface in memory and requires the check to fail for each.
"""

import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = os.path.join(ROOT, "docs", "desktop")
SOURCES = {
    "slash": "crates/lightagent/src/slash.rs",
    "chat": "crates/lightagent/src/chat.rs",
    "acp": "crates/lightagent-acp/src/server.rs",
    "config": "crates/lightagent-core/src/config.rs",
    "extensions": "crates/lightagent-extensions/src/lib.rs",
    "api": "crates/lightagent-api/src/lib.rs",
    "provisioning": "crates/lightagent-api/src/provisioning.rs",
}
KINDS = {"command", "option", "argument", "value", "alias", "setup-option", "slash", "keybinding", "acp-method",
         "mcp-registration", "extension-registration", "skill-registration", "config", "env", "api"}
APPLICABILITY = {"applicable", "not-applicable", "host-owned", "decision-D1"}
STATUS = {"not-started", "implemented", "unsupported", "deferred", "not-applicable"}
REQUIRED = ("id", "kind", "surface", "source", "behavior", "applicability", "counts_toward_parity", "desktop_status")


def read(rel):
    with open(os.path.join(ROOT, rel), encoding="utf-8") as handle:
        return handle.read()


def before_tests(text):
    """The text before the file's top-level `#[cfg(test)] mod …` module.

    A single item can carry `#[cfg(test)]` anywhere in a file, so only the
    attribute at column 0 directly above a `mod` marks where tests begin.
    """
    return re.split(r"^#\[cfg\(test\)\]\s*\n(?:pub(?:\([^)]*\))? )?mod ", text, maxsplit=1, flags=re.M)[0]


def block(text, start):
    """The brace-balanced block opening at the first `{` after `start`."""
    at = text.index(start)
    open_at = text.index("{", at)
    depth = 0
    for index in range(open_at, len(text)):
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
            if depth == 0:
                return text[open_at + 1:index]
    raise ValueError(f"unbalanced block after {start!r}")


def scan_slash(text):
    body = block(text, "pub fn parse(")
    words = set()
    for head in re.findall(r'^\s*((?:"[^"]*"\s*\|\s*)*"[^"]*")\s*=>', body, re.M):
        words.update(re.findall(r'"([^"]*)"', head))
    sub = set(re.findall(r'args == "(\w+)"', body)) | set(re.findall(r'strip_prefix\("(\w+) "\)', body))
    return words | sub


def scan_keys(text):
    code = before_tests(text)
    keys = set()
    for name, char in re.findall(r"KeyCode::(\w+)(?:\('(.)'\))?", code):
        keys.add(f"Char('{char}')" if char else name)
    return keys


def scan_acp(text):
    code = before_tests(text)
    # Dispatch arms open a block; value mappings (`"user" => "…"`) do not.
    methods = set(re.findall(r'^\s*"([a-z_]+(?:/[a-z_]+)?)"\s*=>\s*\{', code, re.M))
    methods |= set(re.findall(r'"(session/[a-z_]+)"', code))
    return methods


def scan_mcp(text):
    body = block(text, "pub enum McpServerEntry")
    found = set()
    for variant, fields in re.findall(r"^\s{4}(\w+)\s*\{(.*?)^\s{4}\}", body, re.M | re.S):
        lower = variant.lower()
        found.add(lower)
        for field in re.findall(r"^\s{8}(\w+):", fields, re.M):
            if field != "name":
                found.add(f"{lower}.{field}")
    return found


def scan_extension(text):
    body = block(text, "pub struct Extension ")
    return {f for f in re.findall(r"^\s*pub (\w+):", body, re.M) if f != "dir"}


def scan_config(text):
    body = block(text, "pub struct Config ")
    return {f for f in re.findall(r"^\s*pub (\w+):", body, re.M) if f != "unknown"}


def scan_routes(text):
    found = set()
    for path, handler in re.findall(r'\.route\(\s*"([^"]+)",(.*?)\)\s*(?=\.route|\.fallback|\.layer|\.with_state)', text, re.S):
        for method in re.findall(r"\b(get|post|put|patch|delete)\(", handler):
            found.add(f"{method.upper()} {path}")
    return found


def scan_env(texts, excluded):
    names = set()
    for text in texts:
        names |= set(re.findall(r'"((?:LIGHTAGENT|HERMES)_[A-Z0-9_]+)"', text))
    def skip(name):
        return any(name == p or (p.endswith("*") and name.startswith(p[:-1])) for p in excluded)
    return {n for n in names if not skip(n)}


def rust_files():
    for base, _, files in os.walk(os.path.join(ROOT, "crates")):
        for name in files:
            if name.endswith(".rs"):
                with open(os.path.join(base, name), encoding="utf-8") as handle:
                    yield handle.read()


def compare(errors, label, found, listed):
    for item in sorted(found - listed):
        errors.append(f"{label}: source has {item!r} but no inventory row lists it")
    for item in sorted(listed - found):
        errors.append(f"{label}: inventory lists {item!r} but the source no longer has it")


def tokens(rows, kind):
    out = set()
    for row in rows:
        if row["kind"] == kind:
            out.update(row.get("tokens") or [])
    return out


def check(inventory, tree_lines, parity_md, sources, env_texts):
    errors = []
    rows = inventory["rows"]
    ids = [row.get("id") for row in rows]
    seen = set()
    for row in rows:
        for key in REQUIRED:
            if key not in row:
                errors.append(f"row {row.get('id')!r}: missing {key}")
        if row.get("id") in seen:
            errors.append(f"duplicate id {row.get('id')!r}")
        seen.add(row.get("id"))
        if row.get("kind") not in KINDS:
            errors.append(f"row {row.get('id')!r}: unknown kind {row.get('kind')!r}")
        if row.get("applicability") not in APPLICABILITY:
            errors.append(f"row {row.get('id')!r}: unknown applicability {row.get('applicability')!r}")
        if row.get("desktop_status") not in STATUS:
            errors.append(f"row {row.get('id')!r}: unknown status {row.get('desktop_status')!r}")
        if row.get("applicability") == "decision-D1" and not row.get("decision"):
            errors.append(f"row {row.get('id')!r}: a pending decision row must name its decision")
        if row.get("applicability") in ("not-applicable", "host-owned") and not (row.get("reason") or row.get("duplicate_of")):
            errors.append(f"row {row.get('id')!r}: {row['applicability']} needs a reason")
        if row.get("duplicate_of") and row["duplicate_of"] not in ids:
            errors.append(f"row {row.get('id')!r}: duplicate_of {row['duplicate_of']!r} is not a row")
        if row.get("desktop_status") == "implemented" and not (row.get("tested_sha") and row.get("actions_evidence")):
            errors.append(f"row {row.get('id')!r}: implemented without tested_sha and actions_evidence")
    decision_ids = {d["id"] for d in inventory.get("decisions", [])}
    for row in rows:
        if row.get("decision") and row["decision"] not in decision_ids:
            errors.append(f"row {row['id']!r}: decision {row['decision']!r} is not declared")

    keyed = [row["tree_key"] for row in rows if row.get("tree_key")]
    if len(keyed) != len(set(keyed)):
        errors.append("two rows share one cli-tree line")
    compare(errors, "cli-tree", set(tree_lines), set(keyed))

    compare(errors, "slash", scan_slash(sources["slash"]), tokens(rows, "slash"))
    compare(errors, "chat keys", scan_keys(sources["chat"]), tokens(rows, "keybinding"))
    compare(errors, "acp", scan_acp(sources["acp"]), tokens(rows, "acp-method"))
    compare(errors, "mcp", scan_mcp(sources["config"]), tokens(rows, "mcp-registration"))
    compare(errors, "extension manifest", scan_extension(sources["extensions"]), tokens(rows, "extension-registration"))
    compare(errors, "config sections", scan_config(sources["config"]), tokens(rows, "config"))
    routes = scan_routes(sources["api"]) | scan_routes(sources["provisioning"])
    compare(errors, "http routes", routes, tokens(rows, "api"))
    compare(errors, "env", scan_env(env_texts, inventory.get("excluded_env", [])), tokens(rows, "env"))

    counted = [r for r in rows if r.get("counts_toward_parity") and r.get("applicability") == "applicable"]
    implemented = sum(1 for r in counted if r.get("desktop_status") == "implemented")
    summary = inventory.get("summary", {})
    if summary.get("parity_denominator") != len(counted):
        errors.append(f"summary.parity_denominator is {summary.get('parity_denominator')}, rows give {len(counted)}")
    if summary.get("parity_implemented") != implemented:
        errors.append(f"summary.parity_implemented is {summary.get('parity_implemented')}, rows give {implemented}")
    if f"- Denominator: {len(counted)}" not in parity_md:
        errors.append(f"DESKTOP_CLI_PARITY.md does not state '- Denominator: {len(counted)}'")
    for row_id in ids:
        if f"`{row_id}`" not in parity_md:
            errors.append(f"DESKTOP_CLI_PARITY.md does not list `{row_id}`")
    return errors, len(counted), implemented


def load():
    with open(os.path.join(DOCS, "desktop-capabilities.json"), encoding="utf-8") as handle:
        inventory = json.load(handle)
    with open(os.path.join(DOCS, "cli-tree.txt"), encoding="utf-8") as handle:
        tree = [line for line in handle.read().splitlines() if line]
    with open(os.path.join(DOCS, "DESKTOP_CLI_PARITY.md"), encoding="utf-8") as handle:
        parity = handle.read()
    sources = {key: read(rel) for key, rel in SOURCES.items()}
    return inventory, tree, parity, sources, list(rust_files())


def self_test():
    inventory, tree, parity, sources, env_texts = load()
    base_errors, _, _ = check(inventory, tree, parity, sources, env_texts)
    if base_errors:
        print("self-test needs a clean baseline; the check itself fails:")
        print("\n".join(base_errors))
        return 1
    mutations = {
        "slash": lambda s: {**s, "slash": s["slash"].replace('"new" => Slash::New,', '"new" => Slash::New,\n        "frobnicate" => Slash::New,')},
        "chat keys": lambda s: {**s, "chat": s["chat"].replace("KeyCode::Home =>", "KeyCode::Tab => {}\n            KeyCode::Home =>", 1)},
        "acp": lambda s: {**s, "acp": s["acp"].replace('"session/cancel" => {', '"session/fork" => {}\n            "session/cancel" => {', 1)},
        "mcp": lambda s: {**s, "config": s["config"].replace("        url: String,\n", "        url: String,\n        proxy: String,\n", 1)},
        "extension manifest": lambda s: {**s, "extensions": s["extensions"].replace("    pub version: String,", "    pub version: String,\n    pub homepage: String,", 1)},
        "config sections": lambda s: {**s, "config": s["config"].replace("    pub inference: InferenceConfig,", "    pub inference: InferenceConfig,\n    pub telemetry: TelemetryConfig,", 1)},
        "http routes": lambda s: {**s, "api": s["api"].replace('.route("/health", get(health))', '.route("/health", get(health))\n        .route("/api/lightagent/v1/memory", get(health))', 1)},
    }
    failures = 0
    for label, mutate in mutations.items():
        changed = mutate(sources)
        if changed == sources:
            print(f"self-test: could not apply the {label} mutation (anchor moved)")
            failures += 1
            continue
        errors, _, _ = check(inventory, tree, parity, changed, env_texts)
        hit = [e for e in errors if e.startswith(label + ":")]
        print(f"self-test {label}: {'detected' if hit else 'MISSED'} {hit[:1]}")
        failures += 0 if hit else 1
    errors, _, _ = check(inventory, tree, parity, sources, env_texts + ['"LIGHTAGENT_NEW_KNOB"'])
    hit = [e for e in errors if e.startswith("env:")]
    print(f"self-test env: {'detected' if hit else 'MISSED'} {hit[:1]}")
    failures += 0 if hit else 1
    errors, _, _ = check(inventory, tree + ["cmd lightagent frobnicate"], parity, sources, env_texts)
    hit = [e for e in errors if e.startswith("cli-tree:")]
    print(f"self-test cli-tree: {'detected' if hit else 'MISSED'} {hit[:1]}")
    failures += 0 if hit else 1
    return 1 if failures else 0


def main():
    if "--self-test" in sys.argv:
        return self_test()
    inventory, tree, parity, sources, env_texts = load()
    errors, denominator, implemented = check(inventory, tree, parity, sources, env_texts)
    report = [
        "### Desktop capability inventory",
        f"- frozen at: `{inventory['frozen_at']['sha']}`",
        f"- rows: {len(inventory['rows'])}",
        f"- parity: {implemented} of {denominator} implemented",
        f"- pending D1 decisions: {', '.join(d['id'] for d in inventory.get('decisions', [])) or 'none'}",
        f"- drift errors: {len(errors)}",
    ]
    print("\n".join(report))
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as handle:
            handle.write("\n".join(report) + "\n")
    for error in errors:
        print(f"::error::{error}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
