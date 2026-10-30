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
| _pending_ | | |

## Failures and resolutions

None yet.

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
