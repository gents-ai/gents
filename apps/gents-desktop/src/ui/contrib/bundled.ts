/* The bundled door: every `src/ui/ux-plugins/<name>/plugin.js` ships in
   the app as source and loads at boot through the same frame pipeline as
   a runtime plugin, under the folder's name as its id. Drop a folder in;
   no import, no registry edit. Same inventory and live-toggle contract as
   runtime plugins. One-shot: the glob is eager, and discovery must not
   re-run on HMR. */
import { loadRuntimeUxPlugin } from "./loader";

const sources = import.meta.glob<string>("../ux-plugins/*/plugin.js", {
  query: "?raw",
  import: "default",
  eager: true,
});

const styles = import.meta.glob<string>("../ux-plugins/*/plugin.css", {
  query: "?raw",
  import: "default",
  eager: true,
});

const nameOf = (path: string) =>
  /\/ux-plugins\/([^/]+)\/plugin\.js$/.exec(path)?.[1] ?? null;

let discovered = false;

export function discoverBundledUxPlugins(): void {
  if (discovered) return;
  discovered = true;
  for (const [path, source] of Object.entries(sources)) {
    const name = nameOf(path);
    if (!name) continue;
    const css = styles[path.replace(/plugin\.js$/, "plugin.css")] ?? null;
    void loadRuntimeUxPlugin(
      { source, css },
      { origin: name, expectId: name, door: "bundled", file: path },
    );
  }
}

/** test seam */
export function resetBundledDiscovery(): void {
  discovered = false;
}
