"""Whether the bundled third-party notices name every crate, at its exact
version, that the shipped Desktop binaries were built from, each with a
license text (release.yml).

  python desktop-notices-shipped.py <THIRD_PARTY_NOTICES.txt> <binary>...

A release build keeps the source paths of code that can panic, under the
cargo registry: .../index.crates.io-<hash>/<crate>-<version>/... Those paths
are a record taken from the shipped binaries themselves, not from cargo
metadata (which the notices are generated from), of which crates and versions
the binaries contain. Every one must be in the notices with the same version,
and its entry must carry a license text: its own, one it shares with an
earlier entry ("the same text as"), or, for an MPL-2.0 crate, the MPL-2.0
text it is pointed to. Crates the notices list that the binaries do not show
are counted, not failed: a crate with no code that can panic leaves no path,
and procedural macros leave none.

Prints a Markdown summary. Exits 1 when a crate or version found in a binary
is not in the notices or has no license text there, or when a binary shows
no registry path at all (the check would prove nothing).
"""

import re
import sys

PATH = re.compile(rb"index\.crates\.io-[0-9a-f]{16}[/\\]([A-Za-z0-9_.+-]+?)-(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)[/\\]")
LISTED = re.compile(r"^-- (\S+) (\S+) \(cargo\);")
TEXT = re.compile(r"^   ---- .+ ----$|^   .+: the same text as |the MPL-2\.0 text it is licensed under is the one in ")


def main():
    notices, binaries = sys.argv[1], sys.argv[2:]
    listed, texted, current = {}, set(), None
    with open(notices, encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            match = LISTED.match(line)
            if match:
                listed.setdefault(match.group(1), set()).add(match.group(2))
                current = (match.group(1), match.group(2))
            elif line.startswith("-- ") or line.startswith("== "):
                current = None
            elif current and TEXT.search(line):
                texted.add(current)
    lines = [f"### notices against the shipped binaries: {sum(len(v) for v in listed.values())} crate versions listed, "
             f"{len(texted)} with a license text"]
    problems, seen = [], set()
    for binary in binaries:
        with open(binary, "rb") as handle:
            data = handle.read()
        found = {(m.group(1).decode(), m.group(2).decode()) for m in PATH.finditer(data)}
        missing = sorted(f"{name}@{version}" for name, version in found if version not in listed.get(name, ()))
        bare = sorted(f"{name}@{version}" for name, version in found if version in listed.get(name, ()) and (name, version) not in texted)
        seen |= found
        lines.append(f"- {binary}: {len(found)} crate versions found in the binary, {len(missing)} not in the notices"
                     + (f": {', '.join(missing[:40])}" if missing else "")
                     + f", {len(bare)} without a license text" + (f": {', '.join(bare[:40])}" if bare else ""))
        if not found:
            problems.append(f"{binary}: no registry path found, nothing was checked")
        if missing:
            problems.append(f"{binary}: {len(missing)} shipped crate versions are not in the notices")
        if bare:
            problems.append(f"{binary}: {len(bare)} shipped crate versions have no license text in the notices")
    unseen = sum(1 for name, versions in listed.items() for version in versions if (name, version) not in seen)
    untexted = sorted(f"{name}@{version}" for name, versions in listed.items() for version in versions if (name, version) not in texted)
    lines.append(f"- listed but not visible in any binary (no panicking code, or build time only): {unseen}")
    lines.append(f"- listed without a license text (allowed only for build time only entries, which desktop-sbom.py enforces): "
                 f"{len(untexted)}" + (f": {', '.join(untexted[:40])}" if untexted else ""))
    lines += [f"- FAILED {problem}" for problem in problems]
    print("\n".join(lines))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
