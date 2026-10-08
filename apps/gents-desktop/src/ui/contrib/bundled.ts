/* The bundled door: every `src/ui/ux-plugins/<name>/plugin.{ts,tsx}` that
   default-exports a GentsUxPlugin registers at boot through the Vite glob.
   Drop a folder in; no import, no registry edit. Same inventory and
   live-toggle contract as runtime plugins. One-shot: the glob is eager, and
   discovery must not re-run on HMR. After hermes-agent's contrib/plugins.ts. */
import { activateUxPlugin } from "./activate";
import { isGentsUxPlugin } from "./plugin";

const modules = import.meta.glob<{ default: unknown }>(
  "../ux-plugins/*/plugin.{ts,tsx}",
  {
    eager: true,
  },
);

let discovered = false;

export function discoverBundledUxPlugins(): void {
  if (discovered) return;
  discovered = true;
  for (const [path, mod] of Object.entries(modules)) {
    const plugin = mod.default;
    if (!isGentsUxPlugin(plugin)) {
      console.warn(
        `[ux-plugins] ${path} has no valid default GentsUxPlugin export; skipped`,
      );
      continue;
    }
    activateUxPlugin(plugin, { door: "bundled" });
  }
}

/** test seam */
export function resetBundledDiscovery(): void {
  discovered = false;
}
