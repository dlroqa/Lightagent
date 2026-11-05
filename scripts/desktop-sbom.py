"""The desktop's SBOM, license inventory and audit report (D9).

  python desktop-sbom.py <metadata-dir> <frontend-dir> <out-dir>
      <metadata-dir> holds `cargo metadata --filter-platform <triple>`
      output for both workspaces: root-<triple>.json (the bundled runtime,
      the `lightagent` binary) and desktop-<triple>.json (the host,
      `lightagent-desktop`). Per target it writes sbom-<triple>.cdx.json
      (CycloneDX 1.5) and licenses-<triple>.csv, listing what ships: the
      crates each binary links through normal dependencies for that
      target, and the bundled UI's production npm packages. Prints a
      Markdown summary, with components whose license is unknown and
      copyleft licenses listed for review.
  python desktop-sbom.py --cargo-audit <audit.json> <lockfile>
  python desktop-sbom.py --npm-audit <audit.json>
      Print an audit's findings; exit 1 when it holds a vulnerability (for
      npm: of high or critical severity), or, for cargo, a warning that is
      not among REVIEWED_WARNINGS (docs/desktop/DESKTOP_ADVISORIES.md).
  python desktop-sbom.py --notices <metadata-dir> <triple> <frontend-dir> <out-dir> <repo-root>
      Write THIRD_PARTY_NOTICES.txt (every third-party component of the
      package for <triple>, its license and the license files it is
      published with, and where the MPL-2.0 components' source is) and
      LICENSE.txt into <out-dir>; exit 1 when an MPL-2.0 component is not a
      checksummed crates.io package, has no license text, or crates are
      patched.
  python desktop-sbom.py --mpl-sources <metadata-dir> <out-dir> <repo-root>
      Copy the exact published archive of each MPL-2.0 crate any target
      links, checked against Cargo.lock's checksum, into <out-dir> with
      SHA256SUMS.
  python desktop-sbom.py --advisory-paths <metadata-dir>
      For each reviewed advisory: where its crate is reached, whether it is
      in the binary or used at build time only, and the call sites of the
      affected code in every crate the binary is built from.
"""

import csv
import datetime
import json
import os
import glob
import hashlib
import re
import shutil
import sys
import uuid

COPYLEFT = re.compile(r"\b(A?GPL|LGPL|MPL|EPL|CDDL|EUPL|OSL|SSPL)", re.I)
TARGETS = ["x86_64-apple-darwin", "aarch64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
LICENSE_NAME = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE|COPYRIGHT|UNLICENSE)([-_.].*)?$", re.I)
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
MPL = re.compile(r"\bMPL-2\.0\b")
# Advisory warnings reviewed in docs/desktop/DESKTOP_ADVISORIES.md. Any other
# warning fails the audit step, as a vulnerability does.
REVIEWED_WARNINGS = {
    "RUSTSEC-2024-0370": "reviewed: build time only (procedural macros of the GTK 3 bindings, Linux); no maintained release to move to",
    "RUSTSEC-2024-0429": "reviewed: fixed only in glib 0.20, which the GTK 3 bindings Tauri 2 uses on Linux cannot take; call sites: --advisory-paths",
}
# Each advisory's crate, and what a call of its affected code looks like.
ADVISORY_CODE = [
    ("RUSTSEC-2024-0429", "glib", re.compile(r"\b(VariantStrIter|array_iter_str)\b")),
    ("RUSTSEC-2024-0370", "proc-macro-error", None),
    ("RUSTSEC-2026-0386", "tokio", re.compile(r"\bAsyncFd\s*::\s*(new|with_interest|try_new|try_with_interest)\b")),
]


