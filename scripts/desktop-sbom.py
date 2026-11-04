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
      npm: of high or critical severity).
"""

import csv
import datetime
import json
import os
import re
import sys
import uuid

COPYLEFT = re.compile(r"\b(A?GPL|LGPL|MPL|EPL|CDDL|EUPL|OSL|SSPL)", re.I)
TARGETS = ["x86_64-apple-darwin", "aarch64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]


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
    for kind, items in (report.get("warnings") or {}).items():
        for item in items or []:
            advisory, package = item.get("advisory") or {}, item.get("package") or {}
            lines.append(f"- warning ({kind}) {advisory.get('id', '')} {package.get('name')}@{package.get('version')}: {advisory.get('title', '')}")
    print("\n".join(lines))
    return 1 if found else 0


def npm_audit(path):
    report = load(path)
    counts = (report.get("metadata") or {}).get("vulnerabilities") or {}
    lines = [f"### npm audit (bundled UI, production dependencies): {json.dumps(counts)}"]
    for name, item in sorted((report.get("vulnerabilities") or {}).items()):
        lines.append(f"- {name}: {item.get('severity')} ({item.get('range', '')})")
    print("\n".join(lines))
    return 1 if (counts.get("high", 0) or 0) + (counts.get("critical", 0) or 0) else 0


def main():
    if sys.argv[1] == "--cargo-audit":
        return cargo_audit(sys.argv[2], sys.argv[3])
    if sys.argv[1] == "--npm-audit":
        return npm_audit(sys.argv[2])
    return inventory(sys.argv[1], sys.argv[2], sys.argv[3])


if __name__ == "__main__":
    sys.exit(main())
