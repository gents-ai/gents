import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { emitUxEvent, listenerCount } from "@/contrib/events";
import {
  createUxContext,
  installUxHost,
  UndeclaredContributionError,
  type UxHost,
} from "@/contrib/plugin";
import { registry } from "@/contrib/registry";
import {
  AGENT_SECTIONS_AREA,
  NAV_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
} from "@/contrib/types";

function fakeHost(): UxHost & { sent: string[]; calls: Array<[string, unknown]> } {
  const sent: string[] = [];
  const calls: Array<[string, unknown]> = [];
  return {
    sent,
    calls,
    shell: {
      route: () => ({ name: "sessions" }),
      navigate: () => {},
      deployments: () => [],
      selectedDeployment: () => null,
      selectedSessionId: () => null,
      send: async (content) => {
        sent.push(content);
      },
    },
    bridge: async (command, args) => {
      calls.push([command, args]);
      return { ok: true } as never;
    },
    plugin: async (pack, name, input) => ({ pack, name, input }),
    os: {
      openExternal: async () => true,
      revealPath: async () => true,
      pickDirectory: async () => null,
    },
  };
}

let host: ReturnType<typeof fakeHost>;
beforeEach(() => {
  host = fakeHost();
  installUxHost(host);
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
    removeItem: (key: string) => store.delete(key),
  });
});

const collected: Array<() => void> = [];
afterEach(() => {
  collected.splice(0).forEach((dispose) => dispose());
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("ux plugin context", () => {
  it("namespaces ids and stamps provenance", () => {
    const ctx = createUxContext("acme", { onDispose: (d) => collected.push(d) });
    ctx.register({ id: "row", area: SESSION_HEADER_ACTIONS_AREA, render: () => null });
    const [c] = registry.getArea(SESSION_HEADER_ACTIONS_AREA);
    expect(c?.id).toBe("acme:row");
    expect(c?.source).toBe("plugin:acme");
  });

  it("tears down every registration, event listener and timer through the disposers", () => {
    vi.useFakeTimers();
    const disposers: Array<() => void> = [];
    const ctx = createUxContext("acme", { onDispose: (d) => disposers.push(d) });
    ctx.register({ id: "row", area: SESSION_HEADER_ACTIONS_AREA, render: () => null });
    const heard = vi.fn();
    ctx.onEvent("client-updated", heard);
    const ticked = vi.fn();
    ctx.setInterval(ticked, 10);
    const listener = vi.fn();
    ctx.addEventListener(window, "resize", listener);
    expect(listenerCount("client-updated")).toBe(1);

    disposers.forEach((d) => d());

    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA)).toHaveLength(0);
    expect(listenerCount("client-updated")).toBe(0);
    emitUxEvent({ type: "client-updated", payload: {} });
    expect(heard).not.toHaveBeenCalled();
    vi.advanceTimersByTime(50);
    expect(ticked).not.toHaveBeenCalled();
    window.dispatchEvent(new Event("resize"));
    expect(listener).not.toHaveBeenCalled();
  });

  it("refuses a contribution into an undeclared area, by name", () => {
    const ctx = createUxContext("acme", {
      onDispose: (d) => collected.push(d),
      declared: { areas: [NAV_AREA], directives: [] },
    });
    expect(() =>
      ctx.register({ id: "page", area: AGENT_SECTIONS_AREA, data: {} }),
    ).toThrow(UndeclaredContributionError);
    expect(() =>
      ctx.register({ id: "page", area: AGENT_SECTIONS_AREA, data: {} }),
    ).toThrow(/agent\.sections/);
    expect(registry.getArea(AGENT_SECTIONS_AREA)).toHaveLength(0);
  });

  it("refuses an undeclared directive name but admits a declared one", () => {
    const ctx = createUxContext("acme", {
      onDispose: (d) => collected.push(d),
      declared: { areas: [TRANSCRIPT_DIRECTIVE_AREA], directives: ["board"] },
    });
    expect(() =>
      ctx.register({
        id: "d",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        data: { name: "other", render: () => null },
      }),
    ).toThrow(/not declared/);
    ctx.register({
      id: "d",
      area: TRANSCRIPT_DIRECTIVE_AREA,
      data: { name: "board", render: () => null },
    });
    expect(registry.getArea(TRANSCRIPT_DIRECTIVE_AREA)).toHaveLength(1);
  });

  it("leaves a plugin with no declaration ungated (bundled)", () => {
    const ctx = createUxContext("bundled", { onDispose: (d) => collected.push(d) });
    expect(() =>
      ctx.register({ id: "x", area: "anything.at.all", data: {} }),
    ).not.toThrow();
  });

  it("routes bridge, send and plugin through the installed host", async () => {
    const ctx = createUxContext("acme", {
      onDispose: (d) => collected.push(d),
      pack: "ns/pack",
    });
    await ctx.bridge("desktop_something", { a: 1 });
    expect(host.calls).toEqual([["desktop_something", { a: 1 }]]);
    await ctx.send("hi");
    expect(host.sent).toEqual(["hi"]);
    await expect(ctx.plugin("tool", { x: 1 })).resolves.toEqual({
      pack: "ns/pack",
      name: "tool",
      input: { x: 1 },
    });
  });

  it("rejects ctx.plugin for a plugin that ships in no pack", async () => {
    const ctx = createUxContext("loose", { onDispose: (d) => collected.push(d) });
    await expect(ctx.plugin("tool", {})).rejects.toThrow(/needs a pack/);
  });

  it("scopes storage under gents.ux.<id>", () => {
    const ctx = createUxContext("acme", { onDispose: (d) => collected.push(d) });
    ctx.storage.set("k", { v: 1 });
    expect(window.localStorage.getItem("gents.ux.acme.k")).toBe('{"v":1}');
    expect(ctx.storage.get("k", null)).toEqual({ v: 1 });
    ctx.storage.remove("k");
    expect(ctx.storage.get("k", "gone")).toBe("gone");
  });

  it("isolates a throwing event listener from the others", () => {
    const ctx = createUxContext("acme", { onDispose: (d) => collected.push(d) });
    const bad = vi.fn(() => {
      throw new Error("boom");
    });
    const good = vi.fn();
    ctx.onEvent("*", bad);
    ctx.onEvent("client-updated", good);
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    emitUxEvent({ type: "client-updated", payload: 1 });
    expect(good).toHaveBeenCalledTimes(1);
    err.mockRestore();
  });
});
