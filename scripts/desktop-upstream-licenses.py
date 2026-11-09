"""Whether desktop/licenses/upstream/ holds exactly the upstream license
files SOURCES.json names (desktop.yml supply chain, release.yml).

  python desktop-upstream-licenses.py <repo-root>

For every crate in SOURCES.json: its version is in Cargo.lock or
desktop/Cargo.lock, and each of its texts is fetched again from
https://raw.githubusercontent.com/<repository>/<commit>/<path> and must
hash to the recorded git blob id, as the stored copy in blobs/ must.
blobs/ holds no file SOURCES.json does not name. (That each recorded
commit is the one the crate version was published from is checked by
desktop-sbom.py --notices against the crate's .cargo_vcs_info.json.)

Prints a Markdown summary. Exits 1 on any difference. Needs network access
to GitHub.
"""

import hashlib
import json
import os
import re
import sys
import urllib.request


def git_blob(data):
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def locked(root):
    found = set()
    for lock in ("Cargo.lock", os.path.join("desktop", "Cargo.lock")):
        with open(os.path.join(root, lock), encoding="utf-8") as handle:
            for block in handle.read().split("[[package]]"):
                name = re.search(r'^name = "([^"]+)"', block, re.M)
                version = re.search(r'^version = "([^"]+)"', block, re.M)
                if name and version:
                    found.add((name.group(1), version.group(1)))
    return found


def main():
    root = sys.argv[1]
    base = os.path.join(root, "desktop", "licenses", "upstream")
    with open(os.path.join(base, "SOURCES.json"), encoding="utf-8") as handle:
        manifest = json.load(handle)
    pinned, problems, fetched = locked(root), [], {}
    notes = manifest.get("notes", {})
    for crate in manifest["crates"]:
        ident = f"{crate['name']}@{crate['version']}"
        if (crate["name"], crate["version"]) not in pinned:
            problems.append(f"{ident}: in SOURCES.json but in neither Cargo.lock")
        if crate.get("note") and crate["note"] not in notes:
            problems.append(f"{ident}: its note {crate['note']!r} is not defined")
        if not crate["texts"]:
            problems.append(f"{ident}: no texts")
        for t in crate["texts"]:
            url = f"https://raw.githubusercontent.com/{t['repository']}/{t['commit']}/{t['path']}"
            if url not in fetched:
                with urllib.request.urlopen(url, timeout=60) as response:
                    fetched[url] = git_blob(response.read())
            try:
                with open(os.path.join(base, "blobs", t["blob"]), "rb") as handle:
                    stored = git_blob(handle.read())
            except OSError:
                stored = None
            if not (fetched[url] == stored == t["blob"]):
                problems.append(f"{ident}: {url} hashes to {fetched[url]}, the stored copy to {stored}, recorded {t['blob']}")
    named = {t["blob"] for crate in manifest["crates"] for t in crate["texts"]}
    stored_blobs = set(os.listdir(os.path.join(base, "blobs")))
    if stored_blobs != named:
        problems.append(f"blobs/ holds {sorted(stored_blobs - named)} that SOURCES.json does not name, and lacks {sorted(named - stored_blobs)}")
    lines = [f"### upstream license texts: {len(manifest['crates'])} crates, {len(named)} distinct files, "
             f"{len(fetched)} upstream files fetched again and compared by git blob id"]
    for crate in manifest["crates"]:
        origin = crate["published_from"]
        files = ", ".join(t["path"] + ("" if t["repository"] == origin["repository"] else f" ({t['repository']} `{t['commit'][:12]}`)")
                          for t in crate["texts"])
        lines.append(f"- {crate['name']} {crate['version']} ({crate['license']}): github.com/{origin['repository']} "
                     f"at `{origin['commit'][:12]}`: {files}")
    lines += [f"- FAILED {problem}" for problem in problems]
    print("\n".join(lines))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
