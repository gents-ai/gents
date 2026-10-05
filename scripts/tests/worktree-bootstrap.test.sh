#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
script="$repo_root/scripts/worktree-bootstrap.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/gents-worktree-test.XXXXXX")"
scratch="$(cd "$scratch" && pwd -P)"
trap 'rm -rf "$scratch"' EXIT
source_root="$scratch/source"
destination="$scratch/destination"
mkdir -p "$source_root/crates/"{gents-lean-contract,consumer}/src "$scratch/warm-dependency/src"
cat > "$source_root/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/gents-lean-contract", "crates/consumer"]
resolver = "2"
EOF
for package in gents-lean-contract consumer warm-dependency; do
    package_root="$source_root/crates/$package"
    [[ "$package" != warm-dependency ]] || package_root="$scratch/warm-dependency"
    cat > "$package_root/Cargo.toml" <<EOF
[package]
name = "$package"
version = "0.1.0"
edition = "2021"
EOF
done
cat >> "$source_root/crates/consumer/Cargo.toml" <<'EOF'
[dependencies]
gents-lean-contract = { path = "../gents-lean-contract" }
warm-dependency = { path = "../../../warm-dependency" }
EOF
cat > "$source_root/crates/gents-lean-contract/src/lib.rs" <<'EOF'
pub fn source_root() -> &'static str { env!("CARGO_MANIFEST_DIR") }
EOF
cat > "$scratch/warm-dependency/src/lib.rs" <<'EOF'
pub fn value() -> u8 { 7 }
EOF
cat > "$source_root/crates/consumer/src/main.rs" <<'EOF'
use std::io::Write;
fn main() {
    assert_eq!(warm_dependency::value(), 7);
    writeln!(std::io::stdout(), "{}", gents_lean_contract::source_root()).unwrap();
    writeln!(std::io::stdout(), "{}", env!("CARGO_MANIFEST_DIR")).unwrap();
}
EOF
export RUSTC_WRAPPER= CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2
for profile in debug release; do
    cargo_profile=dev
    [[ "$profile" != release ]] || cargo_profile=release
    nice -n 10 cargo build --manifest-path "$source_root/Cargo.toml" \
        --target-dir "$source_root/target" -p consumer --profile "$cargo_profile"
done
host="$(rustc -vV | sed -n 's/^host: //p')"
nice -n 10 cargo build --manifest-path "$source_root/Cargo.toml" \
    --target-dir "$source_root/target" --target "$host" -p consumer
[[ -n "$(find "$source_root/target/$host" -name 'libgents_lean_contract*.rlib' -print)" ]]
git -C "$source_root" init -q
git -C "$source_root" config user.email bootstrap-test@example.invalid
git -C "$source_root" config user.name bootstrap-test
printf 'target/\n' > "$source_root/.gitignore"
git -C "$source_root" add .
git -C "$source_root" commit -qm fixture
fixture_base="$(git -C "$source_root" rev-parse HEAD)"
(cd "$source_root" && "$script" relocated "$destination")

[[ -z "$(find "$destination/target" -name 'libgents_lean_contract*' -print)" ]]
for profile in debug release; do
    dependency="$(find "$source_root/target/$profile/deps" -name 'libwarm_dependency-*.rlib' -print)"
    [[ -n "$dependency" ]]
    cmp "$dependency" "$destination/target/$profile/deps/$(basename "$dependency")"
    cargo_profile=dev
    [[ "$profile" != release ]] || cargo_profile=release
    output="$(nice -n 10 cargo run --quiet --manifest-path "$destination/Cargo.toml" \
        --target-dir "$destination/target" -p consumer --profile "$cargo_profile")"
    expected="$(printf '%s\n%s' "$destination/crates/gents-lean-contract" "$destination/crates/consumer")"
    [[ "$output" == "$expected" ]]
    cmp "$dependency" "$destination/target/$profile/deps/$(basename "$dependency")"
    [[ -n "$(find "$source_root/target/$profile" -name 'libgents_lean_contract*.rlib' -print)" ]]
done

# Old bases may predate the helper, but a real clean failure must stop bootstrap.
mkdir "$scratch/bin"
printf '#!/bin/sh\nexit 23\n' > "$scratch/bin/cargo"
chmod +x "$scratch/bin/cargo"
rm -rf "$source_root/crates/gents-lean-contract" "$source_root/crates/consumer"
printf '[workspace]\nmembers = []\nresolver = "2"\n' > "$source_root/Cargo.toml"
git -C "$source_root" add -A
git -C "$source_root" commit -qm without-helper
(cd "$source_root" && PATH="$scratch/bin:$PATH" "$script" old-base "$scratch/old-base")
if (cd "$source_root" && PATH="$scratch/bin:$PATH" "$script" failed-clean "$scratch/failed-clean" "$fixture_base"); then
    echo "bootstrap ignored a helper clean failure" >&2
    exit 1
fi
