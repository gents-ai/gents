#!/usr/bin/env bash
set -euo pipefail

[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]]
: "${RUNNER_TEMP:?}" "${CARGO_TARGET_DIR:?}" "${RELEASE_VERSION:?}"
: "${APPLE_SIGNING_IDENTITY:?}"
notarization_enabled="${NOTARIZATION_ENABLED:-true}"
case "$notarization_enabled" in
  true) : "${APPLE_API_KEY:?}" "${APPLE_API_ISSUER:?}" "${MACOS_NOTARY_API_KEY:?}" ;;
  false)
    [[ "${GITHUB_EVENT_NAME:-}" == workflow_dispatch && "${GITHUB_REF_TYPE:-}" == branch ]]
    unset APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID MACOS_NOTARY_API_KEY
    ;;
  *) echo "Invalid NOTARIZATION_ENABLED" >&2; exit 1 ;;
esac
notary_dir=""
cleanup() {
  if [[ -n "${mount_dir:-}" ]]; then hdiutil detach "$mount_dir" >/dev/null 2>&1 || true; fi
  if [[ -n "$notary_dir" ]]; then rm -f "$APPLE_API_KEY_PATH"; rmdir "$notary_dir"; fi
}
trap cleanup EXIT

npm ci
node scripts/check-desktop-package-boundaries.mjs
[[ "$(node -p 'require("./package.json").version')" == "$RELEASE_VERSION" ]]

bash scripts/stage-tauri-sidecar.sh --source "$CARGO_TARGET_DIR/release/gents"

if [[ "$notarization_enabled" == true ]]; then
  notary_dir=$(mktemp -d "$RUNNER_TEMP/gents-desktop-notary.XXXXXX")
  export APPLE_API_KEY_PATH="$notary_dir/AuthKey_${APPLE_API_KEY}.p8"
  umask 077
  printf '%s\n' "$MACOS_NOTARY_API_KEY" > "$APPLE_API_KEY_PATH"
  unset MACOS_NOTARY_API_KEY
  umask 022

fi

(
  cd apps/gents-desktop
  npm run tauri -- build --config src-tauri/tauri.bundle.conf.json --ci --bundles app,dmg -- --locked
)
app="$CARGO_TARGET_DIR/release/bundle/macos/Gents.app"
codesign --verify --deep --strict --verbose=2 "$app"
codesign -d --entitlements :- "$app" 2>/dev/null | \
  python3 -c 'import plistlib,sys; assert plistlib.loads(sys.stdin.buffer.read()).get("com.apple.security.cs.allow-unsigned-executable-memory") is True'
for executable in "$app" "$app/Contents/MacOS/gents-desktop-tauri" "$app/Contents/MacOS/gents" "$app/Contents/MacOS/bridge_runner"; do
  [[ -e "$executable" ]]
  if [[ -f "$executable" ]]; then [[ "$(lipo -archs "$executable")" == arm64 ]]; fi
  codesign --verify --strict "$executable"
  codesign -dvv "$executable" 2>&1 | grep -Fx 'TeamIdentifier=NXJ753WW5X'
  codesign -d -r- "$executable" 2>&1 | grep -F 'anchor apple generic'
done
if [[ "$notarization_enabled" == true ]]; then
  xcrun stapler validate "$app"
  spctl --assess --type execute --verbose=4 "$app"
fi
[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")" == "$RELEASE_VERSION" ]]
[[ "$(lipo -archs "$app/Contents/MacOS/gents-desktop-tauri")" == arm64 ]]
bundled_cli="$app/Contents/MacOS/gents"
[[ -x "$bundled_cli" ]]
"$bundled_cli" version | grep -F "$RELEASE_VERSION"
"$bundled_cli" version | grep -F "${GENTS_BUILD_GIT_SHA:?}"

shopt -s nullglob
images=("$CARGO_TARGET_DIR"/release/bundle/dmg/*"${RELEASE_VERSION}"*.dmg)
[[ ${#images[@]} == 1 ]]
codesign --force --sign "$APPLE_SIGNING_IDENTITY" --timestamp "${images[0]}"
codesign --verify --strict "${images[0]}"
if [[ "$notarization_enabled" == true ]]; then
  xcrun notarytool submit "${images[0]}" --key "$APPLE_API_KEY_PATH" \
    --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait
  xcrun stapler staple "${images[0]}"
  xcrun stapler validate "${images[0]}"
fi
hdiutil verify "${images[0]}"

mount_dir=$(mktemp -d "$RUNNER_TEMP/gents-desktop-mount.XXXXXX")
smoke_dir=$(mktemp -d "$RUNNER_TEMP/gents-desktop-smoke.XXXXXX")

hdiutil attach -readonly -nobrowse -mountpoint "$mount_dir" "${images[0]}"
ditto "$mount_dir/Gents.app" "$smoke_dir/Gents.app"
hdiutil detach "$mount_dir"
codesign --verify --deep --strict "$smoke_dir/Gents.app"
if [[ "$notarization_enabled" == true ]]; then spctl --assess --type execute "$smoke_dir/Gents.app"; fi
[[ -x "$smoke_dir/Gents.app/Contents/MacOS/gents" ]]
"$smoke_dir/Gents.app/Contents/MacOS/gents" version | grep -F "$RELEASE_VERSION"
if [[ "$notarization_enabled" == true ]]; then
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

fi

suffix=""
if [[ "$notarization_enabled" == false ]]; then
  suffix="-signed-unnotarized-candidate"
  printf '%s\n' 'Developer ID signed, unnotarized candidate. First launch may require System Settings > Privacy & Security > Open Anyway.' >> "$GITHUB_STEP_SUMMARY"
fi
dist="$RUNNER_TEMP/gents-desktop-dist"
mkdir -p "$dist"
if [[ "$notarization_enabled" == false ]]; then
python3 - "$app" "$dist" <<'PYTHON'
import hashlib, json, os, pathlib
app, dist = map(pathlib.Path, __import__('sys').argv[1:])
files = {}
for name in ('gents-desktop-tauri', 'gents', 'bridge_runner'):
    path = app / 'Contents' / 'MacOS' / name
    files[name] = hashlib.file_digest(path.open('rb'), 'sha256').hexdigest()
manifest = {'sha': os.environ['GENTS_BUILD_GIT_SHA'],
            'version': os.environ['RELEASE_VERSION'], 'arch': 'arm64',
            'notarized': os.environ.get('NOTARIZATION_ENABLED', 'true') == 'true',
            'team_id': 'NXJ753WW5X', 'binaries_sha256': files}
(dist / 'candidate-build.json').write_text(json.dumps(manifest, indent=2) + '\n')
PYTHON
fi
cp "${images[0]}" "$dist/gents-desktop_${RELEASE_VERSION}_aarch64${suffix}.dmg"
(
  cd "$dist"
  sums_inputs=("gents-desktop_${RELEASE_VERSION}_aarch64${suffix}.dmg")
  if [[ "$notarization_enabled" == false ]]; then sums_inputs+=(candidate-build.json); fi
  shasum -a 256 "${sums_inputs[@]}" > SHA256SUMS-desktop-macos.txt
)
