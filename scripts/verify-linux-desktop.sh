#!/usr/bin/env bash
set -euo pipefail

# Run inside the disposable Debian release container, not on a developer host.
[[ "$(uname -m)" == x86_64 && "$(id -u)" == 0 ]]
shopt -s nullglob
debs=(target/release/bundle/deb/*.deb)
images=(target/release/bundle/appimage/*.AppImage)
[[ ${#debs[@]} == 1 && ${#images[@]} == 1 ]]
version=$(node -p 'require("./package.json").version')
[[ "$(dpkg-deb -f "${debs[0]}" Architecture)" == amd64 ]]
[[ "$(dpkg-deb -f "${debs[0]}" Version)" == "$version" ]]
dpkg -i "${debs[0]}"
binary=/usr/bin/gents-desktop-tauri
[[ -x "$binary" ]]
cli=/usr/bin/gents
[[ -x "$cli" ]]
"$cli" version | grep -F "$version"
if ldd "$binary" | grep -q 'not found'; then
  echo "Installed desktop has unresolved shared libraries" >&2
  exit 1
fi
required=$(objdump -T "$binary" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sed 's/GLIBC_//' | sort -Vu | tail -1)
[[ -n "$required" && "$(printf '%s\n' 2.36 "$required" | sort -V | tail -1)" == 2.36 ]]

smoke() {
  local label="$1" executable="$2" smoke_root smoke_user status log
  # Each installer gets an ordinary user and fresh home, without Vite or a CLI.
  smoke_root=$(mktemp -d /tmp/gents-desktop-smoke.XXXXXX)
  smoke_user="gents-smoke-${smoke_root##*.}"
  log="target/desktop-smoke-${label}.log"
  useradd --home-dir "$smoke_root" --no-create-home --shell /bin/bash "$smoke_user"
  chown "$smoke_user" "$smoke_root"
  install -d -o "$smoke_user" -m 0700 "$smoke_root/runtime" "$smoke_root/tmp"
  set +e
  # shellcheck disable=SC2016 # The non-root shell resolves its positional arguments.
  timeout --kill-after=5s 20s runuser -u "$smoke_user" -- \
    env GENTS_HOME="$smoke_root/agent" GENTS_DESKTOP_HOME="$smoke_root/desktop" \
    XDG_RUNTIME_DIR="$smoke_root/runtime" TMPDIR="$smoke_root/tmp" APPIMAGE_EXTRACT_AND_RUN=1 \
    bash -c 'cd "$1" && exec dbus-run-session -- xvfb-run -a "$2"' bash "$smoke_root" "$executable" \
    > "$log" 2>&1
  status=$?
  set -e
  if [[ "$status" != 124 ]]; then
    cat "$log"
    echo "$label exited before the startup smoke deadline (status $status)" >&2
    exit 1
  fi
  if grep -Ei 'panicked at|failed to (initialize|create).*webview' "$log"; then
    exit 1
  fi
}
smoke deb "$binary"
chmod +x "${images[0]}"
appimage_path=$(realpath "${images[0]}")
smoke appimage "$appimage_path"

# The bundled CLI must run with no AppImage environment, because the user
# service runs a copy of it that outlives the mount. No service is installed.
appimage_extract=$(mktemp -d /tmp/gents-appimage-extract.XXXXXX)
(
  cd "$appimage_extract"
  "$appimage_path" --appimage-extract >/dev/null
)
appimage_cli=$(find "$appimage_extract/squashfs-root" -type f -name gents -perm -u+x -print -quit)
[[ -n "$appimage_cli" ]]
env -u APPDIR -u APPIMAGE -u LD_LIBRARY_PATH "$appimage_cli" version | grep -F "$version"
rm -rf -- "$appimage_extract"

mkdir -p target/desktop-dist
cp "${debs[0]}" "target/desktop-dist/gents-desktop_${version}_amd64.deb"
cp "${images[0]}" "target/desktop-dist/gents-desktop_${version}_x86_64.AppImage"
cp docs/linux-desktop-install.md target/desktop-dist/INSTALL-linux-desktop.md
(
  cd target/desktop-dist
  sha256sum ./*.deb ./*.AppImage > SHA256SUMS-desktop-linux.txt
)
