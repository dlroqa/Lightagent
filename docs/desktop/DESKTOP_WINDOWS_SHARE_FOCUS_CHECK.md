# Manual check: keyboard focus after the Windows share panel

**Status: PENDING. Nobody has run it yet.** Acceptance item 5
(DESKTOP_RELEASE_ACCEPTANCE.md §7). Hosted runners cannot show the system
share panel, so this check needs a person at a real Windows 10 or 11 desktop.
It takes about 10 minutes.

## You need

- A Windows 10 (22H2) or Windows 11 PC with a normal desktop session, not
  Windows Server and not a remote session that hides system panels. Record
  the edition and build from `winver`.
- The candidate installer `Lightagent_0.6.0_x64-setup.exe` from the dry run
  artifact `release-desktop-windows-x64` (see §1 of the acceptance record for
  the run and its expiry). Check its SHA-256 against `release-SHA256SUMS`:
  `Get-FileHash .\Lightagent_0.6.0_x64-setup.exe`.
- A model provider Lightagent can reach, so that one answer exists. The
  Share button is on an answer.
- The installer is unsigned, so SmartScreen warns. Choose More info, then
  Run anyway. That warning is expected and is not part of this check.

## Steps

Use only the keyboard unless a step says otherwise. After each step, note
where the visible focus ring is.

1. Install, launch Lightagent, send `Say hello.` and wait for the answer.
2. Press Tab until the answer's **Share** button has the focus ring, then
   press Enter. Expected: the Windows share panel opens.
   Record: did it open (yes or no)?
3. Press **Esc**. Expected: the panel closes and Lightagent is the active
   window. Record: is a focus ring visible, and where?
4. Press Tab twice, then Shift+Tab once. **Pass:** the focus ring moves
   between Lightagent's controls.
5. Open the panel again (Tab to Share, then Enter). This time close it by
   **clicking inside the Lightagent window** (mouse). Then repeat step 4
   with the keyboard.
6. If step 4 or step 5 failed, try to recover with the keyboard only, in
   this order, and stop at the first that works. After each, press Tab and
   see whether the focus ring moves inside Lightagent:
   a. Alt+Tab to Lightagent;
   b. F6;
   c. Win+D, then Alt+Tab to Lightagent;
   d. Alt+Space (window menu), then Esc.
   Record which one worked, or that none did.

Repeat on the other Windows version if one is available.

## Result and what it means

| Outcome | Classification |
|---|---|
| Steps 4 and 5 pass | item 5 closed |
| Focus is lost, but one of 6a–6d brings it back with the keyboard | documented limitation, a known issue in the release notes with the recovery key; the owner decides whether it blocks |
| Focus cannot be recovered with the keyboard (6a–6d all fail) | **unrecoverable keyboard trap: release blocker** |

## Record

Copy this into a comment on the release pull request.

```
Share-panel focus check, Lightagent 0.6.0 candidate <installer SHA-256>
Tester / date:
Windows edition, version, build (winver):
WebView2 Runtime version (Settings > Apps > Installed apps):
Installer: NSIS setup.exe | MSI
Step 2 share panel opened: yes | no
Step 3 after Esc, focus ring at:
Step 4 Tab moves focus in the app: pass | fail
Step 5 after closing by click, Tab moves focus: pass | fail
Step 6 recovery (if needed): 6a | 6b | 6c | 6d | none
Outcome: closed | limitation (recovery: ...) | BLOCKER
```
