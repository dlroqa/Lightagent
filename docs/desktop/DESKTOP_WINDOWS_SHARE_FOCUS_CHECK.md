# Manual check: keyboard focus after the Windows share panel

**Status: PENDING. Nobody has run it yet.** Acceptance item 5
(DESKTOP_RELEASE_ACCEPTANCE.md §7). Hosted runners cannot show the system
share panel, so this check needs a person at a real **Windows 11** desktop.
It takes about 10 minutes.

## You need

- A **Windows 11** PC (23H2 or 24H2) with a normal desktop session, not
  Windows Server and not a remote session that hides system panels. Record
  the edition, version and OS build from `winver` (for example "Windows 11
  Pro 24H2, OS build 26100.xxxx").
- The candidate installer `Lightagent_0.6.0_x64-setup.exe` from the dry run
  artifact `release-desktop-windows-x64` (see §1 of the acceptance record for
  the run and its expiry). Check its SHA-256 against `release-SHA256SUMS`:
  `Get-FileHash .\Lightagent_0.6.0_x64-setup.exe`.
- A model provider Lightagent can reach, so that one answer exists. The
  Share button is on an answer.
- The installer is unsigned, so SmartScreen warns. Choose More info, then
  Run anyway. That warning is expected and is not part of this check.

## Steps

Use only the keyboard. Step 5 is the one optional mouse step and is not part
of the verdict. After each step, note
where the visible focus ring is.

1. Install, launch Lightagent, send `Say hello.` and wait for the answer.
2. Press Tab until the answer's **Share** button has the focus ring, then
   press Enter. Expected: the Windows share panel opens.
   Record: did it open (yes or no)?
3. Press **Esc**. Expected: the panel closes and Lightagent is the active
   window. Record: is a focus ring visible, and where?
4. Press Tab twice, then Shift+Tab once. **Pass:** the focus ring moves
   between Lightagent's controls.
5. Optional, mouse: open the panel again (Tab to Share, then Enter). This time close it by
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

Windows 10 is optional; if one is available, repeat there and record it
separately.

## Result and what it means

| Outcome | Classification |
|---|---|
| Step 4 passes (focus returns after Esc) | item 5 closed |
| Focus is lost, but one of 6a–6d brings it back with the keyboard | documented limitation, a known issue in the release notes with the recovery key; the owner decides whether it blocks |
| Focus cannot be recovered with the keyboard (6a–6d all fail) | **unrecoverable keyboard trap: release blocker**, to be fixed and the check run again before release |

## Record

Copy this into a comment on the release pull request.

```
Share-panel focus check, Lightagent 0.6.0 candidate <installer SHA-256>
Tester / date:
Windows 11 edition, version, OS build (winver):
WebView2 Runtime version (Settings > Apps > Installed apps):
Installer: NSIS setup.exe | MSI
Step 2 share panel opened: yes | no
Step 3 after Esc, focus ring at:
Step 4 focus returned to the app after Esc (Tab moves it): yes | no
Step 5 (optional, mouse) focus returned after closing by click: yes | no | not run
Step 6 recoverable by keyboard: yes (6a | 6b | 6c | 6d) | no | not needed
Outcome: closed | limitation (recovery: ...) | BLOCKER
```
