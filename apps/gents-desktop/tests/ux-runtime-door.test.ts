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
  uxDecisions,
  uxPluginRecords,
} from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import {
  configureUxDoors,
  followSelectedUxNode,
  refreshRuntimePackUxPlugins,
  reloadRuntimeUxPlugins,
  resetRuntimeDoor,
  scanRuntimeUxPlugins,
  type UxPluginListing,
} from "@/contrib/runtime-door";
import { SESSION_HEADER_ACTIONS_AREA } from "@/contrib/types";
import { resetSdkImportMap } from "@/sdk/runtime";
import {
  installInProcessSandbox,
  settle,
  uninstallInProcessSandbox,
} from "./sandbox-fixture";

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

it("refreshes only the changed pack and removes its retired pages", async () => {
  const first = packed("first", true);
  const second = {
    ...packed("second", true),
    id: "other/game/second",
    pack: "other/game",
  };
  listings = [first, second];
  sources.set(first.id, chipPlugin("first"));
  sources.set(second.id, chipPlugin("second"));
  await scanRuntimeUxPlugins();
  call.mockClear();

  await refreshRuntimePackUxPlugins("acme/tools");
  expect(reads()).toEqual([["desktop_ux_plugin_source", { id: first.id }]]);
  expect(chips()).toEqual(expect.arrayContaining(["first:chip", "second:chip"]));

  listings = [second];
  call.mockClear();
  await refreshRuntimePackUxPlugins("acme/tools");
  expect(reads()).toHaveLength(0);
  expect(chips()).toEqual(["second:chip"]);
});

it("reloads a pack plugin on a plain scan only when its listing digest changes (a CLI reinstall)", async () => {
  const v1 = { ...packed("first", true), digest: "sha256:a" };
  listings = [v1];
  sources.set(v1.id, chipPlugin("first"));
  await scanRuntimeUxPlugins();
  expect(chips()).toEqual(["first:chip"]);

  call.mockClear();
  await scanRuntimeUxPlugins();
  expect(reads()).toEqual([]);

  listings = [{ ...v1, digest: "sha256:b" }];
  await scanRuntimeUxPlugins();
  expect(reads()).toEqual([["desktop_ux_plugin_source", { id: v1.id }]]);
  expect(chips()).toEqual(["first:chip"]);
});

it("never rereads a pack plugin listed without a digest on a plain scan", async () => {
  const p = packed("first", true);
  listings = [p];
  sources.set(p.id, chipPlugin("first"));
  await scanRuntimeUxPlugins();
  call.mockClear();
  await scanRuntimeUxPlugins();
  expect(reads()).toEqual([]);
});

it("reports an explicit pack refresh failure without retiring the current pages", async () => {
  const entry = packed("widget", true);
  listings = [entry];
  sources.set(entry.id, chipPlugin("widget"));
  await scanRuntimeUxPlugins();
  configureUxDoors({
    list: async () => {
      throw new Error("inventory unavailable");
    },
    source: async () => {
      throw new Error("unexpected read");
    },
  });
  await expect(refreshRuntimePackUxPlugins("acme/tools")).rejects.toThrow(
    "inventory unavailable",
  );
  expect(chips()).toEqual(["widget:chip"]);
});

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

/* a page document on the selected node: the third door */
const pageDoc = (name: string, digest: string, enabled = true): UxPluginListing => ({
  id: `page/${name}`,
  door: "document",
  producer: "file",
  file: `Page/${name}`,
  description: `${name} on this node`,
  contributes: { areas: [SESSION_HEADER_ACTIONS_AREA], directives: [] },
  defaultEnabled: enabled,
  digest,
});

