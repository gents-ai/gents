#!/bin/sh
# Create a git worktree with warm build caches.
#
# A bare `git worktree add` starts every cache cold: the new worktree has no
# `target/` (a full workspace test build recompiles ~40 minutes even with
# sccache, which cannot cache build scripts, proc-macro expansion, or linking)
# and no `crates/gents/proofs/.lake` (Mathlib rebuilds for hours). Both are
# cheaply transferable from the current checkout via APFS clonefile (`cp -c`),
# which shares blocks copy-on-write instead of duplicating them.
#
# Usage: scripts/worktree-bootstrap.sh <branch> [dest-dir] [base-ref]
#   branch    branch to check out; created from base-ref if it does not exist
#   dest-dir  defaults to $WORKTREE_DIR, then a sibling gents-<slug> directory
#   base-ref  defaults to $WORKTREE_BASE, then HEAD; used when creating the branch
# The env-var forms exist so `make worktree` can pass optional values without
# positional-argument collapse (an empty positional would shift later ones).
set -eu

branch="${1:?usage: worktree-bootstrap.sh <branch> [dest-dir] [base-ref]}"
repo_root="$(git rev-parse --show-toplevel)"

slug="$(printf '%s' "$branch" | sed -E 's#^(fix|feat|feature|docs|perf|chore|agent)/##; s#/#-#g')"
dest="${2:-${WORKTREE_DIR:-"$(dirname "$repo_root")/gents-$slug"}}"
base="${3:-${WORKTREE_BASE:-HEAD}}"

if [ -e "$dest" ]; then
    echo "error: destination $dest already exists" >&2
    exit 1
fi

if git show-ref --verify --quiet "refs/heads/$branch"; then
    git worktree add "$dest" "$branch"
else
    git worktree add -b "$branch" "$dest" "$base"
fi

# Copying target/ out from under a live build is only mildly risky (cargo
# rebuilds anything whose fingerprint doesn't verify), but warn so a slow or
# odd first build in the new worktree isn't a mystery.
for pid in $(pgrep -x cargo 2>/dev/null || true); do
    cwd="$(lsof -a -p "$pid" -d cwd -Fn 2>/dev/null | sed -n 's/^n//p')"
    if [ "$cwd" = "$repo_root" ]; then
        echo "warning: cargo (pid $pid) is building in $repo_root; cloned artifacts it is mid-writing will just recompile" >&2
    fi
done

clone_dir() {
    src="$1"
    dst="$2"
    label="$3"
    if [ ! -d "$src" ]; then
        echo "skip: no $label to clone ($src)"
        return 0
    fi
    echo "cloning $label ..."
    if ! cp -Rc "$src" "$dst" 2>/dev/null; then
        rm -rf "$dst"
        echo "clonefile unavailable (cross-volume?); plain copy of $label ..." >&2
        cp -R "$src" "$dst"
    fi
}

clone_dir "$repo_root/target" "$dest/target" "cargo target/"
# Stale incremental state is worthless across worktrees (the workspace builds
# with incremental=false) and just wastes space in the clone.
rm -rf "$dest/target/debug/incremental" "$dest/target/release/incremental"

# Lean caches are only valid against an identical dependency manifest.
proofs="crates/gents/proofs"
if cmp -s "$repo_root/$proofs/lake-manifest.json" "$dest/$proofs/lake-manifest.json" 2>/dev/null; then
    clone_dir "$repo_root/$proofs/.lake" "$dest/$proofs/.lake" "Lean proofs .lake/"
else
    echo "skip: lake-manifest.json differs (or missing); not cloning .lake — lake build will repopulate"
fi

echo
echo "worktree ready: $dest (branch $branch)"
df -h "$dest" | tail -1 | awk '{print "disk: " $4 " free (" $5 " used)"}'
