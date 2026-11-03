#!/bin/sh
# First run of the released macOS DMG on a clean Mac, driven through the
# Accessibility API with ./ax (built from ax.swift). Needs a logged-in console
# session, and the Accessibility and Screen Recording permissions for the SSH
# session that runs it (sshd-keygen-wrapper).
#
#   first-run.sh <tag> <inference-url>
#
# Environment: FRL_OUT (report directory), FRL_MODEL (substring; default the
# first advertised model), FRL_LOGIN_PASSWORD (answers the login keychain
# prompt the way the user would).
#
# Every system prompt it answers is recorded under "prompts" with its text and
# a screenshot, so a prompt a user should never see still shows up in the
# report.
set -u
tag=${1:?usage: first-run.sh <tag> <inference-url>}
url=${2:?usage: first-run.sh <tag> <inference-url>}
out=${FRL_OUT:-$HOME/frl/out}
model=${FRL_MODEL:-}
password=${FRL_LOGIN_PASSWORD:-}
ax="$(cd "$(dirname "$0")" && pwd)/ax"
dmg="gents-desktop_${tag#v}_aarch64.dmg"
uid=$(id -u)
token="first-run-$(date +%s)"
mkdir -p "$out"
steps="" prompts="" failure="" shots=0

escape() { printf '%s' "$1" | tr '\n' ' ' | sed 's/\\/\\\\/g; s/"/\\"/g'; }

report() {
  if [ -n "$failure" ]; then fail_json="\"$(escape "$failure")\""; else fail_json=null; fi
  printf '{"tag":"%s","token":"%s","steps":[%s],"prompts":[%s],"failure":%s}\n' \
    "$tag" "$token" "${steps#,}" "${prompts#,}" "$fail_json" > "$out/report.json"
}

# Sets $picture. Steps and prompts run in this shell, never in $(...), so the
# counter and the report lists survive.
shot() {
  shots=$((shots + 1))
  picture=$(printf '%02d-%s' "$shots" "$1")
  screencapture -x "$out/$picture.png"
  "$ax" text Gents > "$out/$picture.txt" 2> /dev/null
}

step() {
  name=$1
  shift
  started=$(date +%s)
  if "$@" > "$out/.detail" 2>&1; then ok=true; else ok=false; fi
  detail=$(cat "$out/.detail")
  [ "$ok" = true ] || failure="$name: $detail"
  ms=$((($(date +%s) - started) * 1000))
  shot "$name"
  steps="$steps,{\"name\":\"$name\",\"ok\":$ok,\"ms\":$ms,\"detail\":\"$(escape "$detail")\",\"screenshot\":\"$picture\"}"
  report
  echo "$( [ "$ok" = true ] && echo PASS || echo FAIL) $name ${ms}ms $detail"
  [ "$ok" = true ] || exit 1
}

# Answers a system prompt if one is up, and records it. Returns 0 if it did.
answer() { # <prompt name> <process> <button>
  text=$("$ax" dump "$2" 2> /dev/null | grep '"role":"AXStaticText"' | sed 's/.*"value":"\(.*\)"}$/\1/' | head -2 | tr '\n' ' ')
  [ -n "$text" ] || return 1
  "$ax" wait "$2" "AXButton=$3" 1 > /dev/null 2>&1 || return 1
  shot "prompt-$1"
  prompts="$prompts,{\"name\":\"$1\",\"text\":\"$(escape "$text")\",\"screenshot\":\"$picture\"}"
  if [ "$1" = keychain ]; then
    [ -n "$password" ] || { echo "login keychain prompt and no FRL_LOGIN_PASSWORD"; return 2; }
    "$ax" set SecurityAgent AXTextField= "$password" || return 2
  fi
  "$ax" click "$2" "AXButton=$3"
  report
}

download() {
  mkdir -p "$HOME/Downloads" && cd "$HOME/Downloads" || return 1
  release="https://github.com/gents-ai/gents/releases/download/$tag"
  curl -fsSLO "$release/$dmg" && curl -fsSLO "$release/SHA256SUMS-desktop-macos.txt" || return 1
  grep -F "$dmg" SHA256SUMS-desktop-macos.txt | shasum -a 256 -c - || return 1
  grep -F "$dmg" SHA256SUMS-desktop-macos.txt | awk '{print $1}' > "$out/artifact-sha256.txt"
  echo "$dmg" > "$out/artifact-name.txt"
  # What a browser download carries; Gatekeeper and App Translocation key on it.
  xattr -w com.apple.quarantine "0083;$(printf %x "$(date +%s)");Safari;$(uuidgen)" "$dmg"
}

open_dmg() {
  sudo launchctl asuser "$uid" open "$HOME/Downloads/$dmg"
  "$ax" wait Finder AXImage=Applications 120 > /dev/null && "$ax" wait Finder AXImage=Gents 10 > /dev/null
}

