/* The runtime door's lifecycle over a faked bridge: what the inventory
   shows when a dev plugin breaks, is fixed, loses its source, vanishes, or
   is reloaded while the boot scan is still running. The bridge is the only
   thing faked; the loader evaluates the modules for real. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const call = vi.fn();
vi.mock("@/screens/agent/bridgeCall", () => ({
  call: (...args: unknown[]) => call(...args),
  message: (error: unknown) => String(error),
}));

import { isUxPluginLive } from "@/contrib/activate";
import { retireRuntimeUxPlugin } from "@/contrib/loader";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import { resetUxPluginStore, uxPluginRecords } from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import {
  reloadRuntimeUxPlugins,
  resetRuntimeDoor,
  scanRuntimeUxPlugins,
  type UxPluginListing,
} from "@/contrib/runtime-door";
import { SESSION_HEADER_ACTIONS_AREA } from "@/contrib/types";
import { resetSdkImportMap } from "@/sdk/runtime";

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

/* the loader's blob import, rerouted to a data: URL as ux-loader.test does */
const RealBlob = globalThis.Blob;
const realCreate = URL.createObjectURL;
const realRevoke = URL.revokeObjectURL;
class FakeBlob {
  parts: string[];
  constructor(parts: string[]) {
    this.parts = parts;
  }
}

const clock = (id: string): UxPluginListing => ({
  id,
  door: "dev",
  producer: "file",
  file: `/home/ux-plugins/${id}/plugin.js`,
});

const chipPlugin = (id: string) =>
  `import { SESSION_HEADER_ACTIONS_AREA } from '@gents/ux-sdk'
export default { id: '${id}', register(ctx) {
  ctx.register({ id: 'chip', area: SESSION_HEADER_ACTIONS_AREA, render: () => null }) } }`;

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
  vi.stubGlobal("Blob", FakeBlob);
  URL.createObjectURL = (blob: unknown) =>
    `data:text/javascript;base64,${Buffer.from((blob as FakeBlob).parts.join("")).toString("base64")}`;
  URL.revokeObjectURL = () => undefined;
  listings = [];
  sources.clear();
  listGate = Promise.resolve();
  call.mockReset();
  call.mockImplementation(async (command: string, args?: { id?: string }) => {
    if (command === "desktop_ux_plugins_list") {
      await listGate;
      return { plugins: [...listings] };
    }
    if (command === "desktop_ux_plugin_source") {
      const source = sources.get(args?.id ?? "");
      if (source instanceof Error) throw source;
      if (source === undefined) throw new Error(`no source for ${args?.id}`);
      return { source, css: null, digest: "sha256:0" };
    }
    throw new Error(`unexpected bridge command ${command}`);
  });
});
afterEach(() => {
  for (const id of Object.keys(uxPluginRecords())) retireRuntimeUxPlugin(id);
  resetUxPluginStore();
  resetRuntimeDoor();
  resetSdkImportMap();
  vi.stubGlobal("Blob", RealBlob);
  URL.createObjectURL = realCreate;
  URL.revokeObjectURL = realRevoke;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const chips = () => registry.getArea(SESSION_HEADER_ACTIONS_AREA).map((c) => c.id);

describe("the runtime door", () => {
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
