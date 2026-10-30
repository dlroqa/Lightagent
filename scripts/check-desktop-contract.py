#!/usr/bin/env python3
"""Keep the desktop operation contract equal to the server and the UI client.

docs/desktop/contract/operations.json is the allowlist the desktop host
enforces for every UI request (desktop/src-tauri/src/contract.rs embeds it).
It must:

  * list exactly the routes `lightagent_api::router` serves (method + path),
    so the host can never forward something the server lacks, and every
    server route is either reachable or deliberately added here;
  * cover every path the frontend client (frontend/src/api/agent.ts) calls.

The frontend check compares paths, not methods: the client builds requests
through helpers, and a wrong method is refused by the host at run time.
`--self-test` adds a server route and a client path in memory and requires
both to be reported.
"""

import importlib.util
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASE = "/api/lightagent/v1"


def load_inventory_module():
    path = os.path.join(ROOT, "scripts", "check-desktop-inventory.py")
    spec = importlib.util.spec_from_file_location("desktop_inventory", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def read(rel):
    with open(os.path.join(ROOT, rel), encoding="utf-8") as handle:
        return handle.read()


def shape(path):
    """A path with every parameter collapsed to `{}`, relative to the API base."""
    if path.startswith(BASE):
        path = path[len(BASE):]
    return re.sub(r"\{[^}]*\}", "{}", path)


def client_paths(text):
    text = text.replace("${BASE}", "")
    found = set()
    for literal in re.findall(r'[`"](/[^`"\s]*)[`"]', text):
        path = literal.split("?", 1)[0]
        path = re.sub(r"\$\{[^}]*\}", "{}", path)
        if path in ("", BASE):
            continue
        found.add(path)
    return found


def check(contract, server_text, client_text, scan_routes):
    errors = []
    ops = contract["operations"]
    ids = [op["id"] for op in ops]
    if len(ids) != len(set(ids)):
        errors.append("contract: duplicate operation id")
    for op in ops:
        if op["method"] not in ("GET", "POST", "PUT", "PATCH", "DELETE"):
            errors.append(f"contract: {op['id']} has method {op['method']!r}")
        if not op["path"].startswith("/"):
            errors.append(f"contract: {op['id']} path must start with '/'")
    listed = {f"{op['method']} {op['path']}" for op in ops}
    served = scan_routes(server_text)
    for item in sorted(served - listed):
        errors.append(f"server: {item} is served but not in the contract")
    for item in sorted(listed - served):
        errors.append(f"server: contract lists {item} but the server does not serve it")
    shapes = {shape(op["path"]) for op in ops}
    for path in sorted(client_paths(client_text) - shapes):
        errors.append(f"client: frontend calls {path} but no contract operation has that path")
    return errors


def main():
    inventory = load_inventory_module()
    with open(os.path.join(ROOT, "docs", "desktop", "contract", "operations.json"), encoding="utf-8") as handle:
        contract = json.load(handle)
    server = read("crates/lightagent-api/src/lib.rs")
    client = read("frontend/src/api/agent.ts")
    if "--self-test" in sys.argv:
        base = check(contract, server, client, inventory.scan_routes)
        if base:
            print("self-test needs a clean baseline:\n" + "\n".join(base))
            return 1
        added_route = server.replace('.route("/health", get(health))',
                                     '.route("/health", get(health))\n        .route("/api/lightagent/v1/memory", get(health))', 1)
        added_call = client + '\nconst probe = () => jsonRequest("/memory/search");\n'
        failures = 0
        for label, args, prefix in (("server route", (added_route, client), "server:"),
                                    ("client path", (server, added_call), "client:")):
            hit = [e for e in check(contract, args[0], args[1], inventory.scan_routes) if e.startswith(prefix)]
            print(f"self-test {label}: {'detected' if hit else 'MISSED'} {hit[:1]}")
            failures += 0 if hit else 1
        return 1 if failures else 0
    errors = check(contract, server, client, inventory.scan_routes)
    print(f"desktop operation contract: {len(contract['operations'])} operations, {len(errors)} errors")
    for error in errors:
        print(f"::error::{error}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
