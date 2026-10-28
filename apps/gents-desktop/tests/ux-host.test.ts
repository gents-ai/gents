/* The host a UX plugin talks through, built over the app's stores: what each
   verb sends across the bridge and what it reads. The bridge commands take
   their arguments by Rust parameter name, so the shape of every `call` is the
   contract under test. */
import { createDesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { createMemoryTransport } from "@source-inc/gents-desktop-client/testing";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { buildUxHost } from "../src/ui/hooks/useUxHost";
import { node, testApp } from "./app-fixture";

/* the transport under the client's adapter: every bridge command the host
   reaches lands here with the argument object Tauri would deserialize */
const call = vi.fn();
const apiOver = (answer: (command: string, args: unknown) => unknown) =>
  createDesktopApiAdapter(
    createMemoryTransport({
      handlers: new Proxy(
        {},
        {
          get: (_, command: string) => (args: unknown) => {
            call(command, args);
            return answer(command, args);
          },
          has: () => true,
        },
      ),
    }),
  );
const appOver = (answer: (command: string, args: unknown) => unknown) =>
  testApp({ api: apiOver(answer) });

const route = () => ({ name: "sessions" as const });

describe("buildUxHost: ctx.send", () => {
  beforeEach(() => vi.spyOn(window, "confirm").mockReturnValue(true));
  const gesture = (active: boolean) => {
    Object.defineProperty(navigator, "userActivation", {
      configurable: true,
      value: { isActive: active, hasBeenActive: active },
    });
  };
  afterEach(() => {
    delete (navigator as { userActivation?: unknown }).userActivation;
    vi.restoreAllMocks();
  });

  it("refuses without a selected session, so a plugin never opens one", async () => {
    gesture(true);
    const app = testApp({ selection: { sessionId: null } });
    const sendMessage = vi.spyOn(app.actions, "sendMessage");
    const host = buildUxHost(app, route);
    await expect(host.shell.send("hi")).rejects.toThrow("no session is selected");
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it("refuses outside a user gesture: a timer, an effect or a mount cannot send", async () => {
    gesture(false);
    const app = testApp({ selection: { sessionId: "s1", nodeDid: "did:a" } });
    const sendMessage = vi.spyOn(app.actions, "sendMessage");
    const host = buildUxHost(app, route);
    await expect(host.shell.send("hi")).rejects.toThrow("user gesture");
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it("sends in the selected session from a gesture, with the selected agent", async () => {
    gesture(true);
    const app = testApp({
      selection: { sessionId: "s1", nodeDid: "did:a", agentId: "b1" },
    });
    const sendMessage = vi.spyOn(app.actions, "sendMessage").mockResolvedValue(null);
    const host = buildUxHost(app, route);
    await host.shell.send("hi");
    expect(window.confirm).toHaveBeenCalledWith(expect.stringContaining("hi"));
    expect(sendMessage).toHaveBeenCalledWith("hi", "b1");
  });

  it("does not send when the proposed text is declined", async () => {
    gesture(true);
    vi.mocked(window.confirm).mockReturnValue(false);
    const app = testApp({ selection: { sessionId: "s1", nodeDid: "did:a" } });
    const send = vi.spyOn(app.actions, "sendMessage");
    await expect(buildUxHost(app, route).shell.send("proposed text")).rejects.toThrow(
      "not confirmed",
    );
    expect(send).not.toHaveBeenCalled();
  });

  it.each(["sessionId", "nodeDid", "agentId"] as const)(
    "does not retarget a confirmed message after %s changes",
    async (field) => {
      gesture(true);
      const app = testApp({
        selection: { sessionId: "s1", nodeDid: "did:a", agentId: "a1" },
      });
      vi.mocked(window.confirm).mockImplementation(() => {
        app.stores.selection.setState({ [field]: "different" });
        return true;
      });
      const send = vi.spyOn(app.actions, "sendMessage");
      await expect(buildUxHost(app, route).shell.send("text")).rejects.toThrow(
        "selected chat changed",
      );
      expect(send).not.toHaveBeenCalled();
    },
  );

  it("allows only one confirmation at a time and recovers after cancellation", async () => {
    gesture(true);
    vi.mocked(window.confirm).mockReturnValue(false);
    const host = buildUxHost(testApp({ selection: { sessionId: "s1" } }), route);
    const first = host.shell.send("first");
    await expect(host.shell.send("second")).rejects.toThrow(
      "already awaiting confirmation",
    );
    await expect(first).rejects.toThrow("not confirmed");
    await expect(host.shell.send("third")).rejects.toThrow("not confirmed");
    expect(window.confirm).toHaveBeenCalledTimes(2);
  });
});

describe("buildUxHost: bridge verbs", () => {
  it("calls a pack's .afb plugin under the caller's listing, fields under `request`", async () => {
    call.mockReset();
    const host = buildUxHost(
      appOver(() => ({ output: { tasks: [{ id: "1" }] } })),
      route,
    );
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
    const host = buildUxHost(
      appOver(() => {
        throw new Error("pack examples/board_pack is not installed");
      }),
      route,
    );
    await expect(
      host.plugin!("examples/board_pack/board", "board_store", {}),
    ).rejects.toThrow("not installed");
  });

  it("opens an external url by `url` and reports a refusal as false", async () => {
    call.mockReset();
    let refuse = false;
    const app = appOver(() => {
      if (refuse) throw new Error("scheme");
      return undefined;
    });
    /* in the shell the bridge opens it; a refusal there falls back to the
       page's opener, which the host reports as a failure only when that
       throws too */
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: {
        invoke: () => {
          throw new Error("opener refused");
        },
      },
    });
    try {
      const host = buildUxHost(app, route);
      expect(await host.os.openExternal("https://example.test/")).toBe(true);
      expect(call).toHaveBeenCalledWith("desktop_open_external_url", {
        url: "https://example.test/",
      });
      refuse = true;
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      expect(await host.os.openExternal("javascript:1")).toBe(false);
      warn.mockRestore();
    } finally {
      delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    }
  });

  it("exposes no raw bridge verb", () => {
    const host = buildUxHost(testApp(), route);
    expect("bridge" in host).toBe(false);
    /* data: the declared-surface door; onShellChange: the host's route
       feed for the frames, never a plugin-facing verb */
    expect(Object.keys(host).sort()).toEqual([
      "data",
      "onShellChange",
      "os",
      "plugin",
      "shell",
    ]);
  });
});

describe("buildUxHost: shell reads", () => {
  it("reads the route, the selection and the fleet live from the stores", () => {
    const app = testApp({
      deployments: [node({ nodeDid: "did:key:a" }), node({ nodeDid: "did:key:b" })],
    });
    const host = buildUxHost(app, route);
    expect(host.shell.route()).toEqual({ name: "sessions" });
    expect(host.shell.deployments().map((d) => d.nodeDid)).toEqual([
      "did:key:a",
      "did:key:b",
    ]);
    /* nothing selected: the first node, as the app's own useSelectedNode */
    expect(host.shell.selectedDeployment()?.nodeDid).toBe("did:key:a");
    expect(host.shell.selectedSessionId()).toBeNull();

    app.stores.selection.setState({ nodeDid: "did:key:b", sessionId: "s-9" });
    expect(host.shell.selectedDeployment()?.nodeDid).toBe("did:key:b");
    expect(host.shell.selectedSessionId()).toBe("s-9");
  });
});
