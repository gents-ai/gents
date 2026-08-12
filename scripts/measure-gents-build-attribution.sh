#!/usr/bin/env bash
set -euo pipefail

# Produce cold-build, Cargo timing, resolved-feature, and linked-size evidence
# for the release CLI. Raw reports belong in workflow artifacts, not git.
#
# Environment:
#   ATTRIBUTION_OUTPUT_DIR=path   report directory (default: build-attribution)
#   CARGO_TARGET_DIR=path         fresh Cargo target directory (required in CI)
#   ATTRIBUTION_CACHE_STATE=text  human-readable cache state for the report
#   TARGET_TRIPLE=triple          target to measure (default: rustc host)
#
# The target directory must be empty. This prevents an accidentally warm build
# from being labelled cold. Set ATTRIBUTION_ALLOW_EXISTING_TARGET=1 only for
# parser/tool development; never set it in the measurement workflow.

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

extract_timing_units() {
  local timing_html=$1
  local output_json=$2

  awk '
    /^const UNIT_DATA = \[$/ { capture = 1; print "["; next }
    capture && /^\];$/ { print "]"; found_end = 1; exit }
    capture { print }
    END { if (!capture || !found_end) exit 1 }
  ' "$timing_html" > "$output_json"
  jq -e 'type == "array" and all(.[]; (.name | type) == "string" and (.duration | type) == "number")' \
    "$output_json" >/dev/null
}

rank_timing_packages() {
  local units_json=$1
  local output_json=$2

  jq '
    group_by([.name, .version])
    | map({
        name: .[0].name,
        version: .[0].version,
        duration_seconds: (map(.duration) | add),
        longest_unit_seconds: (map(.duration) | max),
        units: length,
        features: (map(.features[]) | unique)
      })
    | sort_by(-.duration_seconds, .name, .version)
  ' "$units_json" > "$output_json"
}

self_test() {
  local fixture_root
  fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/gents-build-attribution-test.XXXXXX")
  trap 'rm -rf -- "$fixture_root"' RETURN

  local fixture_html="$fixture_root/timing.html"
  local units_json="$fixture_root/units.json"
  local packages_json="$fixture_root/packages.json"
  printf '%s\n' \
    '<html>' \
    'const UNIT_DATA = [' \
    '  {"name":"alpha","version":"1.0.0","duration":1.25,"features":["std"]},' \
    '  {"name":"alpha","version":"1.0.0","duration":0.75,"features":["derive"]},' \
    '  {"name":"beta","version":"2.0.0","duration":3.0,"features":[]}' \
    '];' \
    '</html>' > "$fixture_html"

  extract_timing_units "$fixture_html" "$units_json"
  rank_timing_packages "$units_json" "$packages_json"
  jq -e '
    length == 2
    and .[0].name == "beta"
    and .[0].duration_seconds == 3
    and .[1].name == "alpha"
    and .[1].duration_seconds == 2
    and .[1].units == 2
    and .[1].features == ["derive", "std"]
  ' "$packages_json" >/dev/null
  echo "build attribution parser self-test passed"
}

if [[ "${1:-}" == "self-test" ]]; then
  self_test
  exit 0
fi

for tool in cargo jq rustc; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing required tool: $tool" >&2
    exit 1
  fi
done
if ! cargo bloat --version >/dev/null 2>&1; then
  echo "cargo-bloat 0.12.1 is required; install it with: cargo install --locked cargo-bloat --version 0.12.1" >&2
  exit 1
fi
cargo_bloat_version=$(cargo bloat --version)
if [[ "$cargo_bloat_version" != "0.12.1" ]]; then
  echo "cargo-bloat 0.12.1 is required, got $cargo_bloat_version" >&2
  exit 1
fi

