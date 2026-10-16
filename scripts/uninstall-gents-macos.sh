#!/usr/bin/env bash
# Usage:
#   ./uninstall-gents-macos.sh           # interactive (asks for confirmation)
#   ./uninstall-gents-macos.sh --yes     # no prompt
#   ./uninstall-gents-macos.sh --dry-run # show what would be removed, change nothing

set -euo pipefail

GENTS_HOME_DEFAULT="${GENTS_HOME:-$HOME/.gents}"
SERVICE_LABEL="ai.gents.runtime"
LAUNCH_AGENT_PLIST="$HOME/Library/LaunchAgents/${SERVICE_LABEL}.plist"
KEYCHAIN_SERVICE="com.source-inc.gents.identity"
BUNDLE_ID="com.source-inc.gents"
APP_DIR_NAME="gents"
APP_BUNDLE="/Applications/Gents.app"
CLI_BINARY="/usr/local/bin/gents"

LIBRARY_SUBDIRS=(
  "Application Support"
  "Application Scripts"
  "Caches"
  "Containers"
  "Cookies"
  "Group Containers"
  "HTTPStorages"
  "LaunchAgents"
  "Logs"
  "Preferences"
  "Preferences/ByHost"
  "Saved Application State"
  "WebKit"
)
LIBRARY_NAME_PATTERNS=(
  "$APP_DIR_NAME"
  "$APP_DIR_NAME.*"
  "$APP_DIR_NAME-*"
  "$BUNDLE_ID"
  "$BUNDLE_ID.*"
  "$BUNDLE_ID-*"
  "*.$BUNDLE_ID"
  "*.$BUNDLE_ID.*"
  "ai.gents.*"
)

ASSUME_YES=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --yes|-y) ASSUME_YES=1 ;;
    --dry-run|-n) DRY_RUN=1 ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "Unknown argument: $arg" >&2; exit 2 ;;
  esac
done

log()  { printf '  %s\n' "$*"; }
info() { printf '\n==> %s\n' "$*"; }

run() {
  local desc="$1"; shift
  if [[ "$DRY_RUN" -eq 1 ]]; then
    log "[dry-run] $desc"
    return 0
  fi
  log "$desc"
  "$@"
}

remove_path() {
  local path="$1"
  if [[ -e "$path" || -L "$path" ]]; then
    run "removing $path" rm -rf -- "$path"
  else
    log "not present: $path"
  fi
}

need_sudo_for() {
  local path="$1"
  [[ -e "$path" ]] || return 1
  local parent
  parent="$(dirname "$path")"
  [[ ! -w "$path" || ! -w "$parent" ]]
}

sudo_remove_path() {
  local path="$1"
  if [[ ! -e "$path" && ! -L "$path" ]]; then
    log "not present: $path"
    return 0
  fi
  if need_sudo_for "$path"; then
    run "removing (sudo) $path" sudo rm -rf -- "$path"
  else
    run "removing $path" rm -rf -- "$path"
  fi
}

cat <<EOF
This will remove Gents and its local data from this Mac:

  Agent home        : $GENTS_HOME_DEFAULT
  LaunchAgent       : $LAUNCH_AGENT_PLIST  (job: $SERVICE_LABEL)
  Keychain identity : service "$KEYCHAIN_SERVICE"
  CLI binary        : $CLI_BINARY
  Desktop app       : $APP_BUNDLE
  Library entries   : everything named $APP_DIR_NAME, $APP_DIR_NAME-*, $BUNDLE_ID*, or ai.gents.*
                      under ~/Library/{Application Support,Caches,Logs,WebKit,
                      HTTPStorages,Cookies,Preferences,Saved Application State,
                      Containers,Group Containers,Application Scripts,LaunchAgents}

Per-repo worktrees under <checkout>/.gents/workspaces/ are NOT removed.
EOF

if [[ "$DRY_RUN" -eq 1 ]]; then
  echo
  echo "(dry-run mode: nothing will be changed)"
