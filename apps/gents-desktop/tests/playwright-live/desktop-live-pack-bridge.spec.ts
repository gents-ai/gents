import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test } from "../playwright/desktopTest";
import { LiveBridgeRunner } from "../live-bridge-runner";

test("live bridge loads actual local UX modules and refuses undeclared data", async () => {
  const runner = await LiveBridgeRunner.start();
  try {
    if (!runner.dataRoot)
      throw new Error("runner did not expose its isolated data root");
    const path = join(runner.dataRoot, "agent-home", "ux-plugins", "fixture");
    await mkdir(path, { recursive: true });
    const source = "export default { id: 'fixture' };";
    await writeFile(join(path, "plugin.js"), source);
    const listed = await runner.adapter.listUxPlugins();
    expect(listed.plugins).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ id: "fixture", door: "dev", producer: "file" }),
      ]),
    );
    expect((await runner.adapter.readUxPluginSource("fixture")).source).toBe(source);
    await expect(
      runner.adapter.callUxData({
        ux: "fixture",
        nodeDid: runner.nodeDid,
        surface: "board",
        tool: "move",
        args: {},
      }),
    ).rejects.toThrow(/not declared/);
    await expect(runner.adapter.readUxPluginSource("missing")).rejects.toThrow();
  } finally {
    await runner.dispose();
  }
});
