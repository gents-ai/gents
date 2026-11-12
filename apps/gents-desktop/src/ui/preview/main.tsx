/* The page `gents config pack ux preview` drives: one UX plugin through the real frame
   pipeline and the real plugin screen, over a host whose data door is the
   node. Every `/__preview/` request is answered by the Rust owner
   (`gents::plugin::ux::preview`) as the browser driver fulfils it, so a
   page's data takes the same node path the desktop's does. */
import ReactDOM from "react-dom/client";

import { installUxHost, type UxDataCallResult, type UxHost } from "@/contrib/plugin";
import { loadRuntimeUxPlugin, probeId } from "@/contrib/loader";
import {
  setUxPluginEnabled,
  uxPluginRecords,
  type UxDeclaredContributions,
} from "@/contrib/plugins-store";
import { registry } from "@/contrib/registry";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";
import { PluginPageScreen } from "@/screens/PluginPageScreen";
import "../../App.css";

interface Binding {
  listing: string | null;
  declared: UxDeclaredContributions | null;
}

interface Target extends Binding {
  source: string;
  css: string | null;
  file: string;
  nodeDid: string;
  /** the id the listing declares; absent for a draft, whose module names it */
  pluginId: string | null;
}

const DOOR_QUIET_MS = 400;
const SETTLE_DEADLINE_MS = 15_000;

let inFlight = 0;
let lastActivity = performance.now();

async function door<T>(verb: string, body: unknown): Promise<T> {
  inFlight += 1;
  lastActivity = performance.now();
  try {
    const response = await fetch(`/__preview/${verb}`, {
      method: "POST",
      body: JSON.stringify(body),
    });
    const answer = (await response.json()) as { value: T } | { error: string };
    if ("error" in answer) throw new Error(answer.error);
    return answer.value;
  } finally {
    inFlight -= 1;
    lastActivity = performance.now();
  }
}

let node: { nodeDid: string } | null = null;

const dataCall = (
  ux: string,
  nodeDid: string,
  surface: string,
  tool: string,
  args: Record<string, unknown>,
) => door<UxDataCallResult>("data/call", { ux, nodeDid, surface, tool, args });

const host: UxHost = {
  shell: {
    route: () => ({ name: "sessions" }),
    navigate: () => undefined,
    deployments: () => (node ? [node as never] : []),
    selectedDeployment: () => node as never,
    selectedSessionId: () => null,
    send: () => Promise.reject(new Error("ctx.send does not run in preview")),
  },
  plugin: (ux, name) => door("plugin", { ux, name }),
  data: {
    call: dataCall,
    surfaces: (ux, nodeDid) =>
      door<{ surfaces: unknown[] }>("data/surfaces", { ux, nodeDid }).then(
        (answer) => answer.surfaces,
      ),
    /* one snapshot: a preview observes the page once */
    watch: async (ux, nodeDid, surface, tool, args, listener) => {
      listener({
        kind: "snapshot",
        value: await dataCall(ux, nodeDid, surface, tool, args),
      });
      return () => undefined;
    },
  },
  os: {
    openExternal: async () => false,
    revealPath: async () => false,
    pickDirectory: async () => null,
  },
};

async function boot(): Promise<string> {
  if (new URLSearchParams(location.search).get("theme") === "dark") {
    document.documentElement.dataset.theme = "dark";
  }
  const target = await door<Target>("target", {});
  node = { nodeDid: target.nodeDid };
  installUxHost(host);
  const module = { source: target.source, css: target.css };
  const pluginId = target.pluginId ?? (await probeId(module, target.file));
  const binding = target.pluginId
    ? target
    : await door<Binding>("bind", { id: pluginId });
  /* a plugin may ship off; the preview is the switch */
  await setUxPluginEnabled(pluginId, true);
  await loadRuntimeUxPlugin(module, {
    origin: target.file,
    expectId: pluginId,
    door: binding.listing ? "pack" : "dev",
    file: target.file,
    listing: binding.listing ?? undefined,
    declared: binding.declared ?? undefined,
  });
  return pluginId;
}

const pluginId = boot();

void pluginId.then(
  (id) => {
    ReactDOM.createRoot(document.getElementById("root")!).render(
      <PluginPageScreen pluginId={id} windowed />,
    );
  },
  (error: unknown) => console.error("[ux-preview] the page did not boot", error),
);

declare global {
  interface Window {
    __UX_PREVIEW__: { settled: () => Promise<Record<string, unknown>> };
  }
}

/* resolves with the page's state once its data calls have gone quiet */
async function settled(): Promise<Record<string, unknown>> {
  let id: string;
  try {
    id = await pluginId;
  } catch (error) {
    return {
      loaded: false,
      error: error instanceof Error ? error.message : String(error),
    };
  }
  const deadline = performance.now() + SETTLE_DEADLINE_MS;
  const quiet = () =>
    inFlight === 0 &&
    performance.now() - lastActivity >= DOOR_QUIET_MS &&
    document.querySelector('[data-testid="ux-frame-viewport"]') !== null;
  while (!quiet() && performance.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  const record = uxPluginRecords()[id];
  const pages = registry
    .getArea(PLUGIN_PAGE_AREA)
    .filter((c) => c.render)
    .map((c) => c.id);
  return {
    loaded: record?.status === "loaded",
    plugin_id: id,
    error: record?.error ?? null,
    pages,
    page: pages[0] ?? null,
    settled: quiet(),
  };
}

window.__UX_PREVIEW__ = { settled };
