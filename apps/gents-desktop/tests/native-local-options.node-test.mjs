import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { parseNativeLocalOptions } from "./native-local-options.mjs";

test("normal acceptance retains Keychain custody", () => {
  assert.deepEqual(parseNativeLocalOptions([]), {
    skipBuild: false,
    fileStoreKeys: false,
    help: false,
  });
  assert.equal(
    parseNativeLocalOptions([], { GENTS_E2E_FILE_STORE_KEYS: "0" }).fileStoreKeys,
    false,
  );
});

test("file custody is explicit and composes with frozen binaries", () => {
  assert.deepEqual(parseNativeLocalOptions(["--skip-build", "--file-store-keys"]), {
    skipBuild: true,
    fileStoreKeys: true,
    help: false,
  });
  assert.equal(
    parseNativeLocalOptions([], { GENTS_E2E_FILE_STORE_KEYS: "1" }).fileStoreKeys,
    true,
  );
  assert.equal(
    parseNativeLocalOptions(["--file-store-keys"], { GENTS_E2E_FILE_STORE_KEYS: "0" })
      .fileStoreKeys,
    true,
  );
});

test("invalid options and ambiguous custody configuration fail explicitly", () => {
  assert.throws(() => parseNativeLocalOptions(["--disable-auth"]), /Unsupported/);
  for (const value of ["", "true", "file", "2"]) {
    assert.throws(
      () => parseNativeLocalOptions([], { GENTS_E2E_FILE_STORE_KEYS: value }),
      /must be 0 or 1/,
    );
  }
});

test("runner help exits before platform checks, builds or creating a run home", () => {
  const result = spawnSync(
    process.execPath,
    [new URL("./run-native-local-e2e.mjs", import.meta.url).pathname, "--help"],
    {
      encoding: "utf8",
      env: { ...process.env, GENTS_E2E_FILE_STORE_KEYS: "0" },
    },
  );
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /--file-store-keys/);
  assert.match(result.stdout, /DID signing and ACP remain enabled/);
  assert.doesNotMatch(result.stdout, /Native acceptance artifacts:/);
});
