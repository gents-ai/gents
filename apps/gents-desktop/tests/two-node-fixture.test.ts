import { describe, expect, it, vi } from "vitest";

import { startTwoIndependentNodes } from "./live-bridge-runner/two-node";

function fixture(name: string) {
  return {
    nodeDid: `did:key:${name}`,
    baseUrl: `http://127.0.0.1/${name}`,
    dataRoot: `/test/${name}`,
    dispose: vi.fn(async () => {}),
  };
}

describe("two-node live fixture", () => {
  it("starts two separate nodes and cleans up both", async () => {
    const first = fixture("first");
    const second = fixture("second");
    const start = vi.fn().mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    const pair = await startTwoIndependentNodes(start);
    expect(pair.first).toBe(first);
    expect(pair.second).toBe(second);
    await pair.dispose();
    await pair.dispose();
    expect(first.dispose).toHaveBeenCalledOnce();
    expect(second.dispose).toHaveBeenCalledOnce();
  });

  it.each(["nodeDid", "baseUrl", "dataRoot"] as const)(
    "rejects a shared %s and cleans up both processes",
    async (property) => {
      const first = fixture("first");
      const second = { ...fixture("second"), [property]: first[property] };
      const start = vi.fn().mockResolvedValueOnce(first).mockResolvedValueOnce(second);
      await expect(startTwoIndependentNodes(start)).rejects.toThrow(
        "independent nodes",
      );
      expect(first.dispose).toHaveBeenCalledOnce();
      expect(second.dispose).toHaveBeenCalledOnce();
    },
  );

  it("stops the first node if the second does not start", async () => {
    const first = fixture("first");
    const start = vi
      .fn()
      .mockResolvedValueOnce(first)
      .mockRejectedValueOnce(new Error("startup"));
    await expect(startTwoIndependentNodes(start)).rejects.toThrow("startup");
    expect(first.dispose).toHaveBeenCalledOnce();
  });

  it("attempts both cleanups and reports a cleanup failure", async () => {
    const first = fixture("first");
    const second = fixture("second");
    first.dispose.mockRejectedValueOnce(new Error("cleanup"));
    const start = vi.fn().mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    const pair = await startTwoIndependentNodes(start);
    await expect(pair.dispose()).rejects.toThrow("Could not stop both nodes");
    expect(second.dispose).toHaveBeenCalledOnce();
  });
});
