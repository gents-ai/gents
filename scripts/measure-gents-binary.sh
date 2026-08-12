#!/usr/bin/env bash
set -euo pipefail

# Measure the release dependency graph and, when requested, the built CLI.
#
# Environment:
#   MEASURE_MODE=graph|binary  graph skips the release build/binary inspection
#   SKIP_BUILD=1              inspect an existing release binary
#   BUILD_ELAPSED_SECONDS=N   preserve timing from a build performed elsewhere
#   BINARY_PATH=path          override $CARGO_TARGET_DIR/release/gents
#   ARCHIVE_PATH=path         include the packaged release archive in the report
#   OUTPUT_JSON=path          write the stable machine-readable report
#
# When GITHUB_OUTPUT or GITHUB_STEP_SUMMARY are set, the script also exposes
# scalar step outputs and appends a compact Actions summary.

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

mode=${MEASURE_MODE:-binary}
case "$mode" in
  graph | binary) ;;
  *)
    echo "MEASURE_MODE must be 'graph' or 'binary', got '$mode'" >&2
    exit 2
    ;;
esac

target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
if [[ "$target_dir" != /* ]]; then
  target_dir="$repo_root/$target_dir"
fi

profile_value() {
  local key=$1
  awk -v wanted="$key" '
    /^\[profile\.release\]$/ { in_release = 1; next }
    /^\[/ { in_release = 0 }
    in_release && $1 == wanted {
      value = $3
      gsub(/"/, "", value)
      print value
      exit
    }
  ' Cargo.toml
}

json_escape() {
  local value=$1
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  value=${value//$'\n'/\\n}
  value=${value//$'\r'/\\r}
  value=${value//$'\t'/\\t}
  printf '%s' "$value"
}

file_bytes() {
  wc -c < "$1" | tr -d '[:space:]'
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

git_dirty=false
if [[ -n "${GITHUB_SHA:-}" ]]; then
  # Actions checks out the immutable workflow SHA. Prefer that authoritative
  # value because container jobs can intentionally have a different uid than
  # the host-owned checkout, which makes Git reject repository inspection as
  # dubious ownership even though the checked-out sources are valid.
  git_sha=$GITHUB_SHA
else
  git_sha=$(git rev-parse HEAD)
  if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
    git_dirty=true
  fi
fi
cargo_lock_sha256=$(sha256_file Cargo.lock)
rust_version=$(rustc --version)
target_triple=${TARGET_TRIPLE:-${TARGET:-$(rustc -Vv | awk '/^host:/ { print $2 }')}}
release_lto=${CARGO_PROFILE_RELEASE_LTO:-$(profile_value lto)}
release_codegen_units=${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-$(profile_value codegen-units)}
release_strip=${CARGO_PROFILE_RELEASE_STRIP:-$(profile_value strip)}

case "$release_lto" in
  true) release_lto=fat ;;
  false) release_lto=local-thin ;;
esac

# Cargo repeats already-rendered nodes with a trailing `(*)`. Strip that
# presentation marker before counting package/version identities; otherwise a
# package's position in the tree changes the metric.
package_lines=$(
  cargo tree --locked -p gents-cli --edges normal --prefix none --format '{p}' \
    | sed 's/ (\*)$//' \
    | LC_ALL=C sort -u
)
resolved_packages=$(printf '%s\n' "$package_lines" | awk 'NF { count++ } END { print count + 0 }')
duplicate_package_names=$(
  printf '%s\n' "$package_lines" \
    | awk 'NF { print $1 }' \
    | LC_ALL=C sort \
    | uniq -d \
    | awk 'NF { count++ } END { print count + 0 }'
)
codex_packages=$(
  printf '%s\n' "$package_lines" \
    | sed -n 's/^\(codex-[^ ]*\).*/\1/p' \
    | LC_ALL=C sort -u \
    | awk 'NF { count++ } END { print count + 0 }'
)

build_elapsed_seconds=${BUILD_ELAPSED_SECONDS:-}
default_binary_path="$target_dir/release/gents"
if [[ -n "${TARGET:-}" ]]; then
  default_binary_path="$target_dir/$TARGET/release/gents"
fi
binary_path=${BINARY_PATH:-"$default_binary_path"}
binary_bytes=
binary_gzip_bytes=
binary_sha256=

if [[ "$mode" == "binary" ]]; then
  if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    build_started_at=$(date +%s)
    make release-cli
    build_elapsed_seconds=$(($(date +%s) - build_started_at))
  fi

  if [[ ! -f "$binary_path" ]]; then
    echo "missing $binary_path; run without SKIP_BUILD=1 first" >&2
    exit 1
  fi

  binary_bytes=$(file_bytes "$binary_path")
  binary_gzip_bytes=$(gzip -9 -c "$binary_path" | wc -c | tr -d '[:space:]')
  binary_sha256=$(sha256_file "$binary_path")
fi

archive_path=${ARCHIVE_PATH:-}
archive_bytes=
archive_sha256=
if [[ -n "$archive_path" ]]; then
  if [[ ! -f "$archive_path" ]]; then
    echo "missing release archive $archive_path" >&2
    exit 1
  fi
  archive_bytes=$(file_bytes "$archive_path")
  archive_sha256=$(sha256_file "$archive_path")
fi

build_elapsed_json=null
if [[ -n "$build_elapsed_seconds" ]]; then
  if [[ ! "$build_elapsed_seconds" =~ ^[0-9]+$ ]]; then
    echo "BUILD_ELAPSED_SECONDS must be a non-negative integer" >&2
    exit 2
  fi
  build_elapsed_json=$build_elapsed_seconds
fi

binary_json=null
if [[ "$mode" == "binary" ]]; then
  binary_json=$(printf \
    '{"name":"%s","bytes":%s,"gzip_bytes":%s,"sha256":"%s"}' \
    "$(json_escape "$(basename "$binary_path")")" \
    "$binary_bytes" \
    "$binary_gzip_bytes" \
    "$binary_sha256")
fi

archive_json=null
if [[ -n "$archive_path" ]]; then
  archive_json=$(printf \
    '{"name":"%s","bytes":%s,"sha256":"%s"}' \
    "$(json_escape "$(basename "$archive_path")")" \
    "$archive_bytes" \
    "$archive_sha256")
fi

report=$(printf '%s\n' \
  '{' \
  '  "schema_version": 1,' \
  "  \"measurement_kind\": \"$(json_escape "$mode")\"," \
  "  \"git_sha\": \"$(json_escape "$git_sha")\"," \
  "  \"git_dirty\": $git_dirty," \
  "  \"cargo_lock_sha256\": \"$cargo_lock_sha256\"," \
  "  \"rust_version\": \"$(json_escape "$rust_version")\"," \
  "  \"target_triple\": \"$(json_escape "$target_triple")\"," \
  '  "profile": {' \
  '    "name": "release",' \
  "    \"lto\": \"$(json_escape "$release_lto")\"," \
  "    \"codegen_units\": \"$(json_escape "$release_codegen_units")\"," \
  "    \"strip\": \"$(json_escape "$release_strip")\"" \
  '  },' \
  '  "build": {' \
  "    \"elapsed_seconds\": $build_elapsed_json" \
  '  },' \
  "  \"binary\": $binary_json," \
  "  \"archive\": $archive_json," \
  '  "dependencies": {' \
  "    \"normal_packages\": $resolved_packages," \
  "    \"duplicate_package_names\": $duplicate_package_names," \
  "    \"codex_packages\": $codex_packages" \
  '  }' \
  '}')

if [[ -n "${OUTPUT_JSON:-}" ]]; then
  mkdir -p "$(dirname "$OUTPUT_JSON")"
  printf '%s\n' "$report" > "$OUTPUT_JSON"
fi

echo "measurement_kind: $mode"
echo "git_sha: $git_sha"
echo "git_dirty: $git_dirty"
echo "cargo_lock_sha256: $cargo_lock_sha256"
echo "rust_version: $rust_version"
echo "target_triple: $target_triple"
echo "release_lto: $release_lto"
echo "release_codegen_units: $release_codegen_units"
echo "release_strip: $release_strip"
echo "build_elapsed_seconds: ${build_elapsed_seconds:-skipped}"
if [[ "$mode" == "binary" ]]; then
  echo "binary: $binary_path"
  echo "binary_bytes: $binary_bytes"
  echo "binary_gzip_bytes: $binary_gzip_bytes"
  echo "binary_sha256: $binary_sha256"
fi
if [[ -n "$archive_path" ]]; then
  echo "archive: $archive_path"
  echo "archive_bytes: $archive_bytes"
  echo "archive_sha256: $archive_sha256"
fi
echo "resolved_packages: $resolved_packages"
echo "duplicate_package_names: $duplicate_package_names"
echo "codex_packages: $codex_packages"

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  {
    echo "measurement_kind=$mode"
    echo "normal_packages=$resolved_packages"
    echo "duplicate_package_names=$duplicate_package_names"
    echo "codex_packages=$codex_packages"
    [[ -z "$binary_bytes" ]] || echo "binary_bytes=$binary_bytes"
    [[ -z "$binary_gzip_bytes" ]] || echo "binary_gzip_bytes=$binary_gzip_bytes"
    [[ -z "$archive_bytes" ]] || echo "archive_bytes=$archive_bytes"
    [[ -z "${OUTPUT_JSON:-}" ]] || echo "report_path=$OUTPUT_JSON"
  } >> "$GITHUB_OUTPUT"
fi

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### Gents build measurement"
    echo
    echo "| Signal | Value |"
    echo "|---|---:|"
    echo "| Target | \`$target_triple\` |"
    echo "| Release profile | LTO \`$release_lto\`, $release_codegen_units CGUs, strip \`$release_strip\` |"
    echo "| Normal dependency packages | $resolved_packages |"
    echo "| Duplicate package names | $duplicate_package_names |"
    echo "| Upstream Codex packages | $codex_packages |"
    [[ -z "$build_elapsed_seconds" ]] || echo "| Release build | ${build_elapsed_seconds}s |"
    [[ -z "$binary_bytes" ]] || echo "| Binary | $binary_bytes bytes |"
    [[ -z "$binary_gzip_bytes" ]] || echo "| Binary (gzip -9) | $binary_gzip_bytes bytes |"
    [[ -z "$archive_bytes" ]] || echo "| Release archive | $archive_bytes bytes |"
    echo
    echo "Commit: \`$git_sha\` (dirty: \`$git_dirty\`)"
  } >> "$GITHUB_STEP_SUMMARY"
fi

if [[ -z "${OUTPUT_JSON:-}" ]]; then
  printf '%s\n' "$report"
fi