def load(path):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def linked(metadata, root_name):
    """Packages reachable from the workspace package `root_name` through
    normal (not dev, not build) dependencies."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    stack = [package["id"] for package in metadata["packages"] if package["name"] == root_name and package.get("source") is None]
    seen = set()
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes.get(current, {}).get("deps", []):
            if any(kind.get("kind") is None for kind in dep.get("dep_kinds", [])):
                stack.append(dep["pkg"])
    return [packages[key] for key in sorted(seen) if key in packages]


def crate_component(package, ships_in):
    license_ = package.get("license") or (f"see {os.path.basename(package['license_file'])}" if package.get("license_file") else None)
    first_party = package.get("source") is None
    return {
        "ecosystem": "cargo", "name": package["name"], "version": package["version"], "license": license_,
        "purl": f"pkg:cargo/{package['name']}@{package['version']}", "ships_in": ships_in, "first_party": first_party,
    }


def npm_components(frontend):
    lock = load(os.path.join(frontend, "package-lock.json"))
    found = []
    for path, entry in sorted(lock.get("packages", {}).items()):
        if not path or entry.get("dev"):
            continue
        name = entry.get("name") or path.split("node_modules/")[-1]
        license_ = entry.get("license")
        if not license_:
            try:
                manifest = load(os.path.join(frontend, path, "package.json"))
                license_ = manifest.get("license")
                if isinstance(license_, dict):
                    license_ = license_.get("type")
            except (OSError, ValueError):
                license_ = None
        purl_name = name.replace("@", "%40", 1) if name.startswith("@") else name
        found.append({"ecosystem": "npm", "name": name, "version": entry.get("version"), "license": license_,
                      "purl": f"pkg:npm/{purl_name}@{entry.get('version')}", "ships_in": "bundled UI", "first_party": False})
    return found


def cyclonedx(target, version, components):
    return {
        "bomFormat": "CycloneDX", "specVersion": "1.5", "serialNumber": f"urn:uuid:{uuid.uuid4()}", "version": 1,
        "metadata": {
            "timestamp": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "component": {"type": "application", "name": "lightagent-desktop", "version": version, "bom-ref": "lightagent-desktop"},
            "properties": [{"name": "target", "value": target}],
        },
        "components": [
            {
                "type": "application" if component["first_party"] else "library",
                "bom-ref": f"{component['purl']}#{component['ships_in']}",
                "name": component["name"], "version": component["version"], "purl": component["purl"],
                "licenses": [{"expression": component["license"]}] if component["license"] and not component["license"].startswith("see ") else [],
                "properties": [{"name": "ships-in", "value": component["ships_in"]}]
                + ([{"name": "license-file", "value": component["license"][4:]}] if (component["license"] or "").startswith("see ") else []),
            }
            for component in components
        ],
    }


def inventory(metadata_dir, frontend, out):
    os.makedirs(out, exist_ok=True)
    ui = npm_components(frontend)
    lines = ["### desktop supply chain: SBOM and licenses"]
    review = {}
    for target in TARGETS:
        runtime = load(os.path.join(metadata_dir, f"root-{target}.json"))
        host = load(os.path.join(metadata_dir, f"desktop-{target}.json"))
        components = [crate_component(p, "desktop host") for p in linked(host, "lightagent-desktop")]
        components += [crate_component(p, "bundled runtime") for p in linked(runtime, "lightagent")]
        components += ui
        version = next((p["version"] for p in host["packages"] if p["name"] == "lightagent-desktop"), "?")
        with open(os.path.join(out, f"sbom-{target}.cdx.json"), "w", encoding="utf-8") as handle:
            json.dump(cyclonedx(target, version, components), handle, indent=1)
        with open(os.path.join(out, f"licenses-{target}.csv"), "w", encoding="utf-8", newline="") as handle:
            writer = csv.writer(handle)
            writer.writerow(["ecosystem", "name", "version", "license", "ships_in", "first_party"])
            for component in sorted(components, key=lambda c: (c["ships_in"], c["ecosystem"], c["name"], c["version"])):
                writer.writerow([component[k] for k in ("ecosystem", "name", "version", "license", "ships_in", "first_party")])
        third = [c for c in components if not c["first_party"]]
        unknown = sorted({f"{c['name']}@{c['version']}" for c in third if not c["license"]})
        copyleft = sorted({f"{c['name']}@{c['version']} ({c['license']})" for c in third if c["license"] and COPYLEFT.search(c["license"])})
        counts = {where: len({(c["name"], c["version"]) for c in components if c["ships_in"] == where})
                  for where in ("desktop host", "bundled runtime", "bundled UI")}
        lines.append(f"- `{target}`: {counts}; third-party licenses unknown: {len(unknown)}; with a copyleft option: {len(copyleft)}")
        review[target] = {"unknown": unknown, "copyleft": copyleft}
    for target, found in review.items():
        if found["unknown"]:
            lines.append(f"  - {target} unknown: {', '.join(found['unknown'][:40])}")
        if found["copyleft"]:
            lines.append(f"  - {target} copyleft option, review: {', '.join(found['copyleft'][:40])}")
    with open(os.path.join(out, "license-review.json"), "w", encoding="utf-8") as handle:
        json.dump(review, handle, indent=1)
    print("\n".join(lines))
    return 0


def cargo_audit(path, lockfile):
    report = load(path)
    found = (report.get("vulnerabilities") or {}).get("list") or []
    lines = [f"### cargo audit `{lockfile}`: {len(found)} vulnerabilit{'y' if len(found) == 1 else 'ies'}"]
    for item in found:
        advisory, package = item.get("advisory") or {}, item.get("package") or {}
        lines.append(f"- {advisory.get('id')} {package.get('name')}@{package.get('version')}: {advisory.get('title')} "
                     f"(patched: {', '.join((item.get('versions') or {}).get('patched') or []) or 'none'})")
    unreviewed = []
    for kind, items in (report.get("warnings") or {}).items():
        for item in items or []:
            advisory, package = item.get("advisory") or {}, item.get("package") or {}
            ident = advisory.get("id", "")
            review = REVIEWED_WARNINGS.get(ident)
            if not review:
                unreviewed.append(f"{ident or kind} {package.get('name')}@{package.get('version')}")
            lines.append(f"- warning ({kind}) {ident} {package.get('name')}@{package.get('version')}: {advisory.get('title', '')}"
                         f" ({review or 'NOT REVIEWED: fails this step'})")
    print("\n".join(lines))
    return 1 if found or unreviewed else 0


def npm_audit(path):
    report = load(path)
    counts = (report.get("metadata") or {}).get("vulnerabilities") or {}
    lines = [f"### npm audit (bundled UI, production dependencies): {json.dumps(counts)}"]
    for name, item in sorted((report.get("vulnerabilities") or {}).items()):
        lines.append(f"- {name}: {item.get('severity')} ({item.get('range', '')})")
    print("\n".join(lines))
    return 1 if (counts.get("high", 0) or 0) + (counts.get("critical", 0) or 0) else 0


def sha256_file(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def is_proc_macro(package):
    return any("proc-macro" in target.get("kind", []) for target in package.get("targets", []))


def runtime_linked(metadata, root_name):
    """The ids of the packages in the binary itself: `linked`, without
    procedural macros and what only they depend on (they run at build time)."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    stack = [p["id"] for p in metadata["packages"] if p["name"] == root_name and p.get("source") is None]
    seen = set()
    while stack:
        current = stack.pop()
        if current in seen or (current in packages and is_proc_macro(packages[current])):
            continue
        seen.add(current)
        for dep in nodes.get(current, {}).get("deps", []):
            if any(kind.get("kind") is None for kind in dep.get("dep_kinds", [])):
                stack.append(dep["pkg"])
    return seen


