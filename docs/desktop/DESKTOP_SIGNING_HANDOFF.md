# Lightagent Desktop: signing handoff (macOS and Windows)

**For the repository owner.** Status today: the repository has no Actions
secrets or variables, and its only environment is `jev-live`.
`release-signing` does **not** exist yet. Creating it and adding the
credentials are owner actions (§1 to §3). The workflow side is in the
candidate (§4).

**macOS signing and notarization stay open until a signed dry run passes
every macOS check in §5 for both .dmg files.**

## 1. Create the environment first

Do this before anything else. A workflow job that names an environment
that does not exist creates it **without protection**, so no signed run is
dispatched until this is done.

1. Repository › Settings › Environments › **New environment**, name
   exactly `release-signing`.
2. **Required reviewers:** add yourself. Leave "Prevent self-review" off;
   you are the only reviewer.
3. **Wait timer:** none.
4. **Deployment branches and tags:** choose *Selected branches and tags*
   and add two rules:
   - branch pattern `release/*`;
   - tag pattern `v*`.
5. **Environment secrets** and **environment variables:** add exactly the
   names in §2 and §3. Add them to this environment, **not** as repository
   secrets. Repository secrets would reach every job; environment secrets
   reach only the jobs that name `release-signing`, and only after you
   approve them.
6. Tell me when it exists. I check the environment's protection through
   the API before dispatching any signed run.

## 2. macOS: Developer ID and notarization (six environment secrets)

| # | Owner action |
|---|---|
| M1 | Apple Developer Program membership (an organization needs a D-U-N-S number). Only the Account Holder can create Developer ID certificates. |
| M2 | Create a **Developer ID Application** certificate. Not "Apple Development" and not "Mac App Distribution". In Keychain Access, export it **with its private key** as `.p12`, with a strong password. |
| M3 | App Store Connect › Users and Access › Integrations › App Store Connect API: create a **Team key** with the Developer role. Note the Issuer ID and the Key ID, and download `AuthKey_<KeyID>.p8`. Apple lets you download it only once. |

| Environment secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | the `.p12`, base64 encoded (`base64 -i DeveloperID.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_SIGNING_IDENTITY` | exactly as `security find-identity -v -p codesigning` prints it, for example `Developer ID Application: <Name> (<TEAMID>)` |
| `APPLE_API_ISSUER` | the Issuer ID (a UUID) |
| `APPLE_API_KEY` | the Key ID (10 characters) |
| `APPLE_API_KEY_P8` | the full text of `AuthKey_<KeyID>.p8`, its BEGIN and END lines included |

A signed run needs **all six**. With any one missing, the macOS jobs fail
before building.

## 3. Windows: Authenticode through a signing service

Since June 2023, a publicly trusted code-signing key must live in
hardware or a cloud HSM, so a `.pfx` file in a secret is not an option for
a new certificate.

**Option A, Azure Artifact Signing** (formerly Trusted Signing). The
workflow already passes these names:

| # | Owner action |
|---|---|
| W1 | An Azure subscription, an Artifact Signing account, and identity validation. Microsoft sets eligibility and regions. |
| W2 | A **Public Trust** certificate profile. |
| W3 | An Entra ID app registration with the role *Artifact Signing Certificate Profile Signer* (formerly *Trusted Signing Certificate Profile Signer*) on that profile, and a client secret. |

| Name in `release-signing` | Kind | Value |
|---|---|---|
| `AZURE_TENANT_ID` | secret | the directory (tenant) ID |
| `AZURE_CLIENT_ID` | secret | the app registration's client ID |
| `AZURE_CLIENT_SECRET` | secret | its client secret |
| `WINDOWS_SIGN_TOOL` | variable | `trusted-signing-cli@0.11.0` (pin an exact version) |
| `WINDOWS_SIGN_COMMAND` | variable | `trusted-signing-cli -e https://<region>.codesigning.azure.net -a <account> -c <certificate-profile> -d Lightagent %1` |

`WINDOWS_SIGN_COMMAND` is required: without it the Windows job fails
before building. `%1` is the file Tauri signs.

**Option B, a CA's cloud signer:** put its signer in
`WINDOWS_SIGN_COMMAND` (and `WINDOWS_SIGN_TOOL` if it is a crate). Its
credentials need adding to the "Build and bundle" step's `env`, which is a
workflow change.

## 4. How the workflow uses them (in the candidate)

- **`release.yml` input `signing`:** `unsigned` (the default) or `signed`.
  A tag push always signs. The release gate refuses any other value
  (`scripts/release-gate.sh`, tested in `scripts/test-release-gate.py`).
- **Environment, by job:**

  | Run | Uses `release-signing` | Uses no environment |
  |---|---|---|
  | signed | `desktop (macos-arm64)`, `desktop (macos-x64)`, `desktop (windows-x64)` | `desktop (linux-x64)` and every other job |
  | unsigned | none | every job |

  The three signing jobs wait for your approval in the run's review
  prompt. One approval covers all three.
- **A signed run fails when:**
  - a credential is missing;
  - Apple's notary service does not return `Accepted` for a disk image;
  - any check in §5 does not hold.
- **The .dmg files themselves:** after Tauri signs, notarizes and staples
  the app, each .dmg is signed with the Developer ID, submitted with
  `notarytool --wait`, and stapled. The notary results are kept in the
  evidence artifact.

## 5. The checks that prove signing worked

Steps "Signature state of what ships" and the install smoke test run on
the final files of a signed run.

| Platform | Check | Must show |
|---|---|---|
| macOS (both .dmg) | signing mode | `signed (Developer ID), notarized` |
| | `codesign --verify --deep --strict` on the app | `yes` |
| | `codesign -dv` on each .dmg | `Authority=Developer ID Application: …` |
| | `xcrun stapler validate` on each .dmg | `yes` |
| | quarantined copy of each .dmg, `spctl --type open --context context:primary-signature` | `accepted` |
| | the app copied out of it, quarantined, `spctl --type execute` | `accepted`, `source=Notarized Developer ID` |
| | `xcrun stapler validate` on that app | `yes` |
| Windows | Authenticode of the NSIS setup, the MSI, `lightagent-desktop.exe` and the bundled runtime `lightagent-sidecar.exe` as built | `Valid`, your signer, a timestamp; a missing file fails |
| | the same for the app and runtime **as installed** by each installer (install smoke `signatures`) | `Valid` and timestamped |
| Both | install, launch, user data, uninstall on the signed packages; notices identical to the generated ones | all pass |
| All | `SHA256SUMS` regenerated over the signed files | 11 OK |

Recommended before publishing: download one .dmg from the draft release in
Safari on a real Mac and open the app. Only the standard "downloaded from
the Internet" prompt may appear. On Windows, check the downloaded
installer's Properties › Digital Signatures.

## 6. The order of runs once the environment exists

1. **Negative check:** a signed dry run before the credentials are added
   (or with one deliberately missing). The macOS and Windows jobs must
   fail at "Signing configuration". This proves a signed request cannot
   fall back to unsigned.
2. **Signed dry run:** dispatched on `release/v0.6.0` with `dry_run` true
   and `signing` `signed`, after you approve the review gate. It must pass
   every check in §5.

Both are artifact-only: `publish` is skipped for dry runs.
