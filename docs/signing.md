# macOS code signing & notarization

drovr is distributed directly (outside the Mac App Store), so release builds are
signed with a **Developer ID Application** certificate and **notarized** by Apple.
Notarizing means any Mac will open the app without a Gatekeeper warning.

## What's already configured

- **Signing** uses the certificate named by the `APPLE_SIGNING_IDENTITY`
  environment variable, which Tauri reads during `tauri build`. Yours (the same certificate local-wiki/Ink uses) is:

  ```
  Developer ID Application: IZZ JEREMY,LEE NOLAND (S5G2VY53V5)
  ```

  Team ID: `S5G2VY53V5`. Tauri signs with the hardened runtime automatically
  (a prerequisite for notarization), so no custom entitlements file is needed.

  The identity is not in `tauri.conf.json` (`bundle.macOS.signingIdentity`):
  there it would make every build need the certificate, so a plain build on
  another machine or in CI would fail. Without the variable, builds are simply
  unsigned.

- **Notarization** runs automatically during `tauri build` whenever the
  App Store Connect API credentials are present in the environment.

- The bundled `drovr` CLI (the sidecar in `drovr.app/Contents/MacOS/drovr`,
  which every agent runs as its MCP server) is signed along with the app.

- [`scripts/build-signed.sh`](../scripts/build-signed.sh) wraps the build: it
  loads credentials, validates them, then runs `pnpm tauri build --bundles app dmg`.

## One-time setup

### 1. Confirm the certificate is installed

```bash
security find-identity -v -p codesigning
```

