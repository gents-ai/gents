/* The runtime door's lifecycle over a faked bridge: what the inventory
   shows when a dev plugin breaks, is fixed, loses its source, vanishes, or
   is reloaded while the boot scan is still running. The bridge is the only
   thing faked; the loader evaluates the modules for real. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/* the two door verbs, as the app configures them from its client adapter */
const call = vi.fn();

import { isUxPluginLive } from "@/contrib/activate";
import { retireRuntimeUxPlugin } from "@/contrib/loader";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import {
  resetUxPluginStore,
  setUxPluginEnabled,
  uxPluginRecords,
} from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import {
  configureUxDoors,
  reloadRuntimeUxPlugins,
  resetRuntimeDoor,
  scanRuntimeUxPlugins,
  type UxPluginListing,
} from "@/contrib/runtime-door";
import { SESSION_HEADER_ACTIONS_AREA } from "@/contrib/types";
import { resetSdkImportMap } from "@/sdk/runtime";
import { installInProcessSandbox, uninstallInProcessSandbox } from "./sandbox-fixture";

const host: UxHost = {
  shell: {
    route: () => ({ name: "sessions" }),
    navigate: () => {},
    deployments: () => [],
    selectedDeployment: () => null,
    selectedSessionId: () => null,
    send: async () => {},
  },
  os: {
    openExternal: async () => true,
    revealPath: async () => true,
    pickDirectory: async () => null,
  },
};

const clock = (id: string): UxPluginListing => ({
  id,
  door: "dev",
  producer: "file",
  file: `/home/ux-plugins/${id}/plugin.js`,
});

const chipPlugin = (id: string) =>
  `import { SESSION_HEADER_ACTIONS_AREA } from '@gents/ux-sdk'
export default { id: '${id}', register(ctx) {
  ctx.register({ id: 'chip', area: SESSION_HEADER_ACTIONS_AREA, data: { label: 'chip' } }) } }`;

/* the faked bridge: a listing and a source per id, each replaceable */
let listings: UxPluginListing[] = [];
const sources = new Map<string, string | Error>();
let listGate: Promise<void> = Promise.resolve();

beforeEach(() => {
  installUxHost(host);
  resetUxPluginStore();
  resetRuntimeDoor();
  resetSdkImportMap();
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});
  installInProcessSandbox();
  listings = [];
  sources.clear();
  listGate = Promise.resolve();
  call.mockReset();
  configureUxDoors({
    list: async () => {
      call("desktop_ux_plugins_list");
      await listGate;
      return { plugins: [...listings] };
    },
    source: async (id: string) => {
      call("desktop_ux_plugin_source", { id });
      const source = sources.get(id);
      if (source instanceof Error) throw source;
      if (source === undefined) throw new Error(`no source for ${id}`);
      return { source, css: null, digest: "sha256:0" };
    },
  });
});
afterEach(() => {
  for (const id of Object.keys(uxPluginRecords())) retireRuntimeUxPlugin(id);
  resetUxPluginStore();
  resetRuntimeDoor();
  resetSdkImportMap();
  uninstallInProcessSandbox();
  vi.restoreAllMocks();
});

const chips = () => registry.getArea(SESSION_HEADER_ACTIONS_AREA).map((c) => c.id);

const packed = (name: string, defaultEnabled: boolean | null): UxPluginListing => ({
  id: `acme/tools/${name}`,
  door: "pack",
  producer: "afb",
  file: `/home/packs/acme/tools/plugins/${name}_ui.afb`,
  pack: "acme/tools",
  description: `${name} from a pack`,
  contributes: { areas: [SESSION_HEADER_ACTIONS_AREA], directives: [] },
  defaultEnabled,
});
const reads = () =>
  call.mock.calls.filter(([command]) => command === "desktop_ux_plugin_source");

