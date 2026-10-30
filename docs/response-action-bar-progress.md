# Response action bar: progress record

Branch: `feat/response-action-bar` (from `master` @ `12582a0`).

Building, rendering and testing run on GitHub Actions only. The local
checkout was used to edit files, read source and keep this record. No build,
test, formatter, linter or browser was run locally.

## Scope

- An icon-only action row beneath the final assistant answer in the Web UI's
  Agent screen. It is not shown under user messages, reasoning, system
  notices, or an answer that is still streaming or not yet saved.
- Actions, in order: Copy response, Rate response, Read aloud, Share,
  Try again, More (with the answer's time and "Branch in new chat").
- A backend endpoint for Try again, which replaces the answer in place
  rather than sending the prompt again (the user's decision; sending it again
  would have repeated the user message in the transcript).

## How each action is wired

| Action | Wiring |
| --- | --- |
| Copy response | `navigator.clipboard.writeText(answer)`. The icon becomes a check and a polite status says "Copied to clipboard"; a failure says the clipboard is unavailable. |
| Rate response | Opens a menu with two choices, Good response and Bad response (`menuitemradio`, `aria-checked`). The choice is kept in component state only. The API has no feedback endpoint, and none was invented. |
| Read aloud | `speechSynthesis` with one `SpeechSynthesisUtterance`. A second press stops it, `aria-pressed` reflects the state, and leaving the answer stops speech. Without speech synthesis the button stays focusable (`aria-disabled`) and explains on press. |
| Share | `navigator.share({ title, text })` when it exists. Otherwise the text is copied, with a notice saying so. Cancelling the share sheet is silent. |
| Try again | `POST /api/lightagent/v1/sessions/{id}/regenerate` with the latest run's id, through `agentApi.regenerate`. The new run streams through the existing `useRunEvents` path, the old answer is dropped from view at once, and the saved session replaces it when the run ends. |
| More → Branch in new chat | Disabled, marked "Not available yet". The API cannot create a session from another session's transcript. The integration point is the `onBranch` prop, marked `TODO(branch-in-new-chat)` in `Agent.tsx`. |

### The regenerate endpoint

`POST /api/lightagent/v1/sessions/{id}/regenerate`, body
`{"run_id": "…", "model"?: "…", "profile"?: "…"}`.

- It shares `create_run`'s code path (`start_run`), so authorization is the
  same as `POST /runs` with a `session_id`. That covers multi-user ownership,
  `runs_write` + `sessions_write`, and the busy-session lock.
- Under the session lock it checks three things: `run_id` is the session's
  latest run, that run has finished, and the transcript ends with a user
  prompt and an assistant answer. Otherwise it answers 409 and changes
  nothing.
- It removes the answer and its run record, keeps the prompt once, and runs
  the prompt again with the same history its first run had. The normal
  recorder saves the new answer.
- If a supervised (multi-user) start is refused, the original session is
  saved back.

## Files changed

- `crates/lightagent-api/src/lib.rs`: the route, `RegenerateBody`, `Turn`,
  `take_latest_answer`, and `create_run` split into a thin wrapper over
  `start_run`.
- `crates/lightagent-api/tests/api.rs`: two tests,
  `regenerating_replaces_the_latest_answer_in_place` and
  `regenerating_anything_but_the_latest_answer_is_refused`.
- `frontend/src/components/ResponseActions.tsx` (new): the row, its tooltips,
  the status notice, and the rate and more menus.
- `frontend/src/components/Menu.tsx`: optional `placement`, `className` and
  `MenuItem.checked` props. The defaults keep existing menus unchanged.
- `frontend/src/screens/Agent.tsx`: final-answer detection, `regenerate()`,
  and an `actions` slot on `AgentMessage`.
- `frontend/src/api/agent.ts`: `agentApi.regenerate`.
- `frontend/src/api/format.ts`: `answerTimestamp` ("Today, 3:57 PM").
- `frontend/src/styles/tokens.css`: `--response-*` tokens in all three theme
  blocks.
- `frontend/src/styles/app.css`: the row, tooltip, notice and popover styles,
  plus coarse-pointer, narrow-width and reduced-motion rules.
- `e2e/actions.mjs` (new), `e2e/render.mjs`, `e2e/delivery.mjs` (exports
  `seededSession`): the browser check.
- `CHANGELOG.md`, `docs/architecture/profile-scoped-api.md`,
  `docs/subagents/integration-map.md`: the new route is documented.

## Sizes

The screenshot measurements read as about 1.5× CSS pixels. The row uses
18px glyphs in 40px square targets (44px on coarse pointers), a 10px radius,
8px gaps, and 14px medium tooltip text. The hover square is `#1d1d1d` in
dark mode. The values are custom properties on `.response-actions`.

## GitHub Actions checks

No workflow was added. `check.yml` already covers every required step, and
the new tests run inside its existing jobs:

| Job | What it covers here |
| --- | --- |
| `check (linux-x64, windows-x64, macos-arm64, macos-x64)` | `npm ci` for the frontend; `scripts/check.sh` runs `cargo fmt --check`, clippy `-D warnings`, the workspace tests (including the two new API tests) and the frontend production build (`tsc --noEmit && vite build`). |
| `standalone Web UI render` (`web-render`) | `scripts/render-panel.sh` boots the real server and runs `e2e/render.mjs` in headless Chromium, which now includes `checkResponseActions`. Screenshots are uploaded as `lightagent-web-screens` (under `actions/`). |

`checkResponseActions` asserts:

- exactly one row, inside the last assistant message, and none on user
  messages;
- the six actions in order;
- alignment with the answer's text edge;
- hover background `rgb(29, 29, 29)`, a pointer cursor, the tooltip shown
  and no layout movement;
- arrow-key navigation, keyboard-focus tooltips, and the exact "Try again…"
  label;
- copy and read-aloud results;
- the rating state;
- the More menu's timestamp and branch row, closing on Escape (focus
  returns to the trigger) and on an outside click;
