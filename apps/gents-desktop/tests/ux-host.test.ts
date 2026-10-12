/* The host a UX plugin talks through, built over the app's stores: what each
   verb sends across the bridge and what it reads. The bridge commands take
   their arguments by Rust parameter name, so the shape of every `call` is the
   contract under test. */
import { describe, expect, it, vi } from "vitest";

const call = vi.fn();
vi.mock("../src/ui/screens/agent/bridgeCall", () => ({
  call: (...args: unknown[]) => call(...args),
  message: (error: unknown) => String(error),
}));

import { buildUxHost } from "../src/ui/hooks/useUxHost";
import { node, testApp } from "./app-fixture";

const route = () => ({ name: "sessions" as const });

describe("buildUxHost: bridge verbs", () => {
  it("calls a pack's .afb plugin under the caller's listing, fields under `request`", async () => {
    call.mockReset();
    call.mockResolvedValue({ output: { tasks: [{ id: "1" }] } });
    const host = buildUxHost(testApp(), route);
    const output = await host.plugin!("examples/board_pack/board", "board_store", {
      action: "list",
    });
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith("desktop_plugin_call", {
      request: {
        ux: "examples/board_pack/board",
        name: "board_store",
        input: { action: "list" },
      },
    });
    expect(output).toEqual({ tasks: [{ id: "1" }] });
  });

  it("rejects a plugin call the bridge refused", async () => {
    call.mockReset();
    call.mockRejectedValue(new Error("pack examples/board_pack is not installed"));
    const host = buildUxHost(testApp(), route);
    await expect(
      host.plugin!("examples/board_pack/board", "board_store", {}),
    ).rejects.toThrow("not installed");
  });

  it("opens an external url by `url` and reports a refusal as false", async () => {
    call.mockReset();
    call.mockResolvedValueOnce(undefined);
    const host = buildUxHost(testApp(), route);
    expect(await host.os.openExternal("https://example.test/")).toBe(true);
    expect(call).toHaveBeenCalledWith("desktop_open_external_url", {
      url: "https://example.test/",
    });
    call.mockRejectedValueOnce(new Error("scheme"));
    expect(await host.os.openExternal("javascript:1")).toBe(false);
  });

  it("exposes no raw bridge verb", () => {
    const host = buildUxHost(testApp(), route);
    expect("bridge" in host).toBe(false);
    expect(Object.keys(host).sort()).toEqual(["os", "plugin", "shell"]);
  });
});

describe("buildUxHost: shell reads", () => {
  it("reads the route, the selection and the fleet live from the stores", () => {
    const app = testApp({
      deployments: [node({ agentDid: "did:key:a" }), node({ agentDid: "did:key:b" })],
    });
    const host = buildUxHost(app, route);
    expect(host.shell.route()).toEqual({ name: "sessions" });
    expect(host.shell.deployments().map((d) => d.agentDid)).toEqual([
      "did:key:a",
      "did:key:b",
    ]);
    /* nothing selected: the first node, as the app's own useSelectedNode */
    expect(host.shell.selectedDeployment()?.agentDid).toBe("did:key:a");
    expect(host.shell.selectedSessionId()).toBeNull();

    app.stores.selection.setState({ agentDid: "did:key:b", sessionId: "s-9" });
    expect(host.shell.selectedDeployment()?.agentDid).toBe("did:key:b");
    expect(host.shell.selectedSessionId()).toBe("s-9");
  });
});