describe("page documents", () => {
  it("loads an enabled page and reloads it only when its digest changes", async () => {
    const page = pageDoc("notes", "sha256:a");
    listings = [page];
    sources.set(page.id, chipPlugin("notes"));
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords().notes).toMatchObject({
      door: "document",
      status: "loaded",
    });
    expect(chips()).toEqual(["notes:chip"]);

    call.mockClear();
    await scanRuntimeUxPlugins();
    expect(reads()).toEqual([]);

    listings = [pageDoc("notes", "sha256:b")];
    await scanRuntimeUxPlugins();
    expect(reads()).toEqual([["desktop_ux_plugin_source", { id: page.id }]]);
    expect(isUxPluginLive("notes")).toBe(true);
  });

  it("unloads a page when its document is disabled and loads it again when enabled", async () => {
    const page = pageDoc("notes", "sha256:a");
    listings = [page];
    sources.set(page.id, chipPlugin("notes"));
    await scanRuntimeUxPlugins();
    expect(isUxPluginLive("notes")).toBe(true);

    listings = [pageDoc("notes", "sha256:a", false)];
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords().notes).toMatchObject({ status: "disabled" });
    expect(chips()).toEqual([]);

    /* this window's own earlier choice never overrides a disabled document */
    await setUxPluginEnabled("notes", true);
    expect(isUxPluginLive("notes")).toBe(false);
    await scanRuntimeUxPlugins();
    expect(isUxPluginLive("notes")).toBe(false);

    listings = [pageDoc("notes", "sha256:a")];
    await scanRuntimeUxPlugins();
    expect(isUxPluginLive("notes")).toBe(true);
  });

  it("retires a page whose document left the selected node", async () => {
    const page = pageDoc("notes", "sha256:a");
    listings = [page];
    sources.set(page.id, chipPlugin("notes"));
    await scanRuntimeUxPlugins();
    listings = [];
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords().notes).toBeUndefined();
    expect(chips()).toEqual([]);
  });
});

/* Page documents are per node: two nodes may each hold a page of one id,
   and the window shows the selected node's. The bridge here answers for the
   node it is asked about, as the real one does. */
