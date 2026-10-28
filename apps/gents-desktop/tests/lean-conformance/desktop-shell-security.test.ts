import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterEach, expect, it, vi } from "vitest";
import { buildUxHost } from "../../src/ui/hooks/useUxHost";
import { node, testApp } from "../app-fixture";
import {
  configureUxDoors,
  resetRuntimeDoor,
  scanRuntimeUxPlugins,
} from "@/contrib/runtime-door";
import { resetUxPluginStore, setUxPluginEnabled } from "@/contrib/plugins-store";

const run = promisify(execFile);
type Cases = {
  plugins: {
    development: boolean;
    manifest: boolean | null;
    choice: boolean | null;
    expected: boolean;
  }[];
  send: {
    gesture: boolean;
    confirmed: boolean;
    sameTarget: boolean;
    expected: boolean;
  }[];
};

afterEach(() => {
  vi.restoreAllMocks();
  delete (navigator as { userActivation?: unknown }).userActivation;
  resetRuntimeDoor();
  resetUxPluginStore();
});

it("binds plugin loading and message consent to the executable shell model", async () => {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../..",
    "crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.DesktopShellSecurity"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "Proofs/Conformance/DesktopShellSecurity.lean"],
    { cwd },
  );
  const cases = JSON.parse(stdout) as Cases;
  expect(cases.plugins).toHaveLength(18);
  for (const row of cases.plugins) {
    resetRuntimeDoor();
    resetUxPluginStore();
    if (row.choice !== null) await setUxPluginEnabled("widget", row.choice);
    const source = vi.fn().mockRejectedValue(new Error("source reached"));
    configureUxDoors({
      list: async () => ({
        plugins: [
          {
            id: row.development ? "widget" : "acme/tools/widget",
            door: row.development ? "dev" : "pack",
            producer: "file",
            file: "/plugins/widget.js",
            defaultEnabled: row.manifest,
          },
        ],
      }),
      source,
    });
    await scanRuntimeUxPlugins();
    expect(source.mock.calls.length, JSON.stringify(row)).toBe(row.expected ? 1 : 0);
  }
  expect(cases.send).toHaveLength(8);
  for (const row of cases.send) {
    Object.defineProperty(navigator, "userActivation", {
      configurable: true,
      value: { isActive: row.gesture },
    });
    const app = testApp({
      deployments: [node({ nodeDid: "did:a" })],
      selection: { sessionId: "s1", nodeDid: "did:a", agentId: "a1" },
    });
    const send = vi.spyOn(app.actions, "sendMessage").mockResolvedValue(null);
    const confirm = vi.spyOn(window, "confirm").mockImplementation(() => {
      if (!row.sameTarget) app.stores.selection.setState({ sessionId: "s2" });
      return row.confirmed;
    });
    const result = buildUxHost(app, () => ({ name: "sessions" })).shell.send(
      "Exact proposed text ✓",
    );
    if (row.expected) await expect(result).resolves.toBeUndefined();
    else await expect(result).rejects.toThrow("ctx.send:");
    expect(send.mock.calls.length, JSON.stringify(row)).toBe(row.expected ? 1 : 0);
    if (row.gesture)
      expect(confirm).toHaveBeenCalledWith(
        expect.stringContaining("Exact proposed text ✓"),
      );
    else expect(confirm).not.toHaveBeenCalled();
    confirm.mockRestore();
  }
}, 240_000);