drag_to_applications() {
  "$ax" drag Finder AXImage=Gents AXImage=Applications || return 1
  binary=/Applications/Gents.app/Contents/MacOS/gents-desktop-tauri
  last=-1
  for _ in $(seq 1 90); do
    size=$(du -sk /Applications/Gents.app 2> /dev/null | cut -f1)
    [ -x "$binary" ] && [ "$size" = "$last" ] && break
    last=$size
    sleep 2
  done
  [ -x "$binary" ] || { echo "Gents.app never arrived in /Applications"; return 1; }
  hdiutil detach -quiet "/Volumes/Gents" 2> /dev/null
  echo "quarantine $(xattr -p com.apple.quarantine /Applications/Gents.app | cut -d';' -f1); $(spctl -a -vv -t execute /Applications/Gents.app 2>&1 | tr '\n' ' ')"
}

launch() {
  sudo launchctl asuser "$uid" open -a /Applications/Gents.app
  for _ in $(seq 1 60); do
    answer gatekeeper CoreServicesUIAgent Open && break
    "$ax" wait Gents "#setup-node-name" 1 > /dev/null 2>&1 && break
    sleep 1
  done
  "$ax" wait Gents "#setup-node-name" 120 > /dev/null || return 1
  ps -axo command | grep '[g]ents-desktop-tauri' | grep -q AppTranslocation && { echo "running translocated"; return 1; }
  return 0
}

welcome() {
  "$ax" wait Gents "#managed-tool-root" 60 > /dev/null || return 1
  echo "node name $("$ax" value Gents '#setup-node-name'); tool root $("$ax" value Gents '#managed-tool-root'); ceiling $("$ax" value Gents 'AXComboBox=Tool ceiling')"
}

start_node() {
  "$ax" click Gents AXButton=Next || return 1
  for _ in $(seq 1 300); do
    "$ax" wait Gents "AXRadioButton=~Ollama" 1 > /dev/null 2>&1 && return 0
    answer keychain SecurityAgent "Always Allow"
    [ $? -eq 2 ] && return 1
    if "$ax" text Gents 2> /dev/null | grep -q "Startup paused"; then
      "$ax" text Gents | sed -n '1,6p' | tr '\n' ' '
      return 1
    fi
    sleep 1
  done
  echo "no provider choice after 300s"
  return 1
}

connect_provider() {
  "$ax" click Gents "AXRadioButton=~Ollama" && sleep 1 || return 1
  "$ax" type Gents AXTextField=Endpoint "$url" && sleep 0.5 || return 1
  [ "$("$ax" value Gents AXTextField=Endpoint)" = "$url" ] || { echo "endpoint reads $("$ax" value Gents AXTextField=Endpoint)"; return 1; }
}

choose_model() {
  "$ax" click Gents "AXButton=Connect and find models" || return 1
  "$ax" wait Gents "AXList=Advertised models" 120 > /dev/null || return 1
  sleep 1
  models=$("$ax" dump Gents | sed -n -E '/"name":"Advertised models","role":"AXList"/,/"role":"AX(RadioButton|Button|Heading)"/p' |
    grep '"role":"AXStaticText"' | sed 's/.*"name":"\([^"]*\)".*/\1/' | grep -v '^$')
  pick=$(printf '%s\n' "$models" | grep -F "${model}" | head -1)
  [ -n "$pick" ] || { echo "no advertised model matches '$model': $(echo $models)"; return 1; }
  "$ax" click Gents "AXStaticText=$pick" || return 1
  "$ax" wait Gents "AXButton=Change model" 30 > /dev/null || return 1
  echo "models $(echo $models); chose $pick"
}

save_inference() {
  "$ax" click Gents "AXButton=Save and start chatting" || return 1
  "$ax" wait Gents "*=Message" 180 > /dev/null
}

first_message() {
  "$ax" type Gents "*=Message" "Reply with exactly this token and nothing else: $token" && sleep 0.5 || return 1
  "$ax" click Gents AXButton=Send || return 1
  for _ in $(seq 1 240); do
    "$ax" wait Gents "text=$token" 1 > /dev/null 2>&1 && return 0
    answer local-network UserNotificationCenter Allow
    sleep 1
  done
  echo "no reply after 240s"
  return 1
}

quit_app() {
  answer local-network UserNotificationCenter Allow
  "$ax" click Gents "AXMenuItem=Quit Gents" || return 1
  for _ in $(seq 1 30); do pgrep -x gents-desktop-tauri > /dev/null || return 0; sleep 1; done
  echo "app still running"
  return 1
}

report
killall "System Settings" 2> /dev/null
trap 'rm -f "$out/.detail"' EXIT
step download download
step open-dmg open_dmg
step drag-to-applications drag_to_applications
step launch launch
step welcome welcome
step start-node start_node
step connect-provider connect_provider
step choose-model choose_model
step save-inference save_inference
step first-message first_message
step quit-app quit_app
