# Recorded live run 40503493185 (2026-10-27)

The first live Jev specialist-selection run, kept unchanged for offline
diagnosis. **Development evidence only.** Replaying these files reproduces
what Jev answered to these exact requests; it does not show how Jev would
answer different questions or different prompts, and it is not a new live
result.

| | |
| --- | --- |
| Run | https://github.com/dlroqa/Lightagent/actions/runs/40503493185 (`jev-selection-pinned.yml`, one dispatch, owner-approved send) |
| Workflow source | `0a07e74a343a34ea56a1373e6c740f5912c7f76e` |
| Code under test | `b1ffd79c33bc39a13cf26ddf94126b6cb83bfd1c` |
| Request digest | `57cbfbd3921120e12f3e6f4b144c265a885033fec90f6c0092ab5a271c6d1e3a` |
| Model requested / returned | `jev-latest` / `jev-1.14.0` |
| Authoritative verdict | **BELOW_THRESHOLD** (complete, no problems): decision accuracy 12/24, unnecessary delegation 0/12, missed delegation 12/12, role match 0/0 |

Files, byte for byte as the run's artifacts held them:
- `requests/`: the 24 exported request bodies and `manifest.json` (`selection-requests`);
- `responses/`: Jev's 24 typed answers and `send.json` (`selection-responses`);
- `verdict.json`: the guard's verdict (`jev-selection-reports`).

The prompts are the synthetic, hand-labeled `selection.v1.json` set. The
responses hold only typed answers, the model name and token usage. The labels
and this result are not changed after the run; disputed labels are discussed
in `docs/subagents/selection-diagnosis.md`, not relabeled.
