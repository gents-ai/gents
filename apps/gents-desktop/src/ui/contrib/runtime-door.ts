/* The runtime doors from the webview: ask the bridge what is on disk
   (`<home>/ux-plugins/<id>/` and every installed pack's ux[]), fetch each
   module's source, and push it through the loader. A manual Reload re-reads
   every entry (an installer that replaces a folder atomically keeps the
   same path). Rust owns the scan; this owns the lifecycle. */
import { call } from "@/screens/agent/bridgeCall";
import { loadRuntimeUxPlugin, retireRuntimeUxPlugin } from "./loader";
import {
  uxPluginRecords,
  type UxDeclaredContributions,
  type UxPluginDoor,
} from "./plugins-store";

/** one entry of desktop_ux_plugins_list */
export interface UxPluginListing {
  /** `<name>` for a dev plugin, `<ns>/<pack>/<name>` for a pack one */
  id: string;
  door: Exclude<UxPluginDoor, "bundled">;
  producer: "file" | "afb";
  /** absolute path of the entry file, or of the .afb */
  file: string;
  pack?: string | null;
  description?: string | null;
  contributes?: UxDeclaredContributions | null;
  defaultEnabled?: boolean | null;
}

/** what desktop_ux_plugin_source answers */
export interface UxPluginSource {
  source: string;
  css?: string | null;
  digest: string;
}

/* live runtime plugins by listing id -> loaded plugin id (null while broken) */
const known = new Map<string, string | null>();
let scanning = false;

export async function listRuntimeUxPlugins(): Promise<UxPluginListing[]> {
  const { plugins } = await call<{ plugins: UxPluginListing[] }>(
    "desktop_ux_plugins_list",
  );
  return plugins;
}

async function loadListing(entry: UxPluginListing): Promise<void> {
  const previous = known.get(entry.id) ?? null;
  let module: UxPluginSource;
  try {
    module = await call<UxPluginSource>("desktop_ux_plugin_source", { id: entry.id });
  } catch (error) {
    /* a vanished file lands on its own row; the next scan retires it */
    const id = await loadRuntimeUxPlugin(
      { source: `throw new Error(${JSON.stringify(String(error))})` },
      { origin: entry.id, door: entry.door, file: entry.file },
    );
    known.set(entry.id, id);
    return;
  }
  const id = await loadRuntimeUxPlugin(
    { source: module.source, css: module.css },
    {
      origin: entry.id,
      door: entry.door,
      file: entry.file,
      producer: entry.producer,
      pack: entry.pack ?? undefined,
      declared: entry.contributes ?? undefined,
      defaultEnabled: entry.defaultEnabled ?? undefined,
    },
  );
  /* the loader only disposes the new id; a file that no longer yields the
     old one is retired here */
  if (previous && previous !== id) retireRuntimeUxPlugin(previous);
  known.set(entry.id, id);
}

/* reconcile disk with the inventory; `reloadKnown` re-reads every entry */
export async function scanRuntimeUxPlugins(reloadKnown = false): Promise<void> {
  if (scanning) return;
  scanning = true;
  try {
    let listings: UxPluginListing[];
    try {
      listings = await listRuntimeUxPlugins();
    } catch {
      /* no local agent home (mobile, or not started): nothing to scan */
      return;
    }
    const seen = new Set<string>();
    for (const entry of listings.sort((a, b) => a.id.localeCompare(b.id))) {
      seen.add(entry.id);
      if (known.has(entry.id) && !reloadKnown) continue;
      await loadListing(entry);
    }
    for (const [id, loaded] of known) {
      if (seen.has(id)) continue;
      if (loaded) retireRuntimeUxPlugin(loaded);
      else if (uxPluginRecords()[id]) retireRuntimeUxPlugin(id);
      known.delete(id);
    }
  } finally {
    scanning = false;
  }
}

/** the panel's Reload button */
export const reloadRuntimeUxPlugins = (): Promise<void> => scanRuntimeUxPlugins(true);

/** test seam */
export function resetRuntimeDoor(): void {
  known.clear();
  scanning = false;
}
