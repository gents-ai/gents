#!/usr/bin/env python3
"""Run only between Cargo invocations in the current runner's isolated target tree."""

import os
from pathlib import Path
import shutil
import subprocess
import sys


def ensure_build_space(target, minimum_gib):
    if minimum_gib <= 0:
        raise ValueError("CARGO_BUILD_MIN_FREE_GIB must be positive")
    if not target.is_absolute() or target.is_symlink() or not target.is_dir():
        raise ValueError("Cargo target must be an existing absolute directory, not a symlink")
    if target.resolve() in (Path("/"), Path.home().resolve(), Path.cwd().resolve()):
        raise ValueError("Refusing to clean a source, home, or filesystem root")

    required = minimum_gib * 1024**3

    def has_space():
        free = shutil.disk_usage(target).free
        print(f"Cargo build volume: {free / 1024**3:.1f} GiB free; "
              f"{minimum_gib} GiB required", flush=True)
        return free >= required

    if has_space():
        return
    for scope in (["--workspace"], []):
        print(f"Reclaiming {'workspace artifacts' if scope else 'artifacts'} "
              f"from {target}", flush=True)
        subprocess.run(["cargo", "clean", *scope, "--target-dir", str(target)], check=True)
        target.mkdir(exist_ok=True)
        if has_space():
            return
    raise RuntimeError("Insufficient build space after cleaning this runner's Cargo cache; "
                       "free additional space on the host before retrying")


if __name__ == "__main__":
    try:
        if len(sys.argv) != 2:
            raise ValueError(f"usage: {sys.argv[0]} <runner-cargo-target>")
        ensure_build_space(Path(sys.argv[1]), int(os.environ.get("CARGO_BUILD_MIN_FREE_GIB", "64")))
    except (ValueError, RuntimeError, OSError, subprocess.CalledProcessError) as error:
        print(f"::error::{error}", file=sys.stderr)
        sys.exit(1)
