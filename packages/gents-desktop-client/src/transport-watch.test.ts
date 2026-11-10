import { describe, expect, it, vi } from "vitest";
import { tauriTransport } from "./transport.js";

const mocked = vi.hoisted(() => ({
  invoke: vi.fn(),
  channels: [] as Array<{ onmessage: (value: unknown) => void }>,
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocked.invoke,
  Channel: class {
    onmessage: (value: unknown) => void = () => {};
    constructor() {
      mocked.channels.push(this);
    }
  },
}));

describe("pack data watch transport", () => {
  it("delivers through its own channel and stops it once", async () => {
    mocked.invoke.mockReset();
    mocked.channels.length = 0;
    mocked.invoke
      .mockResolvedValueOnce("watch-one")
      .mockResolvedValue(undefined);
    const observed = vi.fn();
    const cancel = await tauriTransport().watchUxData!(
      {
        ux: "gents/game/page",
        nodeDid: "did:node",
        surface: "board",
        tool: "list",
      },
      observed,
    );
    expect(mocked.invoke).toHaveBeenCalledWith(
      "plugin:gents-desktop-bridge|desktop_ux_data_watch",
      expect.objectContaining({
        request: expect.objectContaining({ nodeDid: "did:node" }),
      }),
    );
    const event = {
      kind: "snapshot",
      value: { kind: "query", output: { results: [] } },
    };
    mocked.channels[0]!.onmessage(event);
    expect(observed).toHaveBeenCalledWith(event);
    cancel();
    cancel();
    mocked.channels[0]!.onmessage(event);
    expect(observed).toHaveBeenCalledTimes(1);
    expect(mocked.invoke).toHaveBeenCalledTimes(2);
    expect(mocked.invoke).toHaveBeenLastCalledWith(
      "plugin:gents-desktop-bridge|desktop_ux_data_unwatch",
      { id: "watch-one" },
    );
  });
});
