# Lightagent Desktop: signing handoff (macOS and Windows)

**For the repository owner.** No credential exists in the repository today
(no Actions secrets or variables; the only environment is `jev-live`).
Everything below is an owner action except the one workflow line in step
M4/W4, which goes through the source-edit workflow after approval.
**macOS signing and notarization stay open until a credentialed dry run's
quarantined-download check passes on both macOS packages** (§3).

What the release workflow already does (`.github/workflows/release.yml`,
job `desktop`):

- Step "Signing configuration": reads only `secrets.*` and `vars.*`. With
  none set it builds unsigned. A partial set fails the job.
- Step "Signature state of what ships": reports every package's signature.
  When signing is configured, it fails unless the result verifies (§3).
- Linux needs nothing. The .deb and AppImage are unsigned and covered by
  `SHA256SUMS`.

## 1. macOS: Developer ID signing and notarization

| # | Owner action |
|---|---|
| M1 | Apple Developer Program membership. An organization needs a D-U-N-S number. Only the Account Holder can create Developer ID certificates. |
| M2 | Create a **Developer ID Application** certificate (not "Apple Development", not "Mac App Distribution"), from Xcode (Settings › Accounts › Manage Certificates) or from a CSR on developer.apple.com. In Keychain Access, export the certificate **with its private key** as a `.p12` file with a strong password. |
| M3 | For notarization, create an App Store Connect API **Team key** (Users and Access › Integrations › App Store Connect API) with the Developer role. Note its Issuer ID and Key ID, and download `AuthKey_<KeyID>.p8`. Apple lets you download it only once. |
| M4 | Store the six values below as secrets of a protected environment (§4). |

| Secret | Value |
|---|---|
| `APPLE_CERTIFICATE` | the `.p12` file, base64 (`base64 -i DeveloperID.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <Name> (<TEAMID>)`, exactly as `security find-identity -v -p codesigning` prints it |
| `APPLE_API_ISSUER` | the Issuer ID (a UUID) |
| `APPLE_API_KEY` | the Key ID (10 characters) |
| `APPLE_API_KEY_P8` | the full text of `AuthKey_<KeyID>.p8`, including its BEGIN and END lines |

Tauri signs the app with the hardened runtime, submits it to Apple's notary
service through `notarytool`, and staples the ticket. The first signed run
also shows whether the app launches under the hardened runtime: the install
smoke test launches it. If it does not launch, an entitlements file is the
fix, and a new dry run follows.

## 2. Windows: Authenticode through a signing service

Since June 2023, a publicly trusted code-signing certificate's private key
must be kept in hardware or a cloud HSM. A `.pfx` file in a secret is
therefore not an option for a new certificate. The workflow signs through
Tauri's `bundle.windows.signCommand`, so any service with a command-line
signer fits.

**Option A: Azure Artifact Signing** (formerly Trusted Signing). The
workflow already passes its credentials.

| # | Owner action |
|---|---|
| W1 | An Azure subscription, an Artifact Signing account, and identity validation. Microsoft sets eligibility (organization or individual) and regions; check them first. |
| W2 | A **Public Trust** certificate profile in that account. |
| W3 | An Entra ID app registration (service principal) with the role *Artifact Signing Certificate Profile Signer* (named *Trusted Signing Certificate Profile Signer* in older portals) on that profile, and a client secret for it. |
| W4 | Store the values below in the protected environment (§4). |

| Name | Kind | Value |
|---|---|---|
| `AZURE_TENANT_ID` | secret | the directory (tenant) ID |
| `AZURE_CLIENT_ID` | secret | the app registration's client ID |
| `AZURE_CLIENT_SECRET` | secret | its client secret |
| `WINDOWS_SIGN_TOOL` | variable | `trusted-signing-cli@0.11.0` (the current release, 2026-05-15; pin an exact version) |
| `WINDOWS_SIGN_COMMAND` | variable | `trusted-signing-cli -e https://<region>.codesigning.azure.net -a <account> -c <certificate-profile> -d Lightagent %1` |

A later improvement: an OIDC federated credential in place of the client
secret, so no long-lived secret is stored. That needs `id-token: write` and
a login step in the workflow.

**Option B: a certificate authority's cloud signing service.** Set
`WINDOWS_SIGN_COMMAND` (and `WINDOWS_SIGN_TOOL` if its signer is a crate) to
the service's signer. Its credentials need to be added to the
"Build and bundle" step's `env`, which is a workflow change.

SmartScreen reputation builds up with downloads, whatever the certificate.
A newly signed installer can still warn at first.

## 3. The checks that prove signing worked

All of these run in the first credentialed run, which must be a dry run of
`release.yml` on the release branch (`dry_run` true). Each check fails the
job when signing is configured and the result does not hold.

| Platform | Check (step "Signature state of what ships") | Must show |
|---|---|---|
| macOS (both) | signing mode in the summary | `signed (Developer ID), notarized` |
| | `codesign -dv` | `Authority=Developer ID Application: <Name> (<TEAMID>)`, the team, and the runtime flag in `CodeDirectory` |
| | `codesign --verify --deep --strict` | `yes` |
| | **quarantined download**: a copy of each .dmg with the `com.apple.quarantine` attribute a browser sets, `spctl --assess --type open --context context:primary-signature` | `accepted` |
| | **the app copied out of that .dmg**, quarantined, `spctl --assess --type execute` | `accepted` and `source=Notarized Developer ID` |
| | `xcrun stapler validate` on that app | `yes` |
| Windows | signing mode | `signed (signCommand)` |
| | `Get-AuthenticodeSignature` on the NSIS setup.exe, the .msi, `lightagent-desktop.exe` and the bundled runtime when it is in the build directory | `Valid`, with your signer and a timestamp |
| Both | install smoke test on the signed packages | install, launch, user data and uninstall all pass |

The macOS item closes only when the quarantined-download rows pass for both
`Lightagent_0.6.0_aarch64.dmg` and `Lightagent_0.6.0_x64.dmg`. Before
publishing, I also recommend one real check: download the .dmg in Safari
from the draft release on a Mac and open the app. Only the standard "downloaded
from the Internet" prompt may appear. On Windows: open the downloaded
installer's Properties › Digital Signatures, and note whether SmartScreen
warns.

If the disk image is rejected while the app is accepted (Tauri notarizes and
staples the app; whether the .dmg itself is notarized depends on the
bundler), the job fails, the item stays open, and notarizing the .dmg is the
next change.

## 4. Protected configuration in GitHub

Recommended: an **environment** rather than repository secrets.

1. Settings › Environments › New environment `release-signing`.
2. **Required reviewers:** the owner. Every job that uses the environment
   waits for approval.
3. **Deployment branches and tags:** selected only. Branch `release/*` and
   tag `v*`.
4. Add the secrets and variables from §1 and §2 to **this environment**,
   not to the repository. Environment secrets reach only the jobs that
   declare the environment, and only after approval.
5. Add one workflow line, `environment: release-signing` on the `desktop`
   job of `release.yml`, through a source-edit request on the release
   branch. It is deliberately **not** in the candidate: a job that names an
   environment that does not exist yet would create it unprotected, and
   would hold every dry run for approval. The `secrets.*`/`vars.*`
   references need no change, because environment values use the same
   names.
6. Run the credentialed dry run (§3), approve it, and record its run and
   results in the acceptance record.

No pull request workflow can read these values. `release.yml` runs only on a
`v*` tag push or a manual dispatch.
