"""Whether the Web UI listens for every run event the API can send.

  python3 check-event-names.py

Reads the SSE names crates/lightagent-api/src/sse.rs gives the run's events
(`pub fn name`) and the names frontend/src/hooks/useRunEvents.ts listens for
(EVENT_NAMES). A browser EventSource drops a named event that has no
listener without a trace, and the hook's ordered-id check then reads the
next event as a gap. Prints a Markdown summary; exits 1 when the API can
send a name the UI does not listen for.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def main():
    sse = (ROOT / "crates" / "lightagent-api" / "src" / "sse.rs").read_text(encoding="utf-8")
    body = sse.split("pub fn name(", 1)[1].split("\n}\n", 1)[0]
    sent = set(re.findall(r'"([a-z_]+(?:\.[a-z_]+)*)"', body))
    hook = (ROOT / "frontend" / "src" / "hooks" / "useRunEvents.ts").read_text(encoding="utf-8")
    listed = hook.split("const EVENT_NAMES = [", 1)[1].split("];", 1)[0]
    heard = set(re.findall(r'"([^"]+)"', listed))
    missing = sorted(sent - heard)
    print(f"### run event names: the API can send {len(sent)}, the UI listens for {len(heard)}")
    print(f"- the API: {', '.join(sorted(sent))}")
    print(f"- not listened for: {', '.join(missing) if missing else 'none'}")
    if missing:
        print("- FAILED a browser drops these without a trace, and the next event reads as a gap")
    return 1 if missing or not sent else 0


if __name__ == "__main__":
    sys.exit(main())