describe("page documents across nodes", () => {
  interface NodePages {
    pages: UxPluginListing[];
    modules: Record<string, string>;
    error?: string | null;
  }
  let selected: string | null = null;
  let nodes: Record<string, NodePages> = {};
  let sourceGate: Promise<void> = Promise.resolve();
  const sourceReads = () =>
    reads().map(([, args]) => args as { id: string; node: string | null });
  const labelled = (label: string) =>
    `import { SESSION_HEADER_ACTIONS_AREA } from '@gents/ux-sdk'
export default { id: 'notes', register(ctx) {
  ctx.register({ id: '${label}', area: SESSION_HEADER_ACTIONS_AREA, data: { label: '${label}' } }) } }`;
  const holding = (label: string, digest: string): NodePages => ({
    pages: [pageDoc("notes", digest)],
    modules: { "page/notes": labelled(label) },
  });

  beforeEach(() => {
    selected = null;
    nodes = {};
    sourceGate = Promise.resolve();
    configureUxDoors({
      node: () => selected,
      list: async (node) => {
        call("desktop_ux_plugins_list", { node });
        await listGate;
        const held = nodes[node ?? ""];
        return {
          plugins: [...listings, ...(held?.error ? [] : (held?.pages ?? []))],
          pagesError: held?.error ?? null,
        };
      },
      source: async (id, node) => {
        call("desktop_ux_plugin_source", { id, node });
        await sourceGate;
        const source = nodes[node ?? ""]?.modules[id];
        if (source === undefined) throw new Error(`no source for ${id} on ${node}`);
        return { source, css: null, digest: "sha256:0" };
      },
    });
  });

  it("reads a page's module from the node that listed it, and drops it once that node is left", async () => {
    nodes = {
      "did:x": holding("from-x", "sha256:1"),
      "did:y": holding("from-y", "sha256:1"),
    };
    selected = "did:x";
    let open = () => {};
    sourceGate = new Promise((resolve) => (open = resolve));
    const first = scanRuntimeUxPlugins();
    await vi.waitFor(() => expect(sourceReads()).toHaveLength(1));
    /* the selection moves while x's module is being read */
    selected = "did:y";
    const followed = followSelectedUxNode();
    open();
    await Promise.all([first, followed]);
    expect(sourceReads()).toEqual([
      { id: "page/notes", node: "did:x" },
      { id: "page/notes", node: "did:y" },
    ]);
    expect(chips()).toEqual(["notes:from-y"]);
  });

  it("does not take an unchanged digest for the page of a node the selection left and came back to", async () => {
    nodes = {
      "did:x": holding("from-x", "sha256:1"),
      "did:y": holding("from-y", "sha256:1"),
    };
    selected = "did:x";
    let open = () => {};
    sourceGate = new Promise((resolve) => (open = resolve));
    const first = scanRuntimeUxPlugins();
    await vi.waitFor(() => expect(sourceReads()).toHaveLength(1));
    selected = "did:y";
    const away = followSelectedUxNode();
    selected = "did:x";
    const back = followSelectedUxNode();
    open();
    await Promise.all([first, away, back]);
    await scanRuntimeUxPlugins();
    /* never y's module under x's listing, and x's page is running */
    expect(sourceReads().every((read) => read.node === "did:x")).toBe(true);
    expect(chips()).toEqual(["notes:from-x"]);
    expect(uxPluginRecords().notes).toMatchObject({ status: "loaded" });
  });

  it("retires the left node's pages when the selection moves, before the next node is scanned", async () => {
    nodes = {
      "did:a": holding("from-a", "sha256:1"),
      "did:b": holding("from-b", "sha256:2"),
    };
    selected = "did:a";
    await scanRuntimeUxPlugins();
    expect(isUxPluginLive("notes")).toBe(true);

    let open = () => {};
    listGate = new Promise((resolve) => (open = resolve));
    selected = "did:b";
    const followed = followSelectedUxNode();
    /* b has not answered yet; a's frame is already gone */
    expect(isUxPluginLive("notes")).toBe(false);
    expect(uxPluginRecords().notes).toBeUndefined();
    expect(chips()).toEqual([]);
    open();
    await followed;
    expect(chips()).toEqual(["notes:from-b"]);
  });

  it("keeps a page's choice and storage per node, apart from a pack plugin of the same name", async () => {
    const remembering = (mark: string) =>
      `import { SESSION_HEADER_ACTIONS_AREA } from '@gents/ux-sdk'
export default { id: 'notes', register(ctx) {
  ctx.register({ id: 'read-' + ctx.storage.get('seen', 'nothing'), area: SESSION_HEADER_ACTIONS_AREA, data: { label: 'chip' } })
  ctx.storage.set('seen', '${mark}') } }`;
    nodes = {
      "did:a": {
        pages: [pageDoc("notes", "sha256:1")],
        modules: { "page/notes": remembering("a") },
      },
      "did:b": {
        pages: [pageDoc("notes", "sha256:1")],
        modules: { "page/notes": remembering("b") },
      },
    };
    /* what a pack plugin named `notes` keeps, under the plain id */
    window.localStorage.setItem("gents.ux.notes.seen", JSON.stringify("pack"));

    selected = "did:a";
    await scanRuntimeUxPlugins();
    await settle();
    expect(chips()).toEqual(["notes:read-nothing"]);
    selected = "did:b";
    await followSelectedUxNode();
    await settle();
    expect(chips()).toEqual(["notes:read-nothing"]);
    expect(window.localStorage.getItem("gents.ux.did:a/page/notes.seen")).toBe('"a"');
    expect(window.localStorage.getItem("gents.ux.did:b/page/notes.seen")).toBe('"b"');
    expect(window.localStorage.getItem("gents.ux.notes.seen")).toBe('"pack"');

    /* switched off on b: a's page of the same id is not */
    await setUxPluginEnabled("notes", false);
    expect(uxDecisions()).toEqual({ "did:b/page/notes": false });
    selected = "did:a";
    await followSelectedUxNode();
    await settle();
    expect(uxPluginRecords().notes).toMatchObject({ status: "loaded" });
    expect(chips()).toEqual(["notes:read-a"]);
    selected = "did:b";
    await followSelectedUxNode();
    expect(uxPluginRecords().notes).toMatchObject({ status: "disabled" });
    expect(
      reads().filter(([, args]) => (args as { node: string }).node === "did:b"),
    ).toHaveLength(1);
  });

  it("keeps a pack plugin's choice under its own id", async () => {
    listings = [packed("clock", true)];
    sources.set("acme/tools/clock", chipPlugin("clock"));
    nodes = {
      "did:a": { pages: [], modules: { "acme/tools/clock": chipPlugin("clock") } },
    };
    selected = "did:a";
    await scanRuntimeUxPlugins();
    await setUxPluginEnabled("clock", false);
    expect(uxDecisions()).toEqual({ clock: false });
  });

  it("keeps the pages already loaded when the node's pages cannot be read, and says so on a row", async () => {
    nodes = { "did:a": holding("from-a", "sha256:1") };
    selected = "did:a";
    await scanRuntimeUxPlugins();
    expect(chips()).toEqual(["notes:from-a"]);

    nodes["did:a"]!.error = "node did:a is unreachable";
    await scanRuntimeUxPlugins();
    expect(isUxPluginLive("notes")).toBe(true);
    expect(chips()).toEqual(["notes:from-a"]);
    expect(uxPluginRecords()["page/"]).toMatchObject({
      door: "document",
      status: "error",
      error: "node did:a is unreachable",
    });
    /* a Reload is no different: nothing says the page is gone */
    await reloadRuntimeUxPlugins();
    expect(isUxPluginLive("notes")).toBe(true);

    nodes["did:a"]!.error = null;
    await scanRuntimeUxPlugins();
    expect(uxPluginRecords()["page/"]).toBeUndefined();
    expect(isUxPluginLive("notes")).toBe(true);
  });
});