def lock_checksums(root):
    """(name, version) -> checksum, from both workspaces' Cargo.lock."""
    sums = {}
    for name in ("Cargo.lock", os.path.join("desktop", "Cargo.lock")):
        path = os.path.join(root, name)
        if not os.path.exists(path):
            continue
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        for block in text.split("[[package]]"):
            fields = dict(re.findall(r'^(name|version|checksum) = "([^"]*)"$', block, re.M))
            if fields.get("checksum"):
                sums[(fields.get("name"), fields.get("version"))] = fields["checksum"]
    return sums


def patched(root):
    """Manifests or Cargo configuration that patch or replace crates."""
    found = []
    for name in ("Cargo.toml", os.path.join("desktop", "Cargo.toml"), os.path.join(".cargo", "config.toml"),
                 os.path.join("desktop", ".cargo", "config.toml")):
        path = os.path.join(root, name)
        if os.path.exists(path):
            with open(path, encoding="utf-8") as handle:
                if re.search(r"^\s*\[(patch|replace)\b", handle.read(), re.M):
                    found.append(name)
    return found


def license_texts(directory, license_file=None):
    names = []
    if directory and os.path.isdir(directory):
        names = sorted(n for n in os.listdir(directory) if LICENSE_NAME.match(n) and os.path.isfile(os.path.join(directory, n)))
        if license_file:
            extra = os.path.relpath(license_file if os.path.isabs(license_file) else os.path.join(directory, license_file), directory)
            if extra not in names and os.path.isfile(os.path.join(directory, extra)):
                names.append(extra)
    texts = []
    for name in names:
        try:
            with open(os.path.join(directory, name), encoding="utf-8", errors="replace") as handle:
                texts.append((name, handle.read().strip("\n")))
        except OSError:
            pass
    return texts


