/* Every example plugin and the fixture pack's file-produced plugin load
   through the real runtime pipeline: allowlist, a frame of its own (in
   process here, over the real protocol), SDK shims, register(ctx). A
   regression in the SDK's export list or in an example shows up here, not
   in a user's home. */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  loadRuntimeUxPlugin,
  retireRuntimeUxPlugin,
  unsupportedImports,
} from "@/contrib/loader";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import {
  resetUxPluginStore,
  setUxPluginEnabled,
  uxPluginRecords,
} from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import {
  AGENT_SECTIONS_AREA,
  NAV_AREA,
  PLUGIN_PAGE_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
} from "@/contrib/types";
import { ALLOWED_SPECIFIERS, resetSdkImportMap } from "@/sdk/runtime";
import { fireEvent, render } from "@testing-library/react";
import { createElement, Fragment } from "react";

import {
  frameMount,
  installInProcessSandbox,
  settle,
  uninstallInProcessSandbox,
} from "./sandbox-fixture";

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
  plugin: async () => ({ tasks: [] }),
  os: {
    openExternal: async () => true,
    revealPath: async () => true,
    pickDirectory: async () => null,
  },
};

beforeEach(() => {
  installUxHost(host);
  resetUxPluginStore();
  resetSdkImportMap();
  installInProcessSandbox();
});
afterEach(() => {
  for (const id of Object.keys(uxPluginRecords())) retireRuntimeUxPlugin(id);
  uninstallInProcessSandbox();
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

  it("focus_timer ships off, then loads with its stylesheet once enabled", async () => {
    await loadRuntimeUxPlugin(
      {
        source: read(EXAMPLES.focus_timer),
        css: read("examples/ux-plugins/focus_timer/plugin.css"),
      },
      { origin: "focus_timer", door: "dev" },
    );
    expect(uxPluginRecords().focus_timer?.status).toBe("disabled");
    expect(document.querySelector('style[data-gents-ux="focus_timer"]')).toBeNull();
    await setUxPluginEnabled("focus_timer", true);
    expect(uxPluginRecords().focus_timer?.status).toBe("loaded");
    expect(document.querySelector('style[data-gents-ux="focus_timer"]')).not.toBeNull();
    expect(registry.getArea(SESSION_HEADER_ACTIONS_AREA)).toHaveLength(1);
  });

  it("focus_timer settles a block that ran out while the app was closed, without sending", async () => {
    const sent: string[] = [];
    installUxHost({
      ...host,
      shell: {
        ...host.shell,
        send: async (content) => {
          sent.push(content);
        },
      },
    });
    window.localStorage.setItem(
      "gents.ux.focus_timer.state",
      JSON.stringify({ presetId: "deep", endsAt: Date.now() - 60_000, done: 2 }),
    );
    await loadRuntimeUxPlugin(
      { source: read(EXAMPLES.focus_timer) },
      { origin: "focus_timer", door: "dev" },
    );
    await setUxPluginEnabled("focus_timer", true);
    expect(sent).toEqual([]);
    expect(
      JSON.parse(window.localStorage.getItem("gents.ux.focus_timer.state")!),
    ).toEqual({
      presetId: "deep",
      endsAt: null,
      done: 3,
      ended: true,
    });

    /* a value that is not this plugin's shape reads as the default */
    window.localStorage.setItem("gents.ux.focus_timer.state", JSON.stringify([1, 2]));
    await setUxPluginEnabled("focus_timer", false);
    await setUxPluginEnabled("focus_timer", true);
    expect(uxPluginRecords().focus_timer?.status).toBe("loaded");
  });

  it("board ships off, then loads under its declared gate and claims the ::board directive", async () => {
    const id = await loadRuntimeUxPlugin(
      { source: read(EXAMPLES.board) },
      {
        origin: "examples/board_pack/board",
        door: "pack",
        pack: "examples/board_pack",
        declared: {
          areas: [NAV_AREA, PLUGIN_PAGE_AREA, TRANSCRIPT_DIRECTIVE_AREA],
          directives: ["board"],
        },
      },
    );
    expect(id).toBe("board");
    expect(uxPluginRecords().board?.status).toBe("disabled");
    await setUxPluginEnabled("board", true);
    expect(uxPluginRecords().board?.status).toBe("loaded");
    /* the nav row opens the plugin's own page, never the agent's configuration */
    const nav = registry.getArea(NAV_AREA).find((c) => c.id === "board:nav");
    const to = (nav?.data as { to: (ctx: unknown) => unknown }).to;
    expect(to({ mailboxCount: 0, selectedNodeDid: null })).toEqual({
      name: "plugin",
      pluginId: "board",
    });
    expect(registry.getArea(PLUGIN_PAGE_AREA).map((c) => c.id)).toEqual(["board:page"]);
    expect(registry.getArea(AGENT_SECTIONS_AREA)).toEqual([]);
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
    await setUxPluginEnabled("board", true);
    expect(uxPluginRecords().board?.status).toBe("error");
    expect(uxPluginRecords().board?.error).toMatch(/does not declare/);
    expect(registry.getArea(NAV_AREA).some((c) => c.id === "board:nav")).toBe(false);
  });

  it("board reads and writes only through the surface and tools its manifest declares", async () => {
    /* the pack's own declaration is what the host enforces */
    const manifest = JSON.parse(
      read("examples/ux-plugins/board_pack/manifest.json"),
    ) as {
      kind: string;
      ux: Array<{ name: string; data: Array<{ surface: string; tools: string[] }> }>;
    };
    expect(manifest.kind).toBe("assets");
    expect(manifest.ux[0]?.data).toEqual([
      {
        surface: "board",
        tools: ["list_tasks", "list_moves", "add_task", "move_task"],
      },
    ]);
    /* and the documents pack installs exactly those four tools on one surface */
    const docs = JSON.parse(
      read("examples/ux-plugins/board_docs/pack_config.json"),
    ) as {
      datastore_tool_surfaces: Array<{
        surface_id: string;
        entries: Array<{ tool_name: string; kind?: string; fields: unknown }>;
      }>;
    };
    expect(docs.datastore_tool_surfaces.map((s) => s.surface_id)).toEqual(["board"]);
    expect(
      docs.datastore_tool_surfaces[0]!.entries.map((e) => [
        e.tool_name,
        e.kind ?? "create",
      ]),
    ).toEqual([
      ["list_tasks", "query"],
      ["list_moves", "query"],
      ["add_task", "create"],
      ["move_task", "create"],
    ]);

    /* the page: two live queries on mount, a create on Add, both under the
       plugin's listing and the selected node, and the fold of moves over
       tasks the lanes draw */
    const calls: Array<[string, string, string, string, unknown]> = [];
    const tasks = [
      {
        task_id: "t1",
        title: "write the guide",
        lane: "triage",
        created_at: "2026-10-01T00:00:00Z",
      },
      {
        task_id: "t2",
        title: "ship it",
        lane: "triage",
        created_at: "2026-10-02T00:00:00Z",
      },
      {
        task_id: "t3",
        title: "old one",
        lane: "done",
        created_at: "2026-09-01T00:00:00Z",
      },
    ];
    const moves = [
      { task_id: "t2", lane: "running", moved_at: "2026-10-03T00:00:00Z" },
      { task_id: "t2", lane: "ready", moved_at: "2026-10-02T12:00:00Z" },
      { task_id: "t3", lane: "archived", moved_at: "2026-10-04T00:00:00Z" },
    ];
    installUxHost({
      ...host,
      shell: {
        ...host.shell,
        selectedDeployment: () => ({ nodeDid: "did:key:n" }) as never,
      },
      data: {
        call: async (listing, nodeDid, surface, tool, args) => {
          calls.push([listing, nodeDid, surface, tool, args]);
          /* the tool pages: the moves come two at a time with a next_offset,
             and the board must see all three to put the task in its lane */
          if (tool === "list_tasks")
            return { kind: "query", output: { results: tasks, count: 3 } };
          if (tool === "list_moves") {
            const offset = (args.offset as number | undefined) ?? 0;
            const page = moves.slice(offset, offset + 2);
            const output: Record<string, unknown> = {
              results: page,
              count: page.length,
            };
            if (page.length === 2) output.next_offset = offset + 2;
            return { kind: "query", output };
          }
          return {
            kind: "create",
            output: { collection: "BoardTask", document_id: "bae-1", document: args },
          };
        },
        surfaces: async () => [],
      },
    });
    await loadRuntimeUxPlugin(
      { source: read(EXAMPLES.board) },
      {
        origin: "examples/board_pack/board",
        door: "pack",
        pack: "examples/board_pack",
        listing: "examples/board_pack/board",
        declared: {
          areas: [NAV_AREA, PLUGIN_PAGE_AREA, TRANSCRIPT_DIRECTIVE_AREA],
          directives: ["board"],
        },
      },
    );
    await setUxPluginEnabled("board", true);
    const page = registry.getArea(PLUGIN_PAGE_AREA)[0]!;
    const view = render(createElement(Fragment, null, page.render!()));
    await settle();
    const mount = frameMount("examples/board_pack/board")!;
    expect(calls.map((c) => c.slice(0, 4))).toEqual([
      ["examples/board_pack/board", "did:key:n", "board", "list_tasks"],
      ["examples/board_pack/board", "did:key:n", "board", "list_moves"],
      ["examples/board_pack/board", "did:key:n", "board", "list_moves"],
    ]);
    expect(calls.map((c) => c[4])).toEqual([{}, {}, { offset: 2 }]);
    const lane = (name: string) =>
      [...mount.querySelectorAll(".gents-board-lane")]
        .find((el) => el.querySelector("h4")?.textContent?.startsWith(name))!
        .querySelectorAll("[data-task]");
    expect([...lane("triage")].map((el) => el.getAttribute("data-task"))).toEqual([
      "t1",
    ]);
    /* the latest move wins, not the last one listed */
    expect([...lane("running")].map((el) => el.getAttribute("data-task"))).toEqual([
      "t2",
    ]);
    /* an archived task is off the board */
    expect(mount.querySelector('[data-task="t3"]')).toBeNull();

    fireEvent.change(mount.querySelector("input[aria-label='New task']")!, {
      target: { value: "a new one" },
    });
    fireEvent.submit(mount.querySelector("form")!);
    await settle();
    const create = calls.find((c) => c[3] === "add_task")!;
    expect(create[2]).toBe("board");
    expect(create[4]).toMatchObject({ title: "a new one", lane: "triage" });
    expect((create[4] as { task_id: string }).task_id).toMatch(/^[a-z0-9]+$/);
    expect((create[4] as { created_at: string }).created_at).toMatch(
      /^\d{4}-\d{2}-\d{2}T/,
    );
    /* both queries run again after the write */
    expect(calls.filter((c) => c[3] === "list_tasks").length).toBeGreaterThanOrEqual(2);
    view.unmount();
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