output_dir=${ATTRIBUTION_OUTPUT_DIR:-"$repo_root/target/build-attribution"}
target_dir=${CARGO_TARGET_DIR:-"$output_dir/cargo-target"}
case "$output_dir" in
  /*) ;;
  *) output_dir="$repo_root/$output_dir" ;;
esac
case "$target_dir" in
  /*) ;;
  *) target_dir="$repo_root/$target_dir" ;;
esac

if [[ -d "$target_dir" && -n "$(find "$target_dir" -mindepth 1 -maxdepth 1 -print -quit)" \
  && "${ATTRIBUTION_ALLOW_EXISTING_TARGET:-0}" != "1" ]]; then
  echo "CARGO_TARGET_DIR must be empty for a cold attribution build: $target_dir" >&2
  exit 2
fi
mkdir -p "$output_dir" "$target_dir"

target_triple=${TARGET_TRIPLE:-$(rustc -Vv | awk '/^host:/ { print $2 }')}
cache_state=${ATTRIBUTION_CACHE_STATE:-cold-target-unspecified-compiler-cache}
git_sha=${GITHUB_SHA:-$(git rev-parse HEAD)}
git_ref=${GITHUB_REF:-$(git symbolic-ref -q HEAD || true)}
git_tag=
git_dirty=false
if [[ -z "${GITHUB_SHA:-}" && -n "$(git status --porcelain --untracked-files=normal)" ]]; then
  git_dirty=true
fi
if [[ "$git_ref" == refs/tags/* ]]; then
  git_tag=${git_ref#refs/tags/}
fi
export GENTS_BUILD_GIT_SHA=${GENTS_BUILD_GIT_SHA:-$git_sha}
export GENTS_BUILD_GIT_REF=${GENTS_BUILD_GIT_REF:-$git_ref}
export GENTS_BUILD_GIT_TAG=${GENTS_BUILD_GIT_TAG:-$git_tag}
export GENTS_BUILD_GIT_DIRTY=${GENTS_BUILD_GIT_DIRTY:-$git_dirty}
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$target_dir"

binary_path="$target_dir/$target_triple/release/gents"
measured_binary="$output_dir/gents-release-stripped"
timing_html="$target_dir/cargo-timings/cargo-timing.html"
build_log="$output_dir/cargo-build.log"
resource_usage="$output_dir/resource-usage.txt"
commands_file="$output_dir/commands.txt"
tool_versions="$output_dir/tool-versions.txt"

printf '%s\n' \
  "cargo build --locked -p gents-cli --release --bin gents --target $target_triple --timings" \
  "cargo metadata --locked --format-version 1 --filter-platform $target_triple" \
  "cargo tree --locked -p gents-cli --edges normal --target $target_triple -d" \
  "cargo tree --locked -p gents-cli --edges features --target $target_triple" \
  "cargo bloat --locked --release -p gents-cli --bin gents --target $target_triple --target-dir $target_dir --crates -n 0 --message-format json" \
  > "$commands_file"
printf '%s\n' \
  "$(rustc --version --verbose)" \
  "$(cargo --version --verbose)" \
  "cargo-bloat $cargo_bloat_version" \
  "jq $(jq --version)" \
  "$(uname -a)" \
  > "$tool_versions"

build_started_at=$(date +%s)
if [[ "$(uname -s)" == "Darwin" ]]; then
  {
    /usr/bin/time -l cargo build --locked -p gents-cli --release --bin gents \
      --target "$target_triple" --timings
  } 2>&1 | tee "$build_log"
  awk '/maximum resident set size/ { print }' "$build_log" > "$resource_usage"
  peak_rss_bytes=$(awk '/maximum resident set size/ { print $1; exit }' "$build_log")
  peak_rss_unit=bytes
else
  {
    /usr/bin/time -p cargo build --locked -p gents-cli --release --bin gents \
      --target "$target_triple" --timings
  } 2>&1 | tee "$build_log"
  awk '/^(real|user|sys) / { print }' "$build_log" > "$resource_usage"
  peak_rss_bytes=
  peak_rss_unit=unavailable
fi
build_elapsed_seconds=$(($(date +%s) - build_started_at))

if [[ ! -x "$binary_path" ]]; then
  echo "missing built CLI: $binary_path" >&2
  exit 1
fi
if [[ ! -f "$timing_html" ]]; then
  echo "missing Cargo timing report: $timing_html" >&2
  exit 1
fi

# cargo-bloat intentionally rebuilds with profile stripping disabled so it can
# inspect symbols. Snapshot and measure the ordinary stripped release output
# first; otherwise the inspection build would overwrite the binary whose size
# this report claims to record.
cp "$binary_path" "$measured_binary"
BUILD_ELAPSED_SECONDS=$build_elapsed_seconds \
  BINARY_PATH=$measured_binary \
  OUTPUT_JSON="$output_dir/release-metrics.json" \
  SKIP_BUILD=1 \
  TARGET=$target_triple \
  scripts/measure-gents-binary.sh > "$output_dir/release-metrics.txt"

cp "$timing_html" "$output_dir/cargo-timing.html"
extract_timing_units "$timing_html" "$output_dir/cargo-timing-units.json"
rank_timing_packages "$output_dir/cargo-timing-units.json" "$output_dir/cargo-timing-packages.json"

CARGO_TERM_COLOR=never cargo metadata --locked --format-version 1 \
  --filter-platform "$target_triple" > "$output_dir/cargo-metadata.json"
CARGO_TERM_COLOR=never cargo tree --locked -p gents-cli --edges normal \
  --target "$target_triple" -d > "$output_dir/cargo-duplicates.txt"
CARGO_TERM_COLOR=never cargo tree --locked -p gents-cli --edges features \
  --target "$target_triple" > "$output_dir/cargo-feature-tree.txt"

cargo bloat --locked --release -p gents-cli --bin gents \
  --target "$target_triple" --target-dir "$target_dir" --crates -n 0 \
  --message-format json > "$output_dir/cargo-bloat-crates.json" \
  2> "$output_dir/cargo-bloat.log"
jq -e '
  (."file-size" | type) == "number"
  and (."text-section-size" | type) == "number"
  and (.crates | type) == "array"
  and all(.crates[]; (.name | type) == "string" and (.size | type) == "number")
' "$output_dir/cargo-bloat-crates.json" >/dev/null

peak_rss_json=null
if [[ -n "$peak_rss_bytes" && "$peak_rss_bytes" =~ ^[0-9]+$ ]]; then
  peak_rss_json=$peak_rss_bytes
fi
rustc_wrapper=${RUSTC_WRAPPER:-none}
cargo_lock_sha256=$(jq -r '.cargo_lock_sha256' "$output_dir/release-metrics.json")

jq -n \
  --arg git_sha "$git_sha" \
  --arg cargo_lock_sha256 "$cargo_lock_sha256" \
  --arg target_triple "$target_triple" \
  --arg cache_state "$cache_state" \
  --arg rustc_wrapper "$rustc_wrapper" \
  --arg peak_rss_unit "$peak_rss_unit" \
  --argjson elapsed_seconds "$build_elapsed_seconds" \
  --argjson peak_rss "$peak_rss_json" \
  --slurpfile release "$output_dir/release-metrics.json" \
  --slurpfile timings "$output_dir/cargo-timing-packages.json" \
  --slurpfile bloat "$output_dir/cargo-bloat-crates.json" \
  '{
    schema_version: 1,
    measurement_kind: "build_attribution",
    git_sha: $git_sha,
    cargo_lock_sha256: $cargo_lock_sha256,
    target_triple: $target_triple,
    profile: "release",
    cache: {
      state: $cache_state,
      target_dir_reused: false,
      rustc_wrapper: $rustc_wrapper,
      incremental: false
    },
    build: {
      elapsed_seconds: $elapsed_seconds,
      peak_rss: $peak_rss,
      peak_rss_unit: $peak_rss_unit
    },
    release_metrics: $release[0],
    longest_compiled_packages: $timings[0][0:50],
    largest_linked_crates: ($bloat[0].crates | sort_by(-.size) | .[0:50]),
    linked_file_bytes: $bloat[0]["file-size"],
    linked_text_section_bytes: $bloat[0]["text-section-size"]
  }' > "$output_dir/summary.json"

jq -e '
  .schema_version == 1
  and .measurement_kind == "build_attribution"
  and .build.elapsed_seconds > 0
  and .release_metrics.dependencies.normal_packages > 0
  and (.longest_compiled_packages | length) > 0
  and (.largest_linked_crates | length) > 0
' "$output_dir/summary.json" >/dev/null

echo "build attribution: $output_dir/summary.json"
echo "build_elapsed_seconds: $build_elapsed_seconds"
echo "peak_rss: ${peak_rss_bytes:-unavailable} $peak_rss_unit"
echo "binary_bytes: $(jq -r '.release_metrics.binary.bytes' "$output_dir/summary.json")"
echo "normal_packages: $(jq -r '.release_metrics.dependencies.normal_packages' "$output_dir/summary.json")"
echo "largest linked crates:"
jq -r '.largest_linked_crates[0:15][] | "  \(.size)\t\(.name)"' "$output_dir/summary.json"
echo "longest compiled packages:"
jq -r '.longest_compiled_packages[0:15][] | "  \(.duration_seconds)\t\(.name) \(.version)"' \
  "$output_dir/summary.json"

if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "## gents release build attribution"
    echo
    echo "- Commit: \`$git_sha\`"
    echo "- Target: \`$target_triple\`"
    echo "- Cache: $cache_state"
    echo "- Build wall time: ${build_elapsed_seconds}s"
    echo "- Peak RSS: ${peak_rss_bytes:-unavailable} $peak_rss_unit"
    echo "- Binary: $(jq -r '.release_metrics.binary.bytes' "$output_dir/summary.json") bytes"
    echo "- Normal packages: $(jq -r '.release_metrics.dependencies.normal_packages' "$output_dir/summary.json")"
    echo
    echo "### Largest linked crates"
    echo
    echo '| Bytes | Crate |'
    echo '|---:|---|'
    jq -r '.largest_linked_crates[0:15][] | "| \(.size) | `\(.name)` |"' "$output_dir/summary.json"
    echo
    echo "### Longest compiled packages"
    echo
    echo '| Aggregate seconds | Package | Units |'
    echo '|---:|---|---:|'
    jq -r '.longest_compiled_packages[0:15][] | "| \(.duration_seconds) | `\(.name) \(.version)` | \(.units) |"' \
      "$output_dir/summary.json"
  } >> "$GITHUB_STEP_SUMMARY"
fi