def shipped(metadata_dir, target, frontend):
    """Every third-party component of the package for `target`."""
    components = {}
    for workspace, binary, where in (("desktop", "lightagent-desktop", "desktop host"), ("root", "lightagent", "bundled runtime")):
        metadata = load(os.path.join(metadata_dir, f"{workspace}-{target}.json"))
        in_binary = runtime_linked(metadata, binary)
        for package in linked(metadata, binary):
            if package.get("source") is None:
                continue
            entry = components.setdefault(("cargo", package["name"], package["version"]), {
                "ecosystem": "cargo", "name": package["name"], "version": package["version"],
                "license": package.get("license") or "see its license file", "repository": package.get("repository") or "",
                "source": package.get("source"), "directory": os.path.dirname(package["manifest_path"]),
                "license_file": package.get("license_file"), "ships_in": set(), "build_time_only": True})
            entry["ships_in"].add(where)
            if package["id"] in in_binary:
                entry["build_time_only"] = False
    for component in npm_components(frontend):
        components.setdefault(("npm", component["name"], component["version"]), {
            "ecosystem": "npm", "name": component["name"], "version": component["version"],
            "license": component["license"] or "see its license file", "repository": "", "source": "npm",
            "directory": os.path.join(frontend, "node_modules", *component["name"].split("/")),
            "license_file": None, "ships_in": {"bundled UI"}, "build_time_only": False})
    return components


def notices(metadata_dir, target, frontend, out, root):
    """THIRD_PARTY_NOTICES.txt and LICENSE.txt for the package built for `target`."""
    os.makedirs(out, exist_ok=True)
    sums = lock_checksums(root)
    host = load(os.path.join(metadata_dir, f"desktop-{target}.json"))
    version = next((p["version"] for p in host["packages"] if p["name"] == "lightagent-desktop"), "?")
    ordered = sorted(shipped(metadata_dir, target, frontend).values(), key=lambda c: (c["ecosystem"], c["name"].lower(), c["version"]))
    mpl = [c for c in ordered if MPL.search(c["license"] or "")]
    problems = [f"{name} patches or replaces crates: a listed component may not be its published source" for name in patched(root)]
    text = [f"Lightagent Desktop {version}: third-party notices", f"Target: {target}", "",
            "Lightagent is licensed under the Apache License, Version 2.0 (LICENSE.txt, beside this file).",
            "This package also contains the third-party software listed below: the Rust crates the desktop",
            "host and its bundled runtime are built from, and the npm packages of the bundled interface,",
            "each with its version, its license and the license files it is published with. Components",
            "marked \"build time only\" are procedural macros, or used only by them: they run while the",
            "program is compiled and none of their code is in this package. They are listed so that the",
            "list is complete.", ""]
    if mpl:
        text += ["== Components under the Mozilla Public License 2.0 ==", "",
                 "These components are distributed in executable form under the MPL-2.0. Their Source Code Form",
                 "is the exact published archive named below; this build used it unmodified, and Lightagent",
                 "changes no MPL-covered file. Each archive can be obtained from crates.io at the address given,",
                 "and each Lightagent Desktop release also publishes the same archives, with their checksums, as",
                 "desktop-mpl-sources-<version>.tar.gz. The MPL-2.0 text is in each component's section below.", ""]
        for c in mpl:
            checksum = sums.get((c["name"], c["version"]))
            if c["source"] != CRATES_IO or not checksum:
                problems.append(f"{c['name']}@{c['version']}: not a checksummed crates.io package ({c['source']})")
            text += [f"{c['name']} {c['version']}" + (" (build time only)" if c["build_time_only"] else ""),
                     f"  Source Code Form: https://crates.io/crates/{c['name']}/{c['version']}",
                     f"  archive: https://static.crates.io/crates/{c['name']}/{c['name']}-{c['version']}.crate",
                     f"  SHA-256: {checksum or 'unknown'}",
                     f"  repository: {c['repository'] or 'not stated'}", ""]
    text += ["== Components ==", ""]
    first_seen, without = {}, []
    for c in ordered:
        text.append(f"-- {c['name']} {c['version']} ({c['ecosystem']}); license: {c['license']}; in: {', '.join(sorted(c['ships_in']))}"
                    + ("; build time only" if c["build_time_only"] else ""))
        if c["repository"]:
            text.append(f"   repository: {c['repository']}")
        texts = license_texts(c["directory"], c["license_file"])
        if not texts:
            without.append(f"{c['name']}@{c['version']}")
            text.append("   (published without a license file; its license is the one named above)")
        for name, body in texts:
            digest = hashlib.sha256(body.encode("utf-8")).hexdigest()
            if digest in first_seen:
                text.append(f"   {name}: the same text as {first_seen[digest]}")
            else:
                first_seen[digest] = f"{c['name']} {c['version']} ({name})"
                text += [f"   ---- {name} ----", body, "   ---- end ----"]
        text.append("")
    joined = "\n".join(text) + "\n"
    if mpl and "Mozilla Public License Version 2.0" not in joined:
        problems.append("no MPL-2.0 license text is among the components' license files")
    for c in mpl:
        if f"{c['name']}@{c['version']}" in without:
            problems.append(f"{c['name']}@{c['version']} (MPL-2.0) is published without its license file")
    with open(os.path.join(out, "THIRD_PARTY_NOTICES.txt"), "w", encoding="utf-8", newline="\n") as handle:
        handle.write(joined)
    shutil.copy(os.path.join(root, "LICENSE"), os.path.join(out, "LICENSE.txt"))
    crates = sum(1 for c in ordered if c["ecosystem"] == "cargo")
    lines = [f"### third-party notices `{target}`: {len(ordered)} components ({crates} crates, {len(ordered) - crates} npm), "
             f"{sum(1 for c in ordered if c['build_time_only'])} build time only, {len(first_seen)} distinct license texts, "
             f"{len(without)} without a license file; {len(joined.encode('utf-8')) // 1024} KiB"]
    for c in mpl:
        ident = f"{c['name']}@{c['version']}"
        where = f"crates.io, SHA-256 {sums.get((c['name'], c['version']), '?')}" if c["source"] == CRATES_IO else c["source"]
        lines.append(f"- MPL-2.0 {ident}{' (build time only)' if c['build_time_only'] else ''}: {where}; "
                     f"license file {'MISSING' if ident in without else 'included'}")
    if without:
        lines.append(f"- published without a license file (license named in their metadata): {', '.join(without[:40])}")
    lines += [f"- FAILED {problem}" for problem in problems]
    print("\n".join(lines))
    return 1 if problems else 0