fi

if [[ "$ASSUME_YES" -ne 1 && "$DRY_RUN" -ne 1 ]]; then
  echo
  read -r -p "Proceed? [y/N] " reply
  case "$reply" in
    y|Y|yes|YES) ;;
    *) echo "Aborted."; exit 1 ;;
  esac
fi

info "Stopping and removing the LaunchAgent service ($SERVICE_LABEL)"
uid="$(id -u)"
if [[ "$DRY_RUN" -eq 1 ]]; then
  log "[dry-run] launchctl bootout gui/$uid/$SERVICE_LABEL"
else
  launchctl bootout "gui/$uid/$SERVICE_LABEL" 2>/dev/null || true
  launchctl remove "$SERVICE_LABEL" 2>/dev/null || true
fi
remove_path "$LAUNCH_AGENT_PLIST"

info "Quitting and removing the desktop app"
if [[ "$DRY_RUN" -eq 1 ]]; then
  log "[dry-run] osascript quit \"Gents\""
else
  osascript -e 'tell application "Gents" to quit' 2>/dev/null || true
  pkill -f "$APP_BUNDLE/Contents/MacOS/" 2>/dev/null || true
fi
sudo_remove_path "$APP_BUNDLE"

info "Removing the CLI binary"
sudo_remove_path "$CLI_BINARY"

info "Removing the agent home"
if [[ -z "$GENTS_HOME_DEFAULT" || "$GENTS_HOME_DEFAULT" == "/" || "$GENTS_HOME_DEFAULT" == "$HOME" ]]; then
  echo "Refusing to remove suspicious GENTS_HOME: '$GENTS_HOME_DEFAULT'" >&2
else
  remove_path "$GENTS_HOME_DEFAULT"
fi

info "Removing the macOS keychain identity ($KEYCHAIN_SERVICE)"
if [[ "$DRY_RUN" -eq 1 ]]; then
  log "[dry-run] security delete-generic-password -s \"$KEYCHAIN_SERVICE\""
else
  while security find-generic-password -s "$KEYCHAIN_SERVICE" >/dev/null 2>&1; do
    if ! security delete-generic-password -s "$KEYCHAIN_SERVICE" >/dev/null 2>&1; then
      break
    fi
    log "deleted a keychain item for service $KEYCHAIN_SERVICE"
  done
  log "no remaining generic-password items for $KEYCHAIN_SERVICE"
  log "note: Secure Enclave keys (if that backend was used) must be removed"
  log "      from Keychain Access manually; they are not exportable via CLI."
fi

info "Removing all Gents entries under ~/Library"
if [[ "$DRY_RUN" -ne 1 ]]; then
  # Drop cached preferences so cfprefsd doesn't rewrite them.
  defaults delete "$BUNDLE_ID" >/dev/null 2>&1 || true
fi
found_any=0
shopt -s nullglob
for subdir in "${LIBRARY_SUBDIRS[@]}"; do
  base="$HOME/Library/$subdir"
  [[ -d "$base" ]] || continue
  for pattern in "${LIBRARY_NAME_PATTERNS[@]}"; do
    for entry in "$base"/$pattern; do
      [[ -e "$entry" || -L "$entry" ]] || continue
      [[ "$(dirname "$entry")" == "$base" ]] || continue
      found_any=1
      remove_path "$entry"
    done
  done
done
shopt -u nullglob
[[ "$found_any" -eq 1 ]] || log "no Gents entries found under ~/Library"

info "Per-repo worktrees (not removed)"
log "Gents may have created worktrees under <your-checkouts>/.gents/workspaces/."
log "Search for them with:"
log "  find \"\$HOME\" -type d -path '*/.gents/workspaces' 2>/dev/null"

info "Done."
if [[ "$DRY_RUN" -eq 1 ]]; then
  log "(dry-run: no changes were made)"
fi
