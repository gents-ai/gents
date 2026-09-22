#!/usr/bin/env bash
set -euo pipefail

[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]]
: "${RUNNER_TEMP:?}" "${CARGO_TARGET_DIR:?}" "${RELEASE_VERSION:?}"
: "${APPLE_SIGNING_IDENTITY:?}" "${APPLE_API_KEY:?}" "${APPLE_API_ISSUER:?}"
: "${MACOS_NOTARY_API_KEY:?}"

npm ci
node scripts/check-desktop-package-boundaries.mjs
[[ "$(node -p 'require("./package.json").version')" == "$RELEASE_VERSION" ]]

# The release workflow already built and signed the CLI in this target dir.
# Stage that exact binary as Tauri's target-suffixed sidecar.
bash scripts/stage-tauri-sidecar.sh --source "$CARGO_TARGET_DIR/release/gents"

notary_dir=$(mktemp -d "$RUNNER_TEMP/gents-desktop-notary.XXXXXX")
export APPLE_API_KEY_PATH="$notary_dir/AuthKey_${APPLE_API_KEY}.p8"
trap 'rm -f "$APPLE_API_KEY_PATH"; rmdir "$notary_dir"' EXIT
umask 077
printf '%s\n' "$MACOS_NOTARY_API_KEY" > "$APPLE_API_KEY_PATH"
unset MACOS_NOTARY_API_KEY
umask 022

# Reuse the CLI job's signing identity/keychain; Tauri signs and notarizes the app.
(
  cd apps/gents-desktop
  npm run tauri -- build --config src-tauri/tauri.bundle.conf.json --ci --bundles app,dmg -- --locked
)
app="$CARGO_TARGET_DIR/release/bundle/macos/Gents.app"
codesign --verify --deep --strict --verbose=2 "$app"
codesign -d --entitlements :- "$app" 2>/dev/null | \
  python3 -c 'import plistlib,sys; assert plistlib.loads(sys.stdin.buffer.read()).get("com.apple.security.cs.allow-unsigned-executable-memory") is True'
xcrun stapler validate "$app"
spctl --assess --type execute --verbose=4 "$app"
[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")" == "$RELEASE_VERSION" ]]
[[ "$(lipo -archs "$app/Contents/MacOS/gents-desktop-tauri")" == arm64 ]]
bundled_cli="$app/Contents/MacOS/gents"
[[ -x "$bundled_cli" ]]
"$bundled_cli" version | grep -F "$RELEASE_VERSION"

shopt -s nullglob
images=("$CARGO_TARGET_DIR"/release/bundle/dmg/*"${RELEASE_VERSION}"*.dmg)
[[ ${#images[@]} == 1 ]]
# Staple the installer too, so installation does not depend on a Gatekeeper lookup.
codesign --force --sign "$APPLE_SIGNING_IDENTITY" --timestamp "${images[0]}"
codesign --verify --strict "${images[0]}"
xcrun notarytool submit "${images[0]}" --key "$APPLE_API_KEY_PATH" \
  --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait
xcrun stapler staple "${images[0]}"
xcrun stapler validate "${images[0]}"
hdiutil verify "${images[0]}"

mount_dir=$(mktemp -d "$RUNNER_TEMP/gents-desktop-mount.XXXXXX")
smoke_dir=$(mktemp -d "$RUNNER_TEMP/gents-desktop-smoke.XXXXXX")
trap 'hdiutil detach "$mount_dir" >/dev/null 2>&1 || true; rm -f "$APPLE_API_KEY_PATH"; rmdir "$notary_dir"' EXIT
hdiutil attach -readonly -nobrowse -mountpoint "$mount_dir" "${images[0]}"
ditto "$mount_dir/Gents.app" "$smoke_dir/Gents.app"
hdiutil detach "$mount_dir"
codesign --verify --deep --strict "$smoke_dir/Gents.app"
spctl --assess --type execute "$smoke_dir/Gents.app"
[[ -x "$smoke_dir/Gents.app/Contents/MacOS/gents" ]]
"$smoke_dir/Gents.app/Contents/MacOS/gents" version | grep -F "$RELEASE_VERSION"
node --input-type=module - "$smoke_dir" <<'NODE'
import { spawnSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import assert from 'node:assert/strict';
const root = process.argv[2];
const result = spawnSync(`${root}/Gents.app/Contents/MacOS/gents-desktop-tauri`, [], {
  cwd: root,
  env: { ...process.env, GENTS_HOME: `${root}/agent`, GENTS_DESKTOP_HOME: `${root}/desktop` },
  encoding: 'utf8', timeout: 20000, killSignal: 'SIGKILL', maxBuffer: 4 * 1024 * 1024,
});
const log = `${result.stdout ?? ''}\n${result.stderr ?? ''}`;
writeFileSync(`${root}/startup.log`, log);
assert.equal(result.error?.code, 'ETIMEDOUT', `Desktop exited before smoke deadline: ${log}`);
assert.equal(result.signal, 'SIGKILL', `Desktop exited before smoke termination: ${log}`);
assert.doesNotMatch(log, /panicked at|failed to (initialize|create).*webview/i);
console.log(`Installed desktop stayed running for 20 seconds; diagnostics: ${root}/startup.log`);
NODE

dist="$RUNNER_TEMP/gents-desktop-dist"
mkdir -p "$dist"
cp "${images[0]}" "$dist/gents-desktop_${RELEASE_VERSION}_aarch64.dmg"
cp docs/macos-desktop-install.md "$dist/INSTALL-macos-desktop.md"
(
  cd "$dist"
  shasum -a 256 "gents-desktop_${RELEASE_VERSION}_aarch64.dmg" > SHA256SUMS-desktop-macos.txt
)
