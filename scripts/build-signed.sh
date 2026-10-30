#!/usr/bin/env bash
#
# Build a signed + notarized macOS bundle of drovr.
#
# `tauri build` signs with the Developer ID certificate named by
# APPLE_SIGNING_IDENTITY (the app and the bundled `drovr` CLI), and notarizes
# when the APPLE_API_* credentials are set. The identity is deliberately not in
# tauri.conf.json: there it would make every build, including unsigned local
# and CI builds, fail without the certificate.
#
# Credentials are read from the environment. To avoid committing secrets, put
# them in a git-ignored `.env.signing` (see .env.signing.example) — this script
# sources it automatically if present.
#
# Usage:
#   ./scripts/build-signed.sh
#   SIGN_ENV_FILE=/path/to/other.env ./scripts/build-signed.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

env_file="${SIGN_ENV_FILE:-.env.signing}"
if [[ -f "$env_file" ]]; then
  echo "Loading signing credentials from ${env_file}"
  set -a
  # shellcheck disable=SC1090
  source "$env_file"
  set +a
else
  echo "No ${env_file} found; relying on credentials already exported in the environment."
fi

required=(APPLE_SIGNING_IDENTITY APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH)
missing=()
for var in "${required[@]}"; do
  if [[ -z "${!var:-}" ]]; then
    missing+=("$var")
  fi
done

if (( ${#missing[@]} > 0 )); then
  echo "error: missing required env var(s): ${missing[*]}" >&2
  echo "Copy .env.signing.example to .env.signing and fill it in (see docs/signing.md)." >&2
  exit 1
fi

if [[ ! -f "${APPLE_API_KEY_PATH}" ]]; then
  echo "error: APPLE_API_KEY_PATH does not point to an existing file: ${APPLE_API_KEY_PATH}" >&2
  exit 1
fi

if ! security find-identity -v -p codesigning | grep -qF "\"${APPLE_SIGNING_IDENTITY}\""; then
  echo "error: no codesigning certificate named \"${APPLE_SIGNING_IDENTITY}\" in the keychain." >&2
  echo "Check it against: security find-identity -v -p codesigning" >&2
  exit 1
fi

echo "Signing identity : ${APPLE_SIGNING_IDENTITY}"
echo "API issuer       : ${APPLE_API_ISSUER}"
echo "API key id       : ${APPLE_API_KEY}"
echo "API key file     : ${APPLE_API_KEY_PATH}"
echo
echo "Building… notarization is a network round-trip to Apple and can take several minutes."

(cd app && pnpm tauri build --bundles app dmg)

app_path="target/release/bundle/macos/drovr.app"
echo
echo "Done. Bundle output is under target/release/bundle/"
echo "Verify with:"
echo "  codesign -dv --verbose=4 \"${app_path}\""
echo "  codesign -dv \"${app_path}/Contents/MacOS/drovr\"     # the bundled CLI agents run"
echo "  spctl -a -vvv -t install \"${app_path}\""
echo "  xcrun stapler validate \"${app_path}\""
