"""Save the conversation files the real Lightagent harness writes for the
research scenarios in scripts/tool-trace.py, as fixtures for the Windows
research-history analyzer test.

  LIGHTWEIGHT_MOCK_GATEWAY=... LIGHTAGENT_BIN=... python3 real_sessions.py <out-dir>

scripts/tool-trace.py is loaded unchanged. Its temporary work folder is kept
instead of deleted, and one scenario is added in memory (web.search against a
provider that answers HTTP 429). Afterwards every saved
profiles/<id>/sessions/<32 hex>.json is copied to
<out-dir>/home/profiles/<scenario>/sessions/, and <out-dir>/expected.json lists,
per tool call, what the analyzer must report. Expected counts are read from
the saved file here, independently of the analyzer. Everything stays on the
runner: the provider and the pages are loopback fixtures.
"""

import importlib.util
import json
import pathlib
import re
import shutil
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("tool_trace", ROOT / "scripts" / "tool-trace.py")
tool_trace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool_trace)

EXPECTED = {
    "rag-evidence": "evidence",
    "rag-zero-results": "provider_zero_results",
    "rag-unrelated-pages": "evidence",
    "search-results": "search_results",
    "search-zero-results": "provider_zero_results",
    "provider-429": "provider_http_429",
    "search-429": "provider_http_429",
}

tool_trace.SCENARIOS.append(
    {"name": "search-429", "tool": "web.search", "mode": "ratelimit", "error": True, "outcome": None,
     "contains": ["HTTP 429"], "pages": 0, "answer": ["The search provider ", "refused the request."]})


def counts(tool, text):
    """The analyzer's count columns, derived here independently."""
    out = {"results": "", "passages": "", "distinct_urls": "", "sources_read": "", "sources_total": ""}
    if text.startswith("Results for "):
        urls = re.findall(r"(?m)^\d+\. .*\n(https?://\S+)", text)
        out.update(results=str(len(urls)), distinct_urls=str(len(set(urls))))
    elif text.startswith("CURRENT WEB EVIDENCE"):
        urls = re.findall(r"(?m)^URL: (\S+)", text)
        out.update(passages=str(len(re.findall(r"(?m)^\[\d+\] ", text))), distinct_urls=str(len(set(urls))))
        status = re.search(r"Retrieval status: \w+ \((\d+) of (\d+) sources read\)", text)
        if status:
            out.update(sources_read=status.group(1), sources_total=status.group(2))
    elif text.startswith(("No results for ", "No current web results found for ")):
        out.update(results="0")
    return out


def main():
    out = pathlib.Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=True)
    work = pathlib.Path(tempfile.mkdtemp(prefix="research-history-real-"))
    real_rmtree = shutil.rmtree
    tool_trace.tempfile.mkdtemp = lambda prefix=None: str(work)
    shutil.rmtree = lambda path, *args, **kwargs: None if pathlib.Path(path) == work else real_rmtree(path, *args, **kwargs)
    sys.argv = [sys.argv[0], str(out / "trace")]
    try:
        tool_trace.main()
    except SystemExit as stop:
        # The trace's own checks are reported by check.yml; here only the
        # saved files matter, and each is verified below.
        print(f"tool-trace exited with {stop.code!r}; continuing with the saved files")
    finally:
        shutil.rmtree = real_rmtree

    expected, problems = [], []
    for name, result in EXPECTED.items():
        saved = sorted((work / name / "agent-home" / "profiles").glob("*/sessions/*.json"))
        saved = [path for path in saved if re.fullmatch(r"[0-9a-f]{32}\.json", path.name)]
        if len(saved) != 1:
            problems.append(f"{name}: expected one saved conversation, found {len(saved)}")
            continue
        target = out / "home" / "profiles" / name / "sessions"
        target.mkdir(parents=True, exist_ok=True)
        shutil.copy2(saved[0], target / saved[0].name)
        session = json.loads(saved[0].read_text(encoding="utf-8"))
        call_id = f"call_{name.replace('-', '_')}"
        calls = [tool for run in session.get("runs") or [] for tool in run.get("tools") or [] if tool.get("id") == call_id]
        if len(calls) != 1:
            problems.append(f"{name}: expected one saved call {call_id}, found {len(calls)}")
            continue
        call = calls[0]
        text = call.get("result_excerpt") or ""
        if result == "evidence" and "Retrieval status: evidence (" not in text:
            result = "evidence_partial" if "Retrieval status: partial (" in text else "evidence_status_cut"
        print(f"{name}: expect {result}; saved {call['tool']} outcome={call['outcome']} truncated={call.get('truncated')} "
              f"excerpt={text[:100]!r}")
        expected.append({"scenario": name, "call": call_id, "tool": call["tool"], "result": result,
                         "outcome": call["outcome"], **counts(call["tool"], text)})
    (out / "expected.json").write_text(json.dumps(expected, indent=1))
    real_rmtree(work, ignore_errors=True)
    if problems:
        print("\n".join(problems))
        raise SystemExit(1)
    print(f"saved {len(expected)} real conversation(s) to {out / 'home'}")


if __name__ == "__main__":
    main()
