import { describe, expect, it } from "vitest";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

// Bounded argument-validation controls for tests/run-live-test.mjs. These run
// the helper directly and the script as a child process (without launching
// vitest or any live backend).

import { resolveLiveProvider } from "./run-live-test.mjs";

const SCRIPT = resolve(import.meta.dirname, "run-live-test.mjs");

describe("resolveLiveProvider", () => {
  it("rejects a missing provider with an actionable message (negative control)", () => {
    expect(() => resolveLiveProvider(null, {})).toThrowError(
      /missing live provider.*--provider.*GENTS_TAURI_LIVE_PROVIDER/s,
    );
  });

  it("uses the --provider flag when supplied (positive control)", () => {
    expect(resolveLiveProvider("OpenAiCompatible", {})).toBe("OpenAiCompatible");
  });

  it("uses GENTS_TAURI_LIVE_PROVIDER as the environment override", () => {
    expect(
      resolveLiveProvider(null, { GENTS_TAURI_LIVE_PROVIDER: "OpenAiCompatible" }),
    ).toBe("OpenAiCompatible");
  });

  it("prefers the explicit flag over the environment override", () => {
    expect(
      resolveLiveProvider("OpenAiCompatible", {
        GENTS_TAURI_LIVE_PROVIDER: "SomethingElse",
      }),
    ).toBe("OpenAiCompatible");
  });
});

describe("run-live-test.mjs script guard", () => {
  const runScript = (env: NodeJS.ProcessEnv) => {
    return spawnSync(process.execPath, [SCRIPT, "--suite", "invalid-guard-control"], {
      encoding: "utf8",
      env: { ...process.env, GENTS_TAURI_LIVE_PROVIDER: undefined, ...env },
      timeout: 5000,
    });
  };

  it("fails fast without launching vitest when no provider is supplied (negative control)", () => {
    const result = runScript({});
    expect(result.status).not.toBe(0);
    expect(result.stderr).toMatch(/missing live provider/);
    expect(result.stderr).toMatch(/--provider.*GENTS_TAURI_LIVE_PROVIDER/s);
    expect(result.stderr).not.toMatch(/vitest|RUN v/i);
  });

  it("passes the provider guard and reaches suite validation without launching a backend", () => {
    const result = runScript({ GENTS_TAURI_LIVE_PROVIDER: "OpenAiCompatible" });
    expect(result.status).toBe(1);
    expect(result.stderr).toMatch(/unknown live test suite "invalid-guard-control"/);
    expect(result.stderr).not.toMatch(/missing live provider/);
  });
});