describe("discovery runs nothing that is off", () => {
  it.each([null, true, false])(
    "requires opt-in for development code despite manifest default %s",
    async (defaultEnabled) => {
      listings = [{ ...clock("clock"), defaultEnabled }];
      sources.set("clock", chipPlugin("clock"));
      await scanRuntimeUxPlugins();
      await reloadRuntimeUxPlugins();
      expect(reads()).toHaveLength(0);
      expect(uxPluginRecords().clock?.status).toBe("disabled");
      await setUxPluginEnabled("clock", true);
      expect(reads()).toHaveLength(1);
      expect(chips()).toEqual(["clock:chip"]);
      await setUxPluginEnabled("clock", false);
      await reloadRuntimeUxPlugins();
      expect(reads()).toHaveLength(1);
      expect(chips()).toEqual([]);
    },
  );
  it("lists a pack plugin that ships off from its manifest alone, and loads it on the switch", async () => {
    listings = [packed("widget", false)];
    sources.set("acme/tools/widget", chipPlugin("widget"));
    await scanRuntimeUxPlugins();
    /* the row is there, built from the listing: producer not run, module not read */
    expect(reads()).toHaveLength(0);
    expect(uxPluginRecords().widget).toMatchObject({
      status: "disabled",
      door: "pack",
      producer: "afb",
      pack: "acme/tools",
      description: "widget from a pack",
    });
    expect(chips()).toEqual([]);
    /* a Reload re-reads the listing, still without the module */
    await reloadRuntimeUxPlugins();
    expect(reads()).toHaveLength(0);

    await setUxPluginEnabled("widget", true);
    expect(reads()).toHaveLength(1);
    expect(uxPluginRecords().widget?.status).toBe("loaded");
    expect(chips()).toEqual(["widget:chip"]);

    /* off again: registrations go; the module is not read again to turn it off */
    await setUxPluginEnabled("widget", false);
    expect(uxPluginRecords().widget?.status).toBe("disabled");
    expect(chips()).toEqual([]);
    expect(reads()).toHaveLength(1);
  });

  it("honours a saved decision before any source is read, for a dev plugin too", async () => {
    await setUxPluginEnabled("clock", false);
    listings = [clock("clock")];
    sources.set("clock", chipPlugin("clock"));
    await scanRuntimeUxPlugins();
    expect(reads()).toHaveLength(0);
    expect(uxPluginRecords().clock?.status).toBe("disabled");
    await setUxPluginEnabled("clock", true);
    expect(reads()).toHaveLength(1);
    expect(chips()).toEqual(["clock:chip"]);
  });

  it("refuses a module whose id is not the one its listing declares", async () => {
    listings = [packed("widget", true)];
    sources.set("acme/tools/widget", chipPlugin("gadget"));
    await scanRuntimeUxPlugins();
    const row = uxPluginRecords()["acme/tools/widget"];
    expect(row?.status).toBe("error");
    expect(row?.error).toContain('id "gadget"');
    expect(row?.error).toContain('declares "widget"');
    expect(chips()).toEqual([]);
  });
});

describe("the runtime door", () => {
  beforeEach(async () => {
    await setUxPluginEnabled("clock", true);
  });
  it("keeps a broken dev plugin on its row, registrations gone, and loads it again once fixed", async () => {
    listings = [clock("clock")];
    sources.set("clock", chipPlugin("clock"));
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords().clock?.status).toBe("loaded");
    expect(chips()).toEqual(["clock:chip"]);

    /* a save that no longer evaluates: the id is the folder's own, so the
       error row and the old incarnation share it */
    sources.set("clock", `throw new Error("boom at line 1")`);
    await reloadRuntimeUxPlugins();
    expect(uxPluginRecords().clock?.status).toBe("error");
    expect(uxPluginRecords().clock?.error).toContain("boom at line 1");
    expect(isUxPluginLive("clock")).toBe(false);
    expect(chips()).toEqual([]);

    /* fixed: a plain scan, not a Reload, picks a broken entry up again */
    sources.set("clock", chipPlugin("clock"));
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords().clock?.status).toBe("loaded");
    expect(chips()).toEqual(["clock:chip"]);
  });

  it("retires the old incarnation when the source can no longer be read, on one error row", async () => {
    listings = [clock("clock")];
    sources.set("clock", chipPlugin("clock"));
    await scanRuntimeUxPlugins();
    expect(chips()).toEqual(["clock:chip"]);

    sources.set("clock", new Error("permission denied"));
    await reloadRuntimeUxPlugins();
    const rows = Object.values(uxPluginRecords());
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({
      id: "clock",
      status: "error",
      error: "permission denied",
    });
    expect(chips()).toEqual([]);
    /* the error row is a row, not a module the loader evaluated */
    expect(call).not.toHaveBeenCalledWith(
      "desktop_ux_plugin_source",
      expect.objectContaining({ id: expect.stringContaining("throw") }),
    );
  });

  it("retires an entry that left the disk", async () => {
    listings = [clock("clock")];
    sources.set("clock", chipPlugin("clock"));
    await scanRuntimeUxPlugins();
    listings = [];
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords()).toEqual({});
    expect(chips()).toEqual([]);
  });

  it("runs a Reload asked for during the boot scan after it, never dropping it", async () => {
    listings = [clock("clock")];
    sources.set("clock", chipPlugin("clock"));
    let open!: () => void;
    listGate = new Promise<void>((resolve) => {
      open = resolve;
    });
    const boot = scanRuntimeUxPlugins();
    const reload = reloadRuntimeUxPlugins();
    open();
    await boot;
    await reload;
    const reads = call.mock.calls.filter(
      ([command]) => command === "desktop_ux_plugin_source",
    );
    expect(reads).toHaveLength(2);
    expect(uxPluginRecords().clock?.status).toBe("loaded");
    expect(chips()).toEqual(["clock:chip"]);
  });
});

