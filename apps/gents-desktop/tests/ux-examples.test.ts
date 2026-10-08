/* Every example plugin and the fixture pack's file-produced plugin load
   through the real runtime pipeline: allowlist, SDK shims, blob import,
   register(ctx). A regression in the SDK's export list or in an example
   shows up here, not in a user's home. */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  loadRuntimeUxPlugin,
  retireRuntimeUxPlugin,
  unsupportedImports,
} from "@/contrib/loader";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import { resetUxPluginStore, uxPluginRecords } from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import {
  AGENT_SECTIONS_AREA,
  NAV_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
} from "@/contrib/types";
import { ALLOWED_SPECIFIERS, resetSdkImportMap } from "@/sdk/runtime";

const repo = (rel: string) => join(__dirname, "../../..", rel);
const read = (rel: string) => readFileSync(repo(rel), "utf8");

const EXAMPLES = {
  hello_runtime: "examples/ux-plugins/hello_runtime/plugin.js",
  focus_timer: "examples/ux-plugins/focus_timer/plugin.js",
  board: "examples/ux-plugins/board_pack/ux/board/plugin.js",
  badge: "crates/gents/tests/fixtures/packs/ux_fixture/ux/badge/plugin.js",
};

const host: UxHost = {
  shell: {
    route: () => ({ name: "sessions" }),
    navigate: () => {},
    deployments: () => [],
    selectedDeployment: () => null,
    selectedSessionId: () => null,
    send: async () => {},
  },
  bridge: async () => ({}) as never,
  plugin: async () => ({ tasks: [] }),
  os: {
    openExternal: async () => true,
    revealPath: async () => true,
    pickDirectory: async () => null,
  },
};

const RealBlob = globalThis.Blob;
class FakeBlob {
  parts: string[];
  constructor(parts: string[]) {
    this.parts = parts;
  }
}

beforeEach(() => {
  installUxHost(host);
  resetUxPluginStore();
  resetSdkImportMap();
  vi.stubGlobal("Blob", FakeBlob);
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => store.get(key) ?? null,
    setItem: (key: string, value: string) => store.set(key, value),
    removeItem: (key: string) => store.delete(key),
  });
  URL.createObjectURL = (blob: unknown) =>
    `data:text/javascript;base64,${Buffer.from((blob as FakeBlob).parts.join("")).toString("base64")}`;
  URL.revokeObjectURL = () => undefined;
});
afterEach(() => {
  for (const id of Object.keys(uxPluginRecords())) retireRuntimeUxPlugin(id);
  vi.stubGlobal("Blob", RealBlob);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("example plugins", () => {
  it("import only the allowlisted specifiers", () => {
    for (const [name, rel] of Object.entries(EXAMPLES)) {
      expect(unsupportedImports(read(rel), ALLOWED_SPECIFIERS), name).toEqual([]);
    }
  });

  it("hello_runtime loads and lands one header chip", async () => {
    const id = await loadRuntimeUxPlugin(
      { source: read(EXAMPLES.hello_runtime) },
      { origin: "hello_runtime", door: "dev" },
    );
    expect(id).toBe("hello_runtime");
    expect(uxPluginRecords().hello_runtime?.status).toBe("loaded");
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA).map((c) => c.id)).toEqual([
      "hello_runtime:chip",
    ]);
  });

  it("focus_timer loads with its stylesheet and persists through its storage namespace", async () => {
    await loadRuntimeUxPlugin(
      {
        source: read(EXAMPLES.focus_timer),
        css: read("examples/ux-plugins/focus_timer/plugin.css"),
      },
      { origin: "focus_timer", door: "dev" },
    );
    expect(uxPluginRecords().focus_timer?.status).toBe("loaded");
    expect(document.querySelector('style[data-gents-ux="focus_timer"]')).not.toBeNull();
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA)).toHaveLength(1);
  });

  it("board loads under its declared gate and claims the ::board directive", async () => {
    const id = await loadRuntimeUxPlugin(
      { source: read(EXAMPLES.board) },
      {
        origin: "examples/board_pack/board",
        door: "pack",
        pack: "examples/board_pack",
        declared: {
          areas: [NAV_AREA, AGENT_SECTIONS_AREA, TRANSCRIPT_DIRECTIVE_AREA],
          directives: ["board"],
        },
      },
    );
    expect(id).toBe("board");
    expect(uxPluginRecords().board?.status).toBe("loaded");
    expect(registry.getArea(NAV_AREA).some((c) => c.id === "board:nav")).toBe(true);
    expect(registry.getArea(AGENT_SECTIONS_AREA).map((c) => c.id)).toEqual([
      "board:page",
    ]);
    const directive = registry
      .getArea(TRANSCRIPT_DIRECTIVE_AREA)
      .find((c) => c.id === "board:directive");
    expect((directive?.data as { name: string }).name).toBe("board");
  });

  it("board is refused when its pack declares less than it registers", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    await loadRuntimeUxPlugin(
      { source: read(EXAMPLES.board) },
      {
        origin: "examples/board_pack/board",
        door: "pack",
        pack: "examples/board_pack",
        declared: { areas: [NAV_AREA], directives: [] },
      },
    );
    expect(uxPluginRecords().board?.status).toBe("error");
    expect(uxPluginRecords().board?.error).toMatch(/does not declare/);
    expect(registry.getArea(NAV_AREA).some((c) => c.id === "board:nav")).toBe(false);
  });

  it("the fixture pack's badge loads from its file form", async () => {
    const id = await loadRuntimeUxPlugin(
      { source: read(EXAMPLES.badge) },
      {
        origin: "fixture/ux_fixture/badge",
        door: "pack",
        pack: "fixture/ux_fixture",
        declared: { areas: [SESSION_HEADER_ACTIONS_AREA], directives: [] },
      },
    );
    expect(id).toBe("badge");
    expect(uxPluginRecords().badge?.status).toBe("loaded");
  });
});
