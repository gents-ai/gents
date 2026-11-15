#!/usr/bin/env python3
"""Run only between Cargo invocations in the current runner's isolated target tree."""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


CACHE_SIGNATURE = "Signature: 8a477f597d28d172789f06886806bc55\n"
# A kept tree is reused only serially by one runner process, and only when the
# host keeps this much free beyond the tree's room to grow to its cap: the
# Studios also run other repositories' runners and a live Gents node.
DEFAULT_TARGET_MAX_GIB = 60
DEFAULT_TARGET_FLOOR_GIB = 150
USE_JOB_TREE = 10


def initialize_cache_tag(target):
    """Cargo clean requires a cache tag even after cleanup recreates an empty target."""
    root = Path(os.environ.get("CARGO_TARGET_ROOT", ""))
    runner = os.environ.get("RUNNER_NAME", "")
    key = re.sub(r"[^A-Za-z0-9._-]", "_", runner)
    if (not root.is_absolute() or not runner or key in (".", "..")
            or target != root / key or target.resolve() != target
            or (target / "Cargo.toml").exists() or (target / ".git").exists()):
        raise ValueError("Cargo target must be the configured runner's isolated cache")
    tag = target / "CACHEDIR.TAG"
    try:
        with tag.open("x") as output:
            output.write(CACHE_SIGNATURE)
    except FileExistsError:
        if tag.is_symlink() or not tag.is_file():
            raise ValueError("Cargo cache tag must be a regular file")
        with tag.open() as source:
            if source.readline() != CACHE_SIGNATURE:
                raise ValueError("Cargo cache tag has an invalid signature")


def runner_gib(name, setting, runner, default):
    """A per-runner GiB setting: empty, a bare integer for every runner, or a
    JSON object from runner name to integer with an optional "default" entry."""
    setting = setting.strip()
    if not setting:
        value = default
    else:
        try:
            parsed = json.loads(setting)
        except json.JSONDecodeError as error:
            raise ValueError(f"{name} must be an integer or a JSON object of runner "
                             f"names to integers: {error}") from None
        if isinstance(parsed, dict):
            value = parsed.get(runner, parsed.get("default", default))
        else:
            value = parsed
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ValueError(f"{name} for runner {runner!r} must be a positive integer of GiB")
    return value


def target_limits(environ):
    runner = environ.get("RUNNER_NAME", "")
    if not runner:
        raise ValueError("RUNNER_NAME must name the runner whose kept tree is limited")
    return (runner_gib("GENTS_CI_TARGET_MAX_GIB", environ.get("GENTS_CI_TARGET_MAX_GIB", ""),
                       runner, DEFAULT_TARGET_MAX_GIB),
            runner_gib("GENTS_CI_TARGET_FLOOR_GIB",
                       environ.get("GENTS_CI_TARGET_FLOOR_GIB", ""),
                       runner, DEFAULT_TARGET_FLOOR_GIB))


def tree_size(target):
    """Allocated bytes under the target, counting each hard-linked file once."""
    seen = set()
    total = 0
    for directory, _, files in os.walk(target):
        for name in files:
            status = os.lstat(os.path.join(directory, name))
            if status.st_nlink > 1:
                if (status.st_dev, status.st_ino) in seen:
                    continue
                seen.add((status.st_dev, status.st_ino))
            total += status.st_blocks * 512
    return total


def check_target(target):
    if not target.is_absolute() or target.is_symlink() or not target.is_dir():
        raise ValueError("Cargo target must be an existing absolute directory, not a symlink")
    if target.resolve() in (Path("/"), Path.home().resolve(), Path.cwd().resolve()):
        raise ValueError("Refusing to clean a source, home, or filesystem root")
    initialize_cache_tag(target)


def reclaim(target, fits):
    """Empty the target, workspace artifacts first, until fits() holds."""
    if fits():
        return True
    for scope in (["--workspace"], []):
        print(f"Reclaiming {'workspace artifacts' if scope else 'artifacts'} "
              f"from {target}", flush=True)
        subprocess.run(["cargo", "clean", *scope, "--target-dir", str(target)], check=True)
        target.mkdir(exist_ok=True)
        initialize_cache_tag(target)
        if fits():
            return True
    return False


def keep_target(target, maximum_gib, floor_gib):
    """Whether this runner's kept tree may be used: the host must keep floor_gib
    free even after the tree grows to its cap. The tree is emptied, workspace
    artifacts first, before the job falls back to a job-scoped tree."""
    if maximum_gib <= 0 or floor_gib <= 0:
        raise ValueError("Kept Cargo target cap and host floor must be positive")
    check_target(target)

    def has_headroom():
        free = shutil.disk_usage(target).free
        used = tree_size(target)
        headroom = free - max(0, maximum_gib * 1024**3 - used)
        print(f"Kept Cargo target: {used / 1024**3:.1f} of {maximum_gib} GiB; host keeps "
              f"{headroom / 1024**3:.1f} GiB free at that cap; {floor_gib} GiB floor",
              flush=True)
        return headroom >= floor_gib * 1024**3

    return reclaim(target, has_headroom)


def ensure_build_space(target, minimum_gib, maximum_gib=None):
    if minimum_gib <= 0:
        raise ValueError("CARGO_BUILD_MIN_FREE_GIB must be positive")
    if maximum_gib is not None and maximum_gib <= 0:
        raise ValueError("CARGO_TARGET_MAX_GIB must be positive")
    check_target(target)

    required = minimum_gib * 1024**3

    def has_space():
        free = shutil.disk_usage(target).free
        report = f"Cargo build volume: {free / 1024**3:.1f} GiB free; {minimum_gib} GiB required"
        fits = free >= required
        if maximum_gib is not None:
            used = tree_size(target)
            report += f"; target holds {used / 1024**3:.1f} GiB of {maximum_gib} GiB allowed"
            fits = fits and used <= maximum_gib * 1024**3
        print(report, flush=True)
        return fits

    if not reclaim(target, has_space):
        raise RuntimeError("Insufficient build space after cleaning this runner's Cargo cache; "
                           "free additional space on the host before retrying")


def optional_gib(name):
    value = os.environ.get(name, "")
    return int(value) if value else None


def main(arguments):
    if arguments == ["--limits"]:
        print(*target_limits(os.environ))
        return 0
    if len(arguments) == 2 and arguments[0] == "--keep":
        maximum_gib, floor_gib = target_limits(os.environ)
        return 0 if keep_target(Path(arguments[1]), maximum_gib, floor_gib) else USE_JOB_TREE
    if len(arguments) != 1 or arguments[0].startswith("-"):
        raise ValueError(f"usage: {sys.argv[0]} [--limits | --keep <kept-target> | "
                         "<runner-cargo-target>]")
    ensure_build_space(Path(arguments[0]), int(os.environ.get("CARGO_BUILD_MIN_FREE_GIB", "64")),
                       optional_gib("CARGO_TARGET_MAX_GIB"))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (ValueError, RuntimeError, OSError, subprocess.CalledProcessError) as error:
        print(f"::error::{error}", file=sys.stderr)
        sys.exit(1)