def mpl_sources(metadata_dir, out, root):
    """The exact published archives of every MPL-2.0 crate any target links, checked against Cargo.lock."""
    os.makedirs(out, exist_ok=True)
    sums = lock_checksums(root)
    wanted = {}
    for target in TARGETS:
        for workspace, binary in (("desktop", "lightagent-desktop"), ("root", "lightagent")):
            for package in linked(load(os.path.join(metadata_dir, f"{workspace}-{target}.json")), binary):
                if package.get("source") and MPL.search(package.get("license") or ""):
                    wanted[(package["name"], package["version"])] = package["source"]
    cache = os.path.join(os.environ.get("CARGO_HOME") or os.path.join(os.path.expanduser("~"), ".cargo"), "registry", "cache")
    lines, problems, listing = ["### MPL-2.0 source archives (the exact published crates this build uses)"], [], []
    for (name, version), source in sorted(wanted.items()):
        expected = sums.get((name, version))
        found = sorted(glob.glob(os.path.join(cache, "*", f"{name}-{version}.crate")))
        if source != CRATES_IO or not expected or not found:
            why = "not from crates.io" if source != CRATES_IO else "no Cargo.lock checksum" if not expected else "its archive is not in the cargo cache"
            problems.append(f"{name}@{version}: {why}")
            continue
        actual = sha256_file(found[0])
        if actual != expected:
            problems.append(f"{name}@{version}: the archive's SHA-256 {actual} is not Cargo.lock's {expected}")
            continue
        shutil.copy(found[0], os.path.join(out, f"{name}-{version}.crate"))
        listing.append(f"{actual}  {name}-{version}.crate")
        lines.append(f"- {name} {version}: SHA-256 `{actual}`, equal to Cargo.lock's checksum (the published source, unmodified)")
    with open(os.path.join(out, "SHA256SUMS"), "w", encoding="utf-8", newline="\n") as handle:
        handle.write("".join(line + "\n" for line in listing))
    with open(os.path.join(out, "README.txt"), "w", encoding="utf-8", newline="\n") as handle:
        handle.write("The Source Code Form of the MPL-2.0 components in Lightagent Desktop: each archive is the\n"
                     "crate exactly as published on crates.io (its SHA-256 equals the checksum Cargo.lock pins),\n"
                     "used unmodified. Each archive holds the component's license text.\n")
    problems += [f"{name} patches or replaces crates" for name in patched(root)]
    lines += [f"- FAILED {problem}" for problem in problems]
    print("\n".join(lines))
    return 1 if problems else 0


