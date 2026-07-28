#!/usr/bin/env bash
# Release gate: production capability set must not grant native-e2e.
# Spec: production enumerates "capabilities": ["default"] only; E2E uses overlay.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONF="$ROOT/apps/gents-desktop/src-tauri/tauri.conf.json"
E2E_CONF="$ROOT/apps/gents-desktop/src-tauri/tauri.e2e.conf.json"
CAP_DEFAULT="$ROOT/apps/gents-desktop/src-tauri/capabilities/default.json"
CAP_E2E="$ROOT/apps/gents-desktop/src-tauri/capabilities/native-e2e.json"
MANIFEST="$ROOT/apps/gents-desktop/src-tauri/gen/schemas/capabilities.json"

if ! grep -q '"capabilities": \["default"\]' "$CONF"; then
  echo "error: production tauri.conf.json must enumerate capabilities: [\"default\"] only"
  exit 1
fi
if grep -q 'native-e2e' "$CONF"; then
  echo "error: production tauri.conf.json must not reference native-e2e"
  exit 1
fi
if ! grep -q 'native-e2e' "$E2E_CONF"; then
  echo "error: tauri.e2e.conf.json must grant native-e2e overlay"
  exit 1
fi
if grep -q 'native-e2e' "$CAP_DEFAULT"; then
  echo "error: capabilities/default.json must not grant native-e2e"
  exit 1
fi
if ! grep -q 'gents-desktop-bridge:native-e2e' "$CAP_E2E"; then
  echo "error: capabilities/native-e2e.json must include gents-desktop-bridge:native-e2e"
  exit 1
fi

if [[ -f "$MANIFEST" ]]; then
  python3 - "$MANIFEST" <<'PY'
import json, sys
from pathlib import Path
data = json.loads(Path(sys.argv[1]).read_text())
# Flatten for search: production "default" capability must not include native-e2e.
blob = json.dumps(data)
# Prefer structured walk when shape is known.
def walk(obj, path=""):
    if isinstance(obj, dict):
        ident = obj.get("identifier")
        if ident == "default":
            text = json.dumps(obj)
            if "native-e2e" in text:
                print("error: compiled default capability includes native-e2e")
                sys.exit(1)
        for k, v in obj.items():
            walk(v, f"{path}.{k}")
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            walk(v, f"{path}[{i}]")
walk(data)
print("ok: ACL manifest checked")
PY
fi

echo "ok: native-e2e is production-excluded and E2E-overlay-only"
