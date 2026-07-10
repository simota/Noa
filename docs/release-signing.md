# Release signing and notarization

Distribution builds of Noa are Developer ID code-signed and notarized by Apple so
that Gatekeeper accepts the downloaded `.app` without a prompt. Signing and
notarization run automatically in the `Release` workflow
(`.github/workflows/release.yml`) when a `v*` tag is pushed, provided the GitHub
secrets below are configured. When they are absent (forks, or a repo that has not
set them up), the workflow falls back to the previous behavior: an ad-hoc
signature with no notarization.

## Required GitHub secrets

| Secret | Purpose |
| --- | --- |
| `MACOS_SIGN_IDENTITY` | The signing identity string, e.g. `Developer ID Application: Your Name (TEAMID)`. Its presence is the switch that enables the whole signing path. |
| `MACOS_CERTIFICATE_P12` | Base64-encoded `.p12` export of the Developer ID Application certificate **and** its private key. |
| `MACOS_CERTIFICATE_PASSWORD` | The password protecting the `.p12` export. |
| `APPLE_API_KEY_ID` | App Store Connect API key ID (the `Key ID` column). |
| `APPLE_API_ISSUER_ID` | App Store Connect API issuer ID (shown above the keys table). |
| `APPLE_API_KEY` | The App Store Connect `.p8` private key, stored either as raw PEM text or base64-encoded. |

### Producing each value

**`MACOS_SIGN_IDENTITY`** — after installing the certificate locally, list it:

```bash
security find-identity -v -p codesigning
# -> "Developer ID Application: Your Name (TEAMID)"
```

**`MACOS_CERTIFICATE_P12`** — in Keychain Access, select the *Developer ID
Application* certificate together with its private key, export as
`certificate.p12` (set an export password), then base64-encode it:

```bash
base64 -i certificate.p12 | pbcopy   # paste as the secret value
```

Store the export password as `MACOS_CERTIFICATE_PASSWORD`.

**App Store Connect API key** — in App Store Connect → *Users and Access* →
*Integrations* → *App Store Connect API*, create a key with the *Developer* role
and download the `AuthKey_XXXXXXXXXX.p8` file (downloadable only once). Record the
`Key ID` (`APPLE_API_KEY_ID`) and the team's `Issuer ID` (`APPLE_API_ISSUER_ID`).
Store the key file contents as `APPLE_API_KEY` — either raw:

```bash
pbcopy < AuthKey_XXXXXXXXXX.p8
```

or base64-encoded (`base64 -i AuthKey_XXXXXXXXXX.p8 | pbcopy`). The workflow
detects which form was used.

## What the workflow does

1. Imports `MACOS_CERTIFICATE_P12` into a throwaway keychain with a random
   password, then deletes that keychain on completion (`if: always()`).
2. Runs `scripts/bundle-macos.sh release` with `NOA_SIGN_IDENTITY` set, which
   signs the inner binary and then the `.app` with a hardened runtime and a
   secure timestamp (inside-out; no `--deep`).
3. Verifies the signature, submits a `ditto`-zipped copy of the `.app` to
   `xcrun notarytool submit --wait`, staples the ticket into the bundle with
   `xcrun stapler staple`, and gates on `spctl -a -t exec -vv`.
4. Packages the stapled `.app` into the reproducible release zip. The
   `touch -t` normalization used for reproducible archives does not affect the
   signature (codesign does not depend on mtimes), and the stapled ticket travels
   inside the bundle, so it is included in the final zip.

## Testing signing locally

Sign a real bundle with your Developer ID identity:

```bash
NOA_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
  scripts/bundle-macos.sh release

codesign --verify --deep --strict --verbose=2 target/release/Noa.app
```

Then notarize and staple manually with the same API key:

```bash
ditto -c -k --keepParent target/release/Noa.app /tmp/Noa.zip
xcrun notarytool submit /tmp/Noa.zip \
  --key AuthKey_XXXXXXXXXX.p8 \
  --key-id "$APPLE_API_KEY_ID" \
  --issuer "$APPLE_API_ISSUER_ID" \
  --wait
xcrun stapler staple target/release/Noa.app
spctl -a -t exec -vv target/release/Noa.app   # -> accepted, source=Notarized Developer ID
```

Without `NOA_SIGN_IDENTITY`, `scripts/bundle-macos.sh` keeps ad-hoc signing
(`codesign --sign -`), which is enough to launch locally but is not distributable.
