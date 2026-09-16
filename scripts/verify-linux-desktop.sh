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
if ldd "$binary" | grep -q 'not found'; then
  echo "Installed desktop has unresolved shared libraries" >&2
  exit 1
fi
required=$(objdump -T "$binary" | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sed 's/GLIBC_//' | sort -Vu | tail -1)
[[ -n "$required" && "$(printf '%s\n' 2.36 "$required" | sort -V | tail -1)" == 2.36 ]]

# Fresh ordinary user: no source checkout, Vite, CLI, or pre-existing agent home.
useradd --create-home --shell /bin/bash gents-desktop-smoke
set +e
timeout --kill-after=5s 20s runuser -u gents-desktop-smoke -- \
  env GENTS_HOME=/home/gents-desktop-smoke/agent \
      GENTS_DESKTOP_HOME=/home/gents-desktop-smoke/desktop \
  bash -c 'cd /home/gents-desktop-smoke && exec dbus-run-session -- xvfb-run -a /usr/bin/gents-desktop-tauri' \
  > target/desktop-smoke.log 2>&1
status=$?
set -e
if [[ "$status" != 124 ]]; then
  cat target/desktop-smoke.log
  echo "Desktop exited before the startup smoke deadline (status $status)" >&2
  exit 1
fi
if grep -Ei 'panicked at|failed to (initialize|create).*webview' target/desktop-smoke.log; then
  exit 1
fi

mkdir -p target/desktop-dist
cp "${debs[0]}" "target/desktop-dist/gents-desktop_${version}_amd64.deb"
cp "${images[0]}" "target/desktop-dist/gents-desktop_${version}_x86_64.AppImage"
cp docs/linux-desktop-install.md target/desktop-dist/INSTALL-linux-desktop.md
(
  cd target/desktop-dist
  sha256sum ./*.deb ./*.AppImage > SHA256SUMS-desktop-linux.txt
)
