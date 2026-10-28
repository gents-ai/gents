/* A page that mounts one UX plugin through the real frame pipeline — a
   sandboxed iframe, the shell-composed document with the inlined runtime,
   the port — so a browser (Playwright: Chromium and WebKit) can prove what
   the in-process vitest fixture cannot: that the frame loads at all, that
   its origin is opaque and its policy holds, and that a page renders in it.
   The shell around the plugin is a stub host that records every verb. */
import React from "react";
import ReactDOM from "react-dom/client";

import { installUxHost, type UxHost } from "../../src/ui/contrib/plugin";
import {
  loadRuntimeUxPlugin,
  retireRuntimeUxPlugin,
} from "../../src/ui/contrib/loader";
import {
  resetUxPluginStore,
  setUxPluginEnabled,
  uxPluginRecords,
} from "../../src/ui/contrib/plugins-store";
import { registry } from "../../src/ui/contrib/registry";
import { Slot } from "../../src/ui/contrib/react/slot";
import { ContribBoundary } from "../../src/ui/contrib/react/boundary";
import {
  PLUGIN_PAGE_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
  type TranscriptDirectiveContribution,
} from "../../src/ui/contrib/types";
import "../../src/App.css";

const calls: Array<{ verb: string; args: unknown[] }> = [];

const host: UxHost = {
  shell: {
    route: () => ({ name: "sessions" }),
    navigate: (route) => void calls.push({ verb: "navigate", args: [route] }),
    deployments: () => [{ nodeDid: "did:key:harness" } as never],
    selectedDeployment: () => ({ nodeDid: "did:key:harness" }) as never,
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
      return {
        kind: "query",
        output: { results: [{ title: "from the host" }], count: 1 },
      };
    },
    surfaces: async () => [],
  },
  os: {
    openExternal: async () => true,
    revealPath: async () => true,
    pickDirectory: async () => null,
  },
};

/* the fixture plugin: one of each shell-drawn contribution and a page that
   keeps state, reads through ctx.data, and reaches for things the frame
   must refuse */
const PLUGIN = `
import { PLUGIN_PAGE_AREA, SESSION_HEADER_ACTIONS_AREA, TRANSCRIPT_DIRECTIVE_AREA, NAV_AREA, Button, useQuery } from '@gents/ux-sdk'
import { useState } from 'react'
import { jsx, jsxs } from 'react/jsx-runtime'

function Page({ ctx }) {
  const [draft, setDraft] = useState('')
  const rows = useQuery(ctx, 'board', 'list_tasks', { intervalMs: 0 })
  return jsxs('div', { 'data-testid': 'plugin-page', children: [
    jsx('input', { 'data-testid': 'draft', value: draft, onChange: (e) => setDraft(e.target.value) }),
    jsx('p', { 'data-testid': 'rows', children: rows.data ? rows.data.results.map((r) => r.title).join(',') : (rows.error ?? 'loading') }),
    jsx('p', { 'data-testid': 'origin', children: String(window.origin) }),
    jsx('p', { 'data-testid': 'ipc', children: String(typeof window.__TAURI_INTERNALS__) }),
    jsx(Button, { 'data-testid': 'navigate', onClick: () => ctx.shell.navigate({ name: 'plugin', pluginId: 'probe' }), children: 'go' }),
  ] })
}

export default {
  id: 'probe',
  name: 'Probe',
  register(ctx) {
    ctx.registerMany([
      { id: 'nav', area: NAV_AREA, data: { id: 'probe:nav', label: 'Probe', icon: 'zap', to: { name: 'plugin', pluginId: 'probe' }, placement: 'footer' } },
      { id: 'page', area: PLUGIN_PAGE_AREA, render: () => jsx(Page, { ctx }) },
      { id: 'action', area: SESSION_HEADER_ACTIONS_AREA, data: { label: 'probe', icon: 'zap', onClick: () => ctx.storage.set('clicked', true) } },
      { id: 'chip', area: TRANSCRIPT_DIRECTIVE_AREA, data: { name: 'probe', label: 'Probe {id}', onClick: (attrs) => ctx.storage.set('chip', attrs.id) } },
    ])
  },
}
`;

function PageSurface() {
  const page = registry.getArea(PLUGIN_PAGE_AREA)[0];
  if (!page?.render) return <p data-testid="no-page">no page</p>;
  return (
    <div
      data-testid="page-viewport"
      style={{ display: "flex", flexDirection: "column", height: 320 }}
    >
      <ContribBoundary id={page.id}>{page.render()}</ContribBoundary>
    </div>
  );
}

function ChipSurface() {
  const chip = registry.getArea(TRANSCRIPT_DIRECTIVE_AREA)[0]?.data as
    TranscriptDirectiveContribution | undefined;
  if (!chip) return null;
  return (
    <>
      {chip.render({ attrs: { id: "7" }, source: '::probe{id="7"}', streaming: false })}
    </>
  );
}

let rerenderListener: (() => void) | null = null;

/* the shell re-rendering around the viewport: new props everywhere, the
   same elements, so the frame's surface stays mounted */
function Harness() {
  const [, tick] = React.useState(0);
  const [generation, setGeneration] = React.useState(0);
  React.useEffect(() => registry.subscribe(() => tick((n) => n + 1)), []);
  React.useEffect(() => {
    rerenderListener = () => setGeneration((n) => n + 1);
    return () => {
      rerenderListener = null;
    };
  }, []);
  return (
    <main style={{ padding: 16 }} data-generation={generation}>
      <header data-testid="header">
        <Slot area={SESSION_HEADER_ACTIONS_AREA} />
      </header>
      <ChipSurface />
      <PageSurface />
    </main>
  );
}

declare global {
  interface Window {
    __UX_HARNESS__: {
      load: () => Promise<string | null>;
      enable: (on: boolean) => Promise<void>;
      retire: () => void;
      records: () => Readonly<Record<string, unknown>>;
      calls: () => typeof calls;
      storage: (key: string) => string | null;
      rerender: () => void;
    };
  }
}

installUxHost(host);
resetUxPluginStore();
const root = ReactDOM.createRoot(document.getElementById("root")!);
root.render(<Harness />);

window.__UX_HARNESS__ = {
  load: () =>
    loadRuntimeUxPlugin(
      { source: PLUGIN, css: ".probe-css { color: rgb(1, 2, 3) }" },
      {
        origin: "harness/probe",
        expectId: "probe",
        door: "pack",
        file: "/harness/probe/plugin.js",
        listing: "harness/probe",
        declared: {
          areas: [
            "nav",
            PLUGIN_PAGE_AREA,
            SESSION_HEADER_ACTIONS_AREA,
            TRANSCRIPT_DIRECTIVE_AREA,
          ],
          directives: ["probe"],
        },
      },
    ),
  enable: (on) => setUxPluginEnabled("probe", on),
  retire: () => retireRuntimeUxPlugin("probe"),
  records: () => uxPluginRecords(),
  calls: () => calls,
  storage: (key) => window.localStorage.getItem(key),
  rerender: () => rerenderListener?.(),
};
