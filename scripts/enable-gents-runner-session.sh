#!/usr/bin/env bash
set -euo pipefail

plist="${1:-/Library/LaunchDaemons/com.github.actions.runner.defra-agent.plist}"

if [[ ! -f "${plist}" ]]; then
  echo "Runner LaunchDaemon plist not found: ${plist}" >&2
  exit 1
fi

label="$(/usr/libexec/PlistBuddy -c 'Print :Label' "${plist}")"

if /usr/libexec/PlistBuddy -c 'Print :SessionCreate' "${plist}" >/dev/null 2>&1; then
  sudo /usr/libexec/PlistBuddy -c 'Set :SessionCreate true' "${plist}"
else
  sudo /usr/libexec/PlistBuddy -c 'Add :SessionCreate bool true' "${plist}"
fi

sudo plutil -lint "${plist}"
sudo launchctl bootout system "${plist}" 2>/dev/null || true
sudo launchctl bootstrap system "${plist}"
sudo launchctl kickstart -k "system/${label}" 2>/dev/null || true
sudo launchctl print "system/${label}" | sed -n '1,80p'
