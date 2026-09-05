#!/usr/bin/env python3
"""Collect production lens bytes from Cargo's emitted paths without rebuilding."""
import hashlib
import json
import mmap
import os
from pathlib import Path
import shutil
import sys


target = Path(os.environ["CARGO_TARGET_DIR"])
destination = Path(sys.argv[1])
destination.mkdir(parents=True, exist_ok=True)
variables = {
    "GENTS_LENS_WORKSPACE_CAPABILITY_WASM_PATH",
    "GENTS_LENS_WORKSPACE_RECEIPT_CAPABILITY_WASM_PATH",
}
manifest = {"target_dir": str(target), "github_sha": os.environ.get("GITHUB_SHA"),
            "builds": [], "test_binaries": []}
artifacts = []
for output in sorted(target.glob("debug/build/gents-migration-*/output")):
    build = output.parent
    capture = destination / build.name
    capture.mkdir(exist_ok=True)
    record = {"build_output": str(output), "modified_ns": output.stat().st_mtime_ns,
              "lenses": []}
    for name in ("output", "stderr", "root-output"):
        source = build / name
        if source.is_file():
            shutil.copy2(source, capture / name)
    for line in output.read_text().splitlines():
        if not line.startswith("cargo:rustc-env="):
            continue
        variable, value = line.removeprefix("cargo:rustc-env=").split("=", 1)
        if variable not in variables:
            continue
        source = Path(value)
        lens = {"variable": variable, "emitted_path": value, "exists": source.is_file()}
        if source.is_file():
            data = source.read_bytes()
            lens.update(size=len(data), sha256=hashlib.sha256(data).hexdigest())
            shutil.copy2(source, capture / source.name)
            artifacts.append((str(output), variable, data))
        record["lenses"].append(lens)
    # Include the flags Cargo fingerprinted, including cached dependency inputs.
    nested = build / "out/lens-target"
    for fingerprint in nested.glob("**/.fingerprint/*/*.json"):
        saved = capture / "fingerprints" / fingerprint.relative_to(nested)
        saved.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(fingerprint, saved)
    manifest["builds"].append(record)

# An emitted path alone may belong to an older build. Record which exact bytes
# occur in each extant integration executable; retain timestamps, never label a
# persistent-cache candidate as the executable from this run without evidence.
for binary in sorted(target.glob("debug/deps/workspace_path_capability-*")):
    if not binary.is_file() or not os.access(binary, os.X_OK) or binary.stat().st_size == 0:
        continue
    record = {"path": str(binary), "modified_ns": binary.stat().st_mtime_ns,
              "size": binary.stat().st_size, "embedded_lenses": []}
    with binary.open("rb") as handle, mmap.mmap(handle.fileno(), 0, access=mmap.ACCESS_READ) as data:
        for output, variable, wasm in artifacts:
            offset = data.find(wasm)
            record["embedded_lenses"].append({"build_output": output, "variable": variable,
                                               "byte_offset": offset})
    manifest["test_binaries"].append(record)
    depfile = binary.with_suffix(".d")
    if depfile.is_file():
        shutil.copy2(depfile, destination / depfile.name)
for fingerprint in target.glob("debug/.fingerprint/gents-migration-*/*.json"):
    saved = destination / "outer-fingerprints" / fingerprint.parent.name / fingerprint.name
    saved.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(fingerprint, saved)
(destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
