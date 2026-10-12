# Signing releases

Unsigned builds work, but Windows (SmartScreen, Smart App Control) and macOS (Gatekeeper)
warn about them or block them, and a Mac asks for Screen Recording and Accessibility again
after every update. Releases are signed in CI once the accounts below exist. Until then the
signing steps are skipped and releases come out unsigned, as before.

## Windows: SignPath Foundation (free for open source)

1. Apply at <https://signpath.org/apply.html>. The project qualifies: GPL-3.0, built
   from source on GitHub-hosted runners, released already. The README's "Code signing
   policy" section and the link to it in the release notes are what their terms ask for.
   Turn on multi-factor authentication for GitHub and for SignPath.
2. Once approved, in SignPath:
   - add the predefined trusted build system "GitHub.com" and link it to the project;
   - name the project `sw-freemonplay`, with a `release-signing` policy (manual approval);
   - create two artifact configurations from the files in `build/windows/signpath/`:
     `exes` (the two programs) and `installers` (the two setup programs).
3. In GitHub (Settings > Secrets and variables > Actions): the secret
   `SIGNPATH_API_TOKEN`, and the variable `SIGNPATH_ORGANIZATION_ID`.

A tagged release then sends two signing requests: the programs, which go into the
installers, then the installers. Approve each in SignPath; the Windows job waits up to five
hours for each. Windows shows "SignPath Foundation" as the publisher.

Not signed yet: the uninstaller Inno Setup writes (`unins000.exe`). Signing it takes
Inno Setup's `SignedUninstaller` setting and one more signing step.

## Mac: Apple Developer ID (Apple Developer Program, US$99 a year)

1. Join the Apple Developer Program, then in Certificates, IDs & Profiles create a
   **Developer ID Application** certificate. Export it with its private key from Keychain
   Access as a `.p12` with a password.
2. In App Store Connect (Users and Access > Integrations > App Store Connect API) create a
   key with the Developer role. Note its key ID and the issuer ID, and download the `.p8`.
3. Add these GitHub secrets:

   | Secret | Value |
   |---|---|
   | `MACOS_CERT_P12_BASE64` | `base64 -i cert.p12` |
   | `MACOS_CERT_PASSWORD` | the `.p12` password |
   | `MACOS_SIGN_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
   | `ASC_KEY_ID` | the API key's ID |
   | `ASC_ISSUER_ID` | the issuer ID |
   | `ASC_KEY_P8_BASE64` | `base64 -i AuthKey_XXXX.p8` |

A tagged release then signs both apps with the hardened runtime (`bundle.sh` with
`FMP_SIGN_IDENTITY`), and `build/macos/notarize.sh` notarizes and staples each app and its
disk image. Neither app needs entitlements: Screen Recording and Accessibility are
permissions people grant, not entitlements.

The first signed release asks for Screen Recording and Accessibility one more time (the
signature changes from ad hoc to Developer ID). After that the permissions survive updates,
as long as the bundle IDs and the team stay the same.