/* two packs shipping a plugin of one name: the first listing owns the id,
   whatever its state; the other is shadowed, and leaving takes only your
   own incarnation */
describe("one owner per plugin id", () => {
  const inPack = (
    pack: string,
    name: string,
    defaultEnabled: boolean | null,
  ): UxPluginListing => ({
    id: `${pack}/${name}`,
    door: "pack",
    producer: "file",
    file: `/home/packs/${pack}/ux/${name}/plugin.js`,
    pack,
    contributes: { areas: [SESSION_HEADER_ACTIONS_AREA], directives: [] },
    defaultEnabled,
  });

  it("a disabled same-name listing from another pack does not overwrite the live one", async () => {
    listings = [
      inPack("acme/first", "widget", true),
      inPack("zeta/second", "widget", false),
    ];
    sources.set("acme/first/widget", chipPlugin("widget"));
    sources.set("zeta/second/widget", chipPlugin("widget"));
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords().widget).toMatchObject({
      status: "loaded",
      pack: "acme/first",
    });
    expect(uxPluginRecords()["zeta/second/widget:shadowed"]).toMatchObject({
      status: "disabled",
      pack: "zeta/second",
      description: expect.stringContaining("owned by acme/first/widget"),
    });
    expect(chips()).toEqual(["widget:chip"]);
    expect(isUxPluginLive("widget")).toBe(true);
    /* the switch on the shadowed row loads nothing */
    await setUxPluginEnabled("zeta/second/widget:shadowed", true);
    expect(uxPluginRecords()["zeta/second/widget:shadowed"]?.status).toBe("error");
    expect(reads().map(([, args]) => (args as { id: string }).id)).toEqual([
      "acme/first/widget",
    ]);
    expect(chips()).toEqual(["widget:chip"]);
  });

  it("removing the shadowed pack leaves the owner running; removing the owner lets the other claim", async () => {
    listings = [
      inPack("acme/first", "widget", true),
      inPack("zeta/second", "widget", false),
    ];
    sources.set("acme/first/widget", chipPlugin("widget"));
    sources.set("zeta/second/widget", chipPlugin("widget"));
    await scanRuntimeUxPlugins();

    listings = [inPack("acme/first", "widget", true)];
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords().widget).toMatchObject({
      status: "loaded",
      pack: "acme/first",
    });
    expect(uxPluginRecords()["zeta/second/widget:shadowed"]).toBeUndefined();
    expect(chips()).toEqual(["widget:chip"]);

    listings = [inPack("zeta/second", "widget", false)];
    await scanRuntimeUxPlugins();
    expect(chips()).toEqual([]);
    expect(uxPluginRecords().widget).toMatchObject({
      status: "disabled",
      pack: "zeta/second",
    });
    expect(uxPluginRecords()["zeta/second/widget:shadowed"]).toBeUndefined();
    await setUxPluginEnabled("widget", true);
    expect(uxPluginRecords().widget).toMatchObject({
      status: "loaded",
      pack: "zeta/second",
    });
    expect(chips()).toEqual(["widget:chip"]);
  });

  it("shadows the later listing whatever its state, and a disabled owner keeps its id", async () => {
    listings = [
      inPack("acme/first", "widget", false),
      inPack("zeta/second", "widget", true),
    ];
    sources.set("acme/first/widget", chipPlugin("widget"));
    sources.set("zeta/second/widget", chipPlugin("widget"));
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords().widget).toMatchObject({
      status: "disabled",
      pack: "acme/first",
    });
    expect(uxPluginRecords()["zeta/second/widget:shadowed"]?.status).toBe("disabled");
    expect(reads()).toHaveLength(0);
    expect(chips()).toEqual([]);
    /* a Reload keeps the same owner */
    await reloadRuntimeUxPlugins();
    expect(uxPluginRecords().widget).toMatchObject({
      status: "disabled",
      pack: "acme/first",
    });
    expect(uxPluginRecords()["zeta/second/widget:shadowed"]?.status).toBe("disabled");
  });
});
