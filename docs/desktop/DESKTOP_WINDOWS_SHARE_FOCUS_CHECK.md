# Manual check: keyboard focus after the Windows share panel

**Status: PASSED, with a documented limitation (2026-11-07).** Acceptance
item 5 (DESKTOP_RELEASE_ACCEPTANCE.md §7). Hosted runners cannot show the
system share panel, so this check needs a person at a real **Windows 11**
desktop. It takes about 10 minutes. Results are below the steps.

## You need

- A **Windows 11** PC (23H2 or later) with a normal desktop session, not
  Windows Server and not a remote session that hides system panels. Record
  the edition, version and OS build from `winver` (for example "Windows 11
  Pro 24H2, OS build 26100.xxxx").
- The candidate installer (`Lightagent_0.6.0_x64-setup.exe` or
  `Lightagent_0.6.0_x64_en-US.msi`) from the dry run artifact
  `release-desktop-windows-x64` (see §1 of the acceptance record for the
  run). Check its SHA-256 against `release-SHA256SUMS`:
  `Get-FileHash .\Lightagent_0.6.0_x64_en-US.msi`.
- A model provider Lightagent can reach, so that one answer exists. The
  Share button is on an answer.
- The installer is unsigned, so SmartScreen warns. Choose More info, then
  Run anyway. That warning is expected and is not part of this check.

## Steps

Use only the keyboard. Step 5 is the one optional mouse step and is not part
of the verdict. After each step, note where the visible focus ring is.

1. Install, launch Lightagent, send `Say hello.` and wait for the answer.
2. Press Tab until the answer's **Share** button has the focus ring, then
   press Enter. Expected: the Windows share panel opens.
   Record: did it open (yes or no)?
3. Press **Esc**. Record whether the panel closed. If it is still open,
   close it from the keyboard instead, in either way, and record which:
   a. Tab or Shift+Tab to the panel's **Close (X)** button, then Enter;
   b. **Alt+F4**, only while the share panel is still open (if it has
      already closed, Alt+F4 closes Lightagent).
   Then record: is a focus ring visible, and where?
4. Press Tab twice, then Shift+Tab once. **Pass:** the focus ring moves
   between Lightagent's controls.
5. Optional, mouse: open the panel again (Tab to Share, then Enter). This
   time close it by **clicking inside the Lightagent window** (mouse). Then
   repeat step 4 with the keyboard.
6. If step 4 or step 5 failed, try to recover with the keyboard only, in
   this order, and stop at the first that works. After each, press Tab and
   see whether the focus ring moves inside Lightagent:
   a. Alt+Tab to Lightagent;
   b. F6;
   c. Win+D, then Alt+Tab to Lightagent;
   d. Alt+Space (window menu), then Esc.
   Record which one worked, or that none did.

Windows 10 is optional; if one is available, repeat there and record it
separately.

## Result and what it means

| Outcome | Classification |
|---|---|
| Step 4 passes after the panel closes from the keyboard (Esc, or 3a or 3b) | item 5 closed; if Esc did not close it, a documented limitation naming 3a and 3b |
| Focus is lost, but one of 6a–6d brings it back with the keyboard | documented limitation, a known issue in the release notes with the recovery key; the owner decides whether it blocks |
| The panel cannot be closed, or focus cannot be recovered, with the keyboard | **unrecoverable keyboard trap: release blocker**, to be fixed and the check run again before release |

## Results

Both runs were by the owner, on the same PC: Windows 11 Pro, OS build
26300.9818, with the MSI.

| Candidate | MSI SHA-256 | Result |
|---|---|---|
| `22ad4fe` (dry run 42268737686) | `4f5084c98aa17461a5f2f4546b1bc2a12e97e8f58fa8756c06d4a194308939d6` | Enter opened the panel; Esc closed it; Tab and Shift+Tab then moved in Lightagent; no recovery needed. PASS |
| `b0f66a416ed7809e006bf5807ec59d3141bddf69` (dry run 42345778723) | `a052398862bfef3e0aa197cee625374ccb00563f9ece6a5ffe4701275e8ef084` | Enter opened the panel; **Esc did not close it**; Tab and Shift+Tab moved within the panel; the panel closed with Close (X) then Enter (3a) and with Alt+F4 (3b); after either, focus returned to Lightagent and Tab and Shift+Tab moved through its controls; no mouse and no reload were needed. **PASS with a documented limitation** |

**Classification (owner, 2026-11-07):** a recoverable Windows/WebView2
keyboard-accessibility limitation, not a keyboard trap. Item 5 is closed.
Esc not closing the panel does not prevent keyboard-only use.

**Why Esc is outside Lightagent.** Both Share entry points (the answer's
action row and the conversation menu) call the WebView's `navigator.share`;
WebView2 shows the Windows share panel. While the panel is open it has
keyboard focus, and Lightagent's page receives no key presses, so
Lightagent neither handles nor can intercept Esc there. The Desktop host
has no share or key-handling code. No Share code changed between the two
candidates, so the different Esc behaviour on the same PC and build comes
from the panel or WebView2. A code change to move focus back after the
panel closes was considered and not made (owner decision): focus already
returns.

## Record

Copy this into a comment on the release pull request.

```
Share-panel focus check, Lightagent 0.6.0 candidate <installer SHA-256>
Tester / date:
Windows 11 edition, version, OS build (winver):
WebView2 Runtime version (Settings > Apps > Installed apps):
Installer: NSIS setup.exe | MSI
Step 2 share panel opened: yes | no
Step 3 Esc closed the panel: yes | no; if no, closed with: 3a Close (X) + Enter | 3b Alt+F4 | could not close
Step 3 after closing, focus ring at:
Step 4 focus returned to the app (Tab moves it): yes | no
Step 5 (optional, mouse) focus returned after closing by click: yes | no | not run
Step 6 recoverable by keyboard: yes (6a | 6b | 6c | 6d) | no | not needed
Outcome: closed | limitation (recovery: ...) | BLOCKER
```
