#!/usr/bin/env python3
"""Carry work done on the unsplit tree over to the applied layout.

    scripts/crate-layout/port.py BASE TIP [--out FILE] [--no-fmt]

BASE is the unsplit commit the applied layout was produced from (or any
unsplit commit the work branched from) and TIP the unsplit commit holding the
work. The layout is applied to both in two throwaway worktrees with this
directory's apply.py, manifest and visibility feedback, and the difference
between the two applied trees is written as one patch (default:
port.patch). On the applied-layout branch:

    git apply --3way --index port.patch

Because both sides go through the same rewriter, moved files, `crate::` to
`gents_x::` paths, widened visibility and Cargo manifests come out the way
the layout has them, and the patch holds only what the work changed. New
top-level modules and manifest paths the work removed are reported by
apply.py on the TIP side; add or drop rules in manifest.json first if it
warns. For a commit-by-commit port, run this once per commit with
BASE=commit^ and TIP=commit.

report/mapping.json (written by apply.py) is the plain map for looking up
one path by hand: `files` is old file -> new file, `modules` is old module
path -> new crate and module path.
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))


def run(cmd, cwd, **kw):
    return subprocess.run(cmd, cwd=cwd, check=True, **kw)


def applied_tree(repo, commit, fmt, tmp, name):
    wt = os.path.join(tmp, name)
    run(["git", "worktree", "add", "-q", "--detach", wt, commit], repo)
    cmd = [sys.executable, os.path.join(HERE, "apply.py"), "--repo", wt, "--manifest", os.path.join(HERE, "manifest.json"), "--report-dir", os.path.join(tmp, name + "-report")]
    if fmt:
        cmd.append("--fmt")
    run(cmd, wt)
    run(["git", "add", "-A"], wt)
    run(["git", "-c", "user.name=port", "-c", "user.email=port@localhost", "commit", "-q", "--no-verify", "-m", "applied layout of " + commit], wt)
    return wt, subprocess.run(["git", "rev-parse", "HEAD"], cwd=wt, check=True, capture_output=True, text=True).stdout.strip()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("base")
    ap.add_argument("tip")
    ap.add_argument("--out", default="port.patch")
    ap.add_argument("--no-fmt", action="store_true", help="do not run cargo fmt on the two applied trees (use when the layout branch was applied without --fmt)")
    args = ap.parse_args()
    repo = subprocess.run(["git", "rev-parse", "--show-toplevel"], cwd=HERE, check=True, capture_output=True, text=True).stdout.strip()
    tmp = tempfile.mkdtemp(prefix="crate-layout-port-")
    trees = []
    try:
        a_wt, a = applied_tree(repo, args.base, not args.no_fmt, tmp, "base")
        trees.append(a_wt)
        b_wt, b = applied_tree(repo, args.tip, not args.no_fmt, tmp, "tip")
        trees.append(b_wt)
        patch = subprocess.run(["git", "diff", "--binary", "-M", a, b, "--", ".", ":(exclude)scripts/crate-layout/report"], cwd=repo, check=True, capture_output=True).stdout
        with open(args.out, "wb") as f:
            f.write(patch)
        print("wrote %s (%d bytes)" % (args.out, len(patch)))
    finally:
        for wt in trees:
            subprocess.run(["git", "worktree", "remove", "--force", wt], cwd=repo)
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
