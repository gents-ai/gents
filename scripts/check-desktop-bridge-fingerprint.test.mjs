import assert from "node:assert/strict";
import test from "node:test";

import { bridgePackageVersion, compareBridgeFingerprint } from "./check-desktop-bridge-fingerprint.mjs";

const exact = { version: "8.0", hash: "canonical-hash", package: "0.17.0" };

test("accepts the exact generated bridge fingerprint", () => {
  assert.doesNotThrow(() => compareBridgeFingerprint(exact, { ...exact }));
});

for (const key of ["version", "hash", "package"]) {
  test(`rejects a stale generated ${key}`, () => {
    assert.throws(
      () => compareBridgeFingerprint(exact, { ...exact, [key]: `stale-${key}` }),
      new RegExp(`fingerprint ${key} drifted.*Regenerate with`),
    );
  });
}

test("reads only the workspace.package version after rust-version and comments", () => {
  const workspace = `[workspace.package]
# version = "commented-out"
rust-version = "1.90"
version = "0.17.0"
[workspace.dependencies]
version = "wrong-table"`;
  const cargo = `[package]
name = "gents-desktop-bridge"
version.workspace = true`;
  assert.equal(bridgePackageVersion(workspace, cargo), "0.17.0");
});

test("does not scan past workspace.package into the next table", () => {
  const workspace = `[workspace.package]
rust-version = "1.90"
[[bin]]
version = "wrong-table"`;
  assert.throws(
    () => bridgePackageVersion(workspace, `[package]\nversion.workspace = true`),
    /could not read workspace package version/,
  );
});

test("reads a literal package version without matching rust-version or comments", () => {
  const cargo = `[package]
# version.workspace = true
rust-version = "1.90"
# version = "commented-out"
version = "0.18.0"
[dependencies]
version = "wrong-table"`;
  assert.equal(bridgePackageVersion("[workspace.package]\nversion = \"0.17.0\"", cargo), "0.18.0");
});
