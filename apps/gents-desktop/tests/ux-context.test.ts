import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { resolveDirective } from "@/contrib/directives";
import { emitUxEvent, listenerCount } from "@/contrib/events";
import {
  createUxContext,
  installUxHost,
  isGentsUxPlugin,
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
    plugin: async (listing, name, input) => ({ listing, name, input }),
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

  it("routes send and plugin through the installed host, plugin under its listing", async () => {
    const ctx = createUxContext("acme", {
      onDispose: (d) => collected.push(d),
      listing: "ns/pack/acme",
    });
    await ctx.send("hi");
    expect(host.sent).toEqual(["hi"]);
    await expect(ctx.plugin("tool", { x: 1 })).resolves.toEqual({
      listing: "ns/pack/acme",
      name: "tool",
      input: { x: 1 },
    });
  });

  it("uses the selected node for each pack plugin call", async () => {
    let nodeDid = "did:key:first";
    host.shell.selectedDeployment = () => ({ nodeDid }) as never;
    const plugin = vi.fn(async () => ({}));
    host.plugin = plugin;
    const ctx = createUxContext("board", { listing: "ns/pack/board" });
    await ctx.plugin("store", { op: "read" });
    nodeDid = "did:key:second";
    await ctx.plugin("store", { op: "write" });
    expect(plugin.mock.calls).toEqual([
      ["ns/pack/board", "store", { op: "read" }, "did:key:first"],
      ["ns/pack/board", "store", { op: "write" }, "did:key:second"],
    ]);
  });

  it("offers no raw bridge verb: the host's names are the whole surface", () => {
    const ctx = createUxContext("acme", { onDispose: (d) => collected.push(d) });
    expect("bridge" in ctx).toBe(false);
    expect(Object.keys(ctx).sort()).toEqual(
      [
        "addEventListener",
        "data",
        "onDispose",
        "onEvent",
        "os",
        "plugin",
        "pluginId",
        "register",
        "registerMany",
        "send",
        "setInterval",
        "setTimeout",
        "shell",
        "source",
        "storage",
      ].sort(),
    );
  });

  it("rejects ctx.plugin for a plugin that ships in no pack", async () => {
    const ctx = createUxContext("loose", { onDispose: (d) => collected.push(d) });
    await expect(ctx.plugin("tool", {})).rejects.toThrow(/needs a pack/);
  });

  it("admits only a snake_case plugin id, as a pack name", () => {
    const plugin = (id: unknown) => ({ id, register() {} });
    for (const ok of ["a", "hello", "focus_timer", "b2", "x".repeat(64)]) {
      expect(isGentsUxPlugin(plugin(ok)), ok).toBe(true);
    }
    for (const bad of [
      "",
      "Hello",
      "a.b",
      "a:b",
      "a/b",
      "a b",
      "-a",
      "_a",
      "1a",
      "x".repeat(65),
      "decisions.v1",
      "../x",
      1,
      null,
    ]) {
      expect(isGentsUxPlugin(plugin(bad)), String(bad)).toBe(false);
    }
  });

  it("refuses a contribution id that could not be a route segment", () => {
    const ctx = createUxContext("acme", { onDispose: (d) => collected.push(d) });
    const data = { group: "g", label: "l", render: () => null };
    for (const bad of ["", "a/b", "a?b", "a#b", "a:b", "a b", "\t"]) {
      expect(
        () => ctx.register({ id: bad, area: AGENT_SECTIONS_AREA, data }),
        JSON.stringify(bad),
      ).toThrow("one segment");
    }
    expect(registry.getArea(AGENT_SECTIONS_AREA)).toHaveLength(0);
    ctx.register({ id: "page-2.v1", area: AGENT_SECTIONS_AREA, data });
    expect(registry.getArea(AGENT_SECTIONS_AREA).map((c) => c.id)).toEqual([
      "acme:page-2.v1",
    ]);
  });

  it("gives a directive name one owner: order and reload cannot move it", () => {
    const first = createUxContext("first", { onDispose: (d) => collected.push(d) });
    const second = createUxContext("second", { onDispose: (d) => collected.push(d) });
    const claim = (ctx: ReturnType<typeof createUxContext>, order?: number) =>
      ctx.register({
        id: "d",
        area: TRANSCRIPT_DIRECTIVE_AREA,
        order,
        data: { name: "dup", render: () => ctx.pluginId },
      });
    const resolved = () =>
      resolveDirective("dup")?.render({ attrs: {}, source: "", streaming: false });

    const release = claim(first);
    expect(() => claim(second, -1)).toThrow(
      'directive "dup" is already claimed by plugin:first',
    );
    expect(resolved()).toBe("first");

    /* a reload of the owner (dispose, register again) keeps it */
    release();
    expect(resolveDirective("dup")).toBeNull();
    const again = claim(first);
    expect(() => claim(second, -1)).toThrow("already claimed");
    expect(resolved()).toBe("first");

    /* once the owner is gone, the next claim takes the name */
    again();
    claim(second);
    expect(resolved()).toBe("second");
    expect(() => claim(first)).toThrow("already claimed by plugin:second");
  });

  it("serves the payload it checked, not the plugin's object afterwards", () => {
    const ctx = createUxContext("acme", {
      onDispose: (d) => collected.push(d),
      declared: { areas: [TRANSCRIPT_DIRECTIVE_AREA], directives: ["ok"] },
    });
    const data = { name: "ok", render: () => "ok" };
    ctx.register({ id: "d", area: TRANSCRIPT_DIRECTIVE_AREA, data });
    expect(() => {
      data.name = "undeclared";
    }).not.toThrow();
    expect(resolveDirective("undeclared")).toBeNull();
    expect(
      resolveDirective("ok")?.render({ attrs: {}, source: "", streaming: false }),
    ).toBe("ok");
    const served = registry.getArea(TRANSCRIPT_DIRECTIVE_AREA)[0]!.data as {
      name: string;
    };
    expect(Object.isFrozen(served)).toBe(true);
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
