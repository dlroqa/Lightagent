"""Whether the bundled third-party notices name every crate, at its exact
version, that the shipped Desktop binaries were built from (release.yml).

  python desktop-notices-shipped.py <THIRD_PARTY_NOTICES.txt> <binary>...

A release build keeps the source paths of code that can panic, under the
cargo registry: .../index.crates.io-<hash>/<crate>-<version>/... Those paths
are a record taken from the shipped binaries themselves, not from cargo
metadata (which the notices are generated from), of which crates and versions
the binaries contain. Every one must be in the notices with the same version.
Crates the notices list that the binaries do not show are counted, not
failed: a crate with no code that can panic leaves no path, and procedural
macros leave none.

Prints a Markdown summary. Exits 1 when a crate or version found in a binary
is not in the notices, or when a binary shows no registry path at all (the
check would prove nothing).
"""

import re
import sys

PATH = re.compile(rb"index\.crates\.io-[0-9a-f]{16}[/\\]([A-Za-z0-9_.+-]+?)-(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)[/\\]")
LISTED = re.compile(r"^-- (\S+) (\S+) \(cargo\);")


def main():
    notices, binaries = sys.argv[1], sys.argv[2:]
    listed = {}
    with open(notices, encoding="utf-8") as handle:
        for line in handle:
            match = LISTED.match(line)
            if match:
                listed.setdefault(match.group(1), set()).add(match.group(2))
    lines = [f"### notices against the shipped binaries: {sum(len(v) for v in listed.values())} crate versions listed"]
    problems, seen = [], set()
    for binary in binaries:
        with open(binary, "rb") as handle:
            data = handle.read()
        found = {(m.group(1).decode(), m.group(2).decode()) for m in PATH.finditer(data)}
        missing = sorted(f"{name}@{version}" for name, version in found if version not in listed.get(name, ()))
        seen |= found
        lines.append(f"- {binary}: {len(found)} crate versions found in the binary, {len(missing)} not in the notices"
                     + (f": {', '.join(missing[:40])}" if missing else ""))
        if not found:
            problems.append(f"{binary}: no registry path found, nothing was checked")
        if missing:
            problems.append(f"{binary}: {len(missing)} shipped crate versions are not in the notices")
    unseen = sum(1 for name, versions in listed.items() for version in versions if (name, version) not in seen)
    lines.append(f"- listed but not visible in any binary (no panicking code, or build time only): {unseen}")
    lines += [f"- FAILED {problem}" for problem in problems]
    print("\n".join(lines))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