You should see `Developer ID Application: IZZ JEREMY,LEE NOLAND (S5G2VY53V5)`.
It's already present on this machine. If you ever set up a new Mac, install the
certificate + its private key from your Apple Developer account (or export a
`.p12` from the original machine's Keychain and import it).

### 2. Create an App Store Connect API key (for notarization)

1. Go to <https://appstoreconnect.apple.com/access/integrations/api>
   (**Users and Access → Integrations → App Store Connect API**).
2. Create a key with the **Developer** role (sufficient for notarization).
3. Note the **Issuer ID** (a UUID, shown above the key list) and the
   **Key ID** (10 characters, next to the key).
4. Download the `AuthKey_<KEYID>.p8` file. **Apple lets you download it only
   once.** Store it somewhere safe outside the repo, e.g.
   `~/.appstoreconnect/private_keys/`.

### 3. Provide the credentials to the build

Copy the template and fill it in. If you already have a `.env.signing` for
local-wiki, the same values work, so you can copy that file instead:

```bash
cp .env.signing.example .env.signing     # or: cp ../local-wiki/.env.signing .env.signing
```

Edit `.env.signing`:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: IZZ JEREMY,LEE NOLAND (S5G2VY53V5)"
export APPLE_API_ISSUER="<your issuer UUID>"
export APPLE_API_KEY="<your 10-char key id>"
export APPLE_API_KEY_PATH="$HOME/.appstoreconnect/private_keys/AuthKey_<KEYID>.p8"
```

`.env.signing` and `*.p8` files are git-ignored — never commit them.

## Building a release

```bash
cd app && pnpm tauri:build:signed     # or ./scripts/build-signed.sh from the repo root
```

Notarization is a network round-trip to Apple and can take a few minutes. On
success, the signed + notarized, stapled artifacts are written to:

```
target/release/bundle/dmg/drovr_1.0.1_aarch64.dmg
target/release/bundle/macos/drovr.app
```

(The exact filename reflects the version and your CPU architecture.)

## Releasing from CI

Releases come from [release-please](https://github.com/googleapis/release-please) (`.github/workflows/release-please.yml`):

1. Merge work into `main` with Conventional Commit messages (`fix: …`, `feat: …`). Squash-merged PRs use the PR title, so title PRs that way.
2. release-please opens or updates a PR named like `chore(main): release 0.6.0`, with the version bumped everywhere and the `CHANGELOG.md` entry.
3. Merge that PR when you want to ship. release-please tags it and creates the GitHub Release, and `.github/workflows/release.yml` then builds the same signed, notarized bundle on GitHub, attaches the `.dmg` and titles the release `drovr-<version>`.

One-time repository settings:

- **Settings → Actions → General → Workflow permissions:** turn on **Allow GitHub Actions to create and approve pull requests**, so release-please can open its PR.
- PRs opened with the built-in token don't trigger other workflows, and CI is a required check on `main`. So after updating the release PR, `release-please.yml` starts CI on its branch with `workflow_dispatch`, which the built-in token is allowed to trigger.

There's no releasing by hand: `release.yml` only runs when release-please calls it, so pushing a `v*` tag builds nothing. The **release tags** ruleset, which has no bypass, stops anyone moving or deleting a `v*` tag. It can't restrict creating them as well, because on a personal account's repo the built-in GitHub Actions app can't be a bypass actor, and release-please needs to create them.

The job runs in the GitHub environment `release`, which holds the signing secrets and only deploys from `main`, so a workflow on any other branch can't read them. To make each release wait for your approval, add yourself as a required reviewer under **Settings → Environments → release**.

### Secrets

Add these under **Settings → Secrets and variables → Actions**, either as repository secrets or as secrets of the `release` environment:

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Your Developer ID Application certificate and its private key, exported as a `.p12` and base64-encoded (see below) |
| `APPLE_CERTIFICATE_PASSWORD` | The password you set when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: IZZ JEREMY,LEE NOLAND (S5G2VY53V5)` |
| `APPLE_API_ISSUER` | Same as in `.env.signing` |
| `APPLE_API_KEY` | Same as in `.env.signing` (the 10-character key ID) |
| `APPLE_API_KEY_P8` | The contents of your `AuthKey_<KEYID>.p8` file |

To export the certificate: in Keychain Access, open **login → My Certificates**, right-click **Developer ID Application: IZZ JEREMY,LEE NOLAND (S5G2VY53V5)**, choose **Export…**, save it as a `.p12`, and set a password. Then:

```bash
openssl base64 -A -in DeveloperID.p12 | pbcopy             # paste as APPLE_CERTIFICATE
pbcopy < ~/.appstoreconnect/private_keys/AuthKey_XXXXXXXXXX.p8   # paste as APPLE_API_KEY_P8
rm DeveloperID.p12
```

What the job does:

1. Checks that the tag matches the app version.
2. Imports the certificate into a temporary keychain, and checks that `APPLE_SIGNING_IDENTITY` is in it.
3. Builds, signs and notarizes the app.
4. Verifies the result with `codesign`, `spctl` and `stapler`.
5. Attaches the `.dmg` to the release and titles it `drovr-<version>`.
6. Deletes the keychain and the key file, even if an earlier step failed.

Pull-request CI (`.github/workflows/ci.yml`) never signs and builds no bundle: it only runs the tests and the type-check.

## Verifying the result

```bash
# Signature + hardened runtime
codesign -dv --verbose=4 "target/release/bundle/macos/drovr.app"

# Gatekeeper acceptance (should say: accepted / source=Notarized Developer ID)
spctl -a -vvv -t install "target/release/bundle/macos/drovr.app"

# The bundled CLI is signed too
codesign -dv "target/release/bundle/macos/drovr.app/Contents/MacOS/drovr"

# Notarization ticket is stapled to the app
xcrun stapler validate "target/release/bundle/macos/drovr.app"
```

## Troubleshooting

- **`The specified item could not be found in the keychain`** — the signing
  identity in `APPLE_SIGNING_IDENTITY` doesn't match an installed certificate.
  Re-check `security find-identity -v -p codesigning` and copy the name exactly.
  `scripts/build-signed.sh` checks this before building.
- **Notarization fails with an auth error** — verify `APPLE_API_ISSUER`,
  `APPLE_API_KEY`, and `APPLE_API_KEY_PATH`. The `.p8` path must be absolute and
  point to an existing file.
- **See why notarization was rejected** — Tauri prints a submission ID; inspect it with:

  ```bash
  xcrun notarytool log <submission-id> \
    --issuer "$APPLE_API_ISSUER" --key-id "$APPLE_API_KEY" --key "$APPLE_API_KEY_PATH"
  ```

- **Alternative credential method** — instead of an API key you can use an Apple
  ID with an app-specific password by setting `APPLE_ID`, `APPLE_PASSWORD`, and
  `APPLE_TEAM_ID` (`S5G2VY53V5`). The API key is preferred and is what the script
  checks for.