- for Try again: one regenerate request naming the replaced run, no row and
  no old answer while the new one streams, then the new answer and the
  prompt each shown exactly once, with the row back;
- no horizontal overflow at 360px.

## CI runs

| Run | Commit | Result |
| --- | --- | --- |
| [check 41144035801](https://github.com/dlroqa/Lightagent/actions/runs/41144035801) | `13f2ecc` | Failed: `cargo fmt --check` on all four platforms (see below). `standalone Web UI render` passed, including the response-action check. The other jobs in the run passed. |
| [check 41144488593](https://github.com/dlroqa/Lightagent/actions/runs/41144488593) | `2241e56` | Failed: fmt passed; clippy `type_complexity` on the test factory's field (see below). `standalone Web UI render` passed again. |
| [check 41144876654](https://github.com/dlroqa/Lightagent/actions/runs/41144876654) | `0da3370` | Failed: fmt and clippy passed; 20 of 21 `lightagent-api` API tests passed, and `regenerating_replaces_the_latest_answer_in_place` failed on a wrong assumption in the test (see below). `standalone Web UI render` passed. |
| [check 41145565722](https://github.com/dlroqa/Lightagent/actions/runs/41145565722) | `15e2693` | **Passed**: every job, including `check` on linux-x64, windows-x64, macos-arm64 and macos-x64 (fmt, clippy, workspace tests with both regenerate tests, frontend production build), and `standalone Web UI render` with the response-action check. |
| [jev evaluation 41145565768](https://github.com/dlroqa/Lightagent/actions/runs/41145565768), [subagents 41145565880](https://github.com/dlroqa/Lightagent/actions/runs/41145565880) | `15e2693` | Passed. The dispatched `subagent fixture fail` and `verify` runs failed, as they do on other branches. |
| [jev evaluation 41144035777](https://github.com/dlroqa/Lightagent/actions/runs/41144035777) | `13f2ecc` | Passed |
| [subagents 41144035939](https://github.com/dlroqa/Lightagent/actions/runs/41144035939) | `13f2ecc` | Passed. The `subagent fixture fail` and `verify` runs it dispatches failed, as they do on other branches. |

## Failures and resolutions

1. Run 41144035801 failed at `cargo fmt --check`, at two call sites
   (`.route(…regenerate…)` in `lib.rs` and a `format!` in `tests/api.rs`).
   rustfmt's call-width heuristic wraps them even though they fit within 100
   columns. `check.sh` stops at fmt, so clippy and the Rust tests did not run
   in that run. Fixed by applying CI's diff verbatim.
2. Run 41144488593 failed in clippy (`-D warnings`) with `type_complexity`
   on `PromptFactory.requests` in `tests/api.rs`. Fixed with a `Prompts` type
   alias. The tests had not run yet, because clippy comes first.
3. Run 41144876654: `regenerating_replaces_the_latest_answer_in_place`
   asserted that the saved run record's id equals the id `POST /regenerate`
   returned. They never match, and this predates this work: the agent loop
   mints its own `RunId` (`lightagent-core/src/loop_.rs`), and that is the id
   `record_run_events` saves. The manager's API id is separate. The endpoint
   itself was correct, because the UI sends the saved id and the server
   compares it with the saved id. The test now asserts that the old record was
   replaced. `e2e/actions.mjs` now also gives the regenerated record a saved
   id different from the API id, so the UI is tested the way it runs in
   production (its saved-answer match, not run-id equality).

## Limitations and follow-ups

- Branch in new chat needs an API that copies a session's transcript up to a
  message into a new session. Until then the row is disabled.
- Ratings are not persisted or sent anywhere, because there is no feedback
  endpoint.
- Regenerate replaces only the latest answer, which is what the UI offers. A
  regenerated answer's earlier version is not kept, so there is no version
  switcher.
- Sharing sends the answer's text. The Web UI has no public link for one
  answer to share instead.
