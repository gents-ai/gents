/* A plugin behind its frame, end to end over the in-process transport: what
   it registers lands in the shell's registry as data the shell draws, its
   pages draw in the frame, every verb it calls is answered under its own
   context, and a deactivation takes the frame with it. */
import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { isUxPluginLive } from "@/contrib/activate";
import {
  loadRuntimeUxPlugin,
  retireRuntimeUxPlugin,
  sandboxHostOf,
} from "@/contrib/loader";
import { navItems, navRowState } from "@/app/navRegistry";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import {
  resetUxPluginStore,
  setUxPluginEnabled,
  uxPluginRecords,
} from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import { Slot } from "@/contrib/react/slot";
import {
  AGENT_SECTIONS_AREA,
  NAV_AREA,
  PLUGIN_PAGE_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
  type TranscriptDirectiveContribution,
} from "@/contrib/types";
import {
  frameMount,
  frames,
  installInProcessSandbox,
  settle,
  uninstallInProcessSandbox,
} from "./sandbox-fixture";

const calls: Array<{ verb: string; args: unknown[] }> = [];
const host: UxHost = {
  shell: {
    route: () => ({ name: "sessions" }),
    navigate: (route) => void calls.push({ verb: "navigate", args: [route] }),
    deployments: () => [{ nodeDid: "did:key:a" } as never],
    selectedDeployment: () => ({ nodeDid: "did:key:a" }) as never,
    selectedSessionId: () => "s1",
    send: async (content) => void calls.push({ verb: "send", args: [content] }),
  },
  plugin: async (listing, name, input) => {
    calls.push({ verb: "plugin", args: [listing, name, input] });
    return { echoed: input };
  },
  data: {
    call: async (listing, nodeDid, surface, tool, args) => {
      calls.push({ verb: "data.call", args: [listing, nodeDid, surface, tool, args] });
      return { kind: "query", output: { rows: [{ title: "a task" }] } };
    },
    surfaces: async (listing, nodeDid) => {
      calls.push({ verb: "data.surfaces", args: [listing, nodeDid] });
      return [];
    },
  },
  os: {
    openExternal: async () => true,
    revealPath: async () => true,
    pickDirectory: async () => "/picked",
  },
};

const plugin = `
import { AGENT_SECTIONS_AREA, Button, NAV_AREA, PLUGIN_PAGE_AREA, SESSION_HEADER_ACTIONS_AREA, TRANSCRIPT_DIRECTIVE_AREA, toast } from '@gents/ux-sdk'
import { useState } from 'react'
import { jsx, jsxs } from 'react/jsx-runtime'

function Page({ ctx }) {
  const [draft, setDraft] = useState('')
  const [rows, setRows] = useState(null)
  return jsxs('div', { children: [
    jsx('input', { 'data-testid': 'draft', value: draft, onChange: (e) => setDraft(e.target.value) }),
    jsx('button', { 'data-testid': 'load', onClick: () => ctx.data.call('board', 'list_tasks', { limit: 5 }).then((r) => setRows(r.output.rows)) }),
    jsx('button', { 'data-testid': 'call', onClick: () => ctx.plugin('store', { action: 'list' }) }),
    jsx('button', { 'data-testid': 'toast', onClick: () => toast.success('saved') }),
    jsx('button', { 'data-testid': 'remember', onClick: () => ctx.storage.set('seen', 3) }),
    jsx('p', { 'data-testid': 'rows', children: rows ? rows.map((r) => r.title).join(',') : 'none' }),
  ] })
}

function Settings({ nodeDid }) {
  return jsx('p', { 'data-testid': 'settings', children: 'settings for ' + nodeDid })
}

export default {
  id: 'board',
  name: 'Board',
  register(ctx) {
    let clicks = 0
    ctx.registerMany([
      { id: 'nav', area: NAV_AREA, data: { id: 'board:nav', label: 'Board', icon: 'kanban', to: { name: 'plugin', pluginId: 'board' }, placement: 'footer', order: 7 } },
      { id: 'page', area: PLUGIN_PAGE_AREA, render: () => jsx(Page, { ctx }) },
      { id: 'settings', area: AGENT_SECTIONS_AREA, data: { group: 'Packs', label: 'Board', icon: 'kanban', render: (props) => jsx(Settings, props) } },
      { id: 'header', area: SESSION_HEADER_ACTIONS_AREA, data: { label: 'Open board', icon: 'kanban', onClick: () => { clicks += 1; ctx.shell.navigate({ name: 'plugin', pluginId: 'board' }) } } },
      { id: 'chip', area: TRANSCRIPT_DIRECTIVE_AREA, data: { name: 'board', label: 'Board · task #{id}', onClick: (attrs) => ctx.storage.set('highlight', attrs.id) } },
    ])
  },
}
`;