def shortest_path(metadata, root_name, wanted):
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    start = [p["id"] for p in metadata["packages"] if p["name"] == root_name and p.get("source") is None]
    parent = {ident: None for ident in start}
    queue = list(start)
    while queue:
        current = queue.pop(0)
        if packages[current]["name"] == wanted:
            path = []
            while current is not None:
                path.append(f"{packages[current]['name']}@{packages[current]['version']}")
                current = parent[current]
            return list(reversed(path))
        for dep in nodes.get(current, {}).get("deps", []):
            if dep["pkg"] not in parent and any(kind.get("kind") is None for kind in dep.get("dep_kinds", [])):
                parent[dep["pkg"]] = current
                queue.append(dep["pkg"])
    return None


def call_sites(metadata, root_name, crate, pattern):
    found = []
    for package in linked(metadata, root_name):
        if package["name"] == crate:
            continue
        directory = os.path.dirname(package["manifest_path"])
        for base, dirs, names in os.walk(directory):
            dirs[:] = [d for d in dirs if d not in ("target", "node_modules", ".git")]
            for name in names:
                if not name.endswith(".rs"):
                    continue
                try:
                    with open(os.path.join(base, name), encoding="utf-8", errors="replace") as handle:
                        source = handle.read().splitlines()
                except OSError:
                    continue
                for number, line in enumerate(source, 1):
                    if pattern.search(line):
                        found.append(f"{package['name']}@{package['version']}:{os.path.relpath(os.path.join(base, name), directory)}:{number}")
    return found


def advisory_paths(metadata_dir):
    """Where each reviewed advisory's crate is reached, whether it is in the
    binary or only used at build time, and the call sites of its affected
    code in every crate the binary is built from (exact locked versions)."""
    lines = ["### Advisory crates: dependency paths and call sites (exact versions)"]
    for advisory, crate, pattern in ADVISORY_CODE:
        for target in TARGETS:
            for workspace, binary in (("desktop", "lightagent-desktop"), ("root", "lightagent")):
                metadata = load(os.path.join(metadata_dir, f"{workspace}-{target}.json"))
                path = shortest_path(metadata, binary, crate)
                if not path:
                    continue
                packages = {p["id"]: p for p in metadata["packages"]}
                in_binary = runtime_linked(metadata, binary)
                versions = sorted({p["version"] for p in linked(metadata, binary) if p["name"] == crate})
                built_in = any(packages[i]["name"] == crate for i in in_binary if i in packages)
                sites = call_sites(metadata, binary, crate, pattern) if pattern else []
                lines.append(f"- {advisory} {crate} {', '.join(versions)} in `{binary}` for `{target}`: "
                             f"{'in the binary' if built_in else 'build time only'}; path {' > '.join(path)}"
                             + (f"; call sites of the affected code: {len(sites)}" if pattern else ""))
                lines += [f"  - {site}" for site in sites[:20]]
    print("\n".join(lines))
    return 0


def main():
    if sys.argv[1] == "--cargo-audit":
        return cargo_audit(sys.argv[2], sys.argv[3])
    if sys.argv[1] == "--npm-audit":
        return npm_audit(sys.argv[2])
    if sys.argv[1] == "--notices":
        return notices(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6])
    if sys.argv[1] == "--mpl-sources":
        return mpl_sources(sys.argv[2], sys.argv[3], sys.argv[4])
    if sys.argv[1] == "--advisory-paths":
        return advisory_paths(sys.argv[2])
    return inventory(sys.argv[1], sys.argv[2], sys.argv[3])


if __name__ == "__main__":
    sys.exit(main())