const load = (source = plugin, extra: Record<string, unknown> = {}) =>
  loadRuntimeUxPlugin(
    { source },
    {
      origin: "acme/kit/board",
      expectId: "board",
      door: "pack",
      file: "/packs/acme/kit/ux/board/plugin.js",
      listing: "acme/kit/board",
      declared: {
        areas: [
          NAV_AREA,
          PLUGIN_PAGE_AREA,
          AGENT_SECTIONS_AREA,
          SESSION_HEADER_ACTIONS_AREA,
          TRANSCRIPT_DIRECTIVE_AREA,
        ],
        directives: ["board"],
      },
      ...extra,
    },
  );

beforeEach(() => {
  installUxHost(host);
  resetUxPluginStore();
  installInProcessSandbox();
  calls.length = 0;
  vi.spyOn(console, "error").mockImplementation(() => {});
});
afterEach(() => {
  for (const id of Object.keys(uxPluginRecords())) retireRuntimeUxPlugin(id);
  uninstallInProcessSandbox();
  vi.restoreAllMocks();
});

describe("a plugin in its frame", () => {
  it("registers shell-drawn rows as data, and the shell draws them", async () => {
    expect(await load()).toBe("board");
    await settle();
    expect(uxPluginRecords().board?.status).toBe("loaded");
    expect(isUxPluginLive("board")).toBe(true);

    /* the nav row: a route and an icon name became a row the rail reads */
    const row = navItems().find((i) => i.id === "board:nav");
    expect(row?.label).toBe("Board");
    const state = navRowState(
      row!,
      { name: "plugin", pluginId: "board" },
      {
        mailboxCount: 0,
        selectedNodeDid: "did:key:a",
      },
    );
    expect(state.to).toEqual({ name: "plugin", pluginId: "board" });
    expect(state.active).toBe(true);

    /* the header action: a button the shell draws; the click runs in the frame */
    render(<Slot area={SESSION_HEADER_ACTIONS_AREA} />);
    fireEvent.click(screen.getByTestId("ux-header-action"));
    await settle();
    expect(calls).toContainEqual({
      verb: "navigate",
      args: [{ name: "plugin", pluginId: "board" }],
    });

    /* the directive chip: a label template over the model's attributes */
    const chip = registry.getArea(TRANSCRIPT_DIRECTIVE_AREA)[0]!
      .data as TranscriptDirectiveContribution;
    expect(chip.name).toBe("board");
    render(
      <>
        {chip.render({ attrs: { id: "3" }, source: "::board{id=3}", streaming: false })}
      </>,
    );
    expect(screen.getByTestId("ux-directive-chip")).toHaveTextContent(
      "Board · task #3",
    );
    fireEvent.click(screen.getByTestId("ux-directive-chip"));
    await settle();
    expect(window.localStorage.getItem("gents.ux.board.highlight")).toBe('"3"');
  });

  it("draws a page in the frame, keeps its state across the shell's re-renders, and answers its verbs under its own context", async () => {
    await load();
    await settle();
    const page = registry.getArea(PLUGIN_PAGE_AREA)[0]!;
    const view = render(<>{page.render!()}</>);
    await settle();
    const mount = frameMount("acme/kit/board")!;
    expect(mount.querySelector("[data-testid=draft]")).not.toBeNull();

    /* a draft survives a re-render of the shell around the viewport */
    fireEvent.change(mount.querySelector("[data-testid=draft]")!, {
      target: { value: "half" },
    });
    view.rerender(<>{page.render!()}</>);
    await settle();
    expect((mount.querySelector("[data-testid=draft]") as HTMLInputElement).value).toBe(
      "half",
    );

    /* the data verb: the host fills the listing and the selected node */
    fireEvent.click(mount.querySelector("[data-testid=load]")!);
    await settle();
    expect(calls).toContainEqual({
      verb: "data.call",
      args: ["acme/kit/board", "did:key:a", "board", "list_tasks", { limit: 5 }],
    });
    expect(mount.querySelector("[data-testid=rows]")).toHaveTextContent("a task");

    /* the pack verb: the listing is the host's, never the message's */
    fireEvent.click(mount.querySelector("[data-testid=call]")!);
    await settle();
    expect(calls).toContainEqual({
      verb: "plugin",
      args: ["acme/kit/board", "store", { action: "list" }],
    });

    /* storage writes land in the shell's storage under the plugin's namespace */
    fireEvent.click(mount.querySelector("[data-testid=remember]")!);
    await settle();
    expect(window.localStorage.getItem("gents.ux.board.seen")).toBe("3");

    /* unmounting the viewport hides the surface; the frame stays */
    view.unmount();
    await settle();
    expect(frames.get("acme/kit/board")?.runtime.shown()).toEqual([]);
    expect(mount.querySelector("[data-testid=draft]")).toBeNull();
  });

  it("draws an agent section with the route's node as props", async () => {
    await load();
    await settle();
    const section = registry.getArea(AGENT_SECTIONS_AREA)[0]!.data as {
      group: string;
      label: string;
      render: (p: { nodeDid: string }) => React.ReactNode;
    };
    expect(section.group).toBe("Packs");
    render(<>{section.render({ nodeDid: "did:key:z" })}</>);
    await settle();
    expect(
      frameMount("acme/kit/board")!.querySelector("[data-testid=settings]"),
    ).toHaveTextContent("settings for did:key:z");
  });

  it("takes the frame down with the deactivation and builds a new one on enable", async () => {
    await load();
    await settle();
    expect(frames.has("acme/kit/board")).toBe(true);
    await setUxPluginEnabled("board", false);
    await settle();
    expect(frames.has("acme/kit/board")).toBe(false);
    expect(registry.getArea(NAV_AREA)).toEqual([]);
    expect(uxPluginRecords().board?.status).toBe("disabled");

    await setUxPluginEnabled("board", true);
    await settle();
    expect(frames.has("acme/kit/board")).toBe(true);
    expect(registry.getArea(NAV_AREA).map((c) => c.id)).toEqual(["board:nav"]);
    expect(sandboxHostOf("board")?.live).toBe(true);
  });

  it("fails the plugin on an undeclared area, with nothing left registered and no frame", async () => {
    await load(plugin, {
      declared: { areas: [NAV_AREA, PLUGIN_PAGE_AREA], directives: [] },
    });
    await settle();
    expect(uxPluginRecords().board?.status).toBe("error");
    expect(uxPluginRecords().board?.error).toContain(AGENT_SECTIONS_AREA);
    expect(registry.getArea(NAV_AREA)).toEqual([]);
    expect(registry.getArea(AGENT_SECTIONS_AREA)).toEqual([]);
    expect(frames.has("acme/kit/board")).toBe(false);
  });

  it("lands a module that fails, a register that throws and a page that throws on the right row", async () => {
    await load(
      `export default { id: 'board', register() { throw new Error('no thanks') } }`,
    );
    await settle();
    expect(uxPluginRecords().board?.status).toBe("error");
    expect(uxPluginRecords().board?.error).toContain("no thanks");
    expect(frames.has("acme/kit/board")).toBe(false);
    retireRuntimeUxPlugin("board");

    await load(`throw new Error('boom at load')`);
    await settle();
    expect(uxPluginRecords()["acme/kit/board"]?.status).toBe("error");
    expect(uxPluginRecords()["acme/kit/board"]?.error).toContain("boom at load");
    retireRuntimeUxPlugin("acme/kit/board");

    await load(`
      import { PLUGIN_PAGE_AREA } from '@gents/ux-sdk'
      export default { id: 'board', register(ctx) {
        ctx.register({ id: 'page', area: PLUGIN_PAGE_AREA, render: () => { throw new Error('render failed') } }) } }`);
    await settle();
    const page = registry.getArea(PLUGIN_PAGE_AREA)[0]!;
    render(<>{page.render!()}</>);
    await settle();
    expect(
      frameMount("acme/kit/board")!.querySelector("[data-testid=frame-surface-error]"),
    ).toHaveTextContent("render failed");
    expect(uxPluginRecords().board?.status).toBe("loaded");
  });

  it("refuses a module whose id is not the listing's", async () => {
    await load(`export default { id: 'other', register() {} }`);
    await settle();
    expect(uxPluginRecords()["acme/kit/board"]?.error).toContain('id "other"');
    expect(frames.size).toBe(0);
  });
});
