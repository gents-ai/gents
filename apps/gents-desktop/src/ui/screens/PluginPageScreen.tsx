/* The full page a UX plugin owns: what the plugin route (`/plugins/<id>`,
   the main nav's destination for a plugin's row) draws. The plugin with
   that id registered it in the `plugin.pages` area; the page is the first
   it registered there. Mounted behind a boundary, so a throw stays on this
   page; mounted once per registration, so the shell's re-renders reach it
   as props and its drafts survive. The page fills the main pane, edge to
   edge and top to bottom, not the reading column: a plugin page is an
   app surface of its own (a board, a diff viewer) that lays itself out and
   scrolls inside its frame. */
import { ContribBoundary, ContribRender } from "@/contrib/react/boundary";
import { useContributions } from "@/contrib/react/use-contributions";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";

export function PluginPageScreen({ pluginId }: { pluginId: string }) {
  const pages = useContributions(PLUGIN_PAGE_AREA);
  const page = pages.find((c) => c.source === `plugin:${pluginId}` && c.render);
  if (!page?.render) {
    return (
      <div
        className="flex h-full items-center justify-center text-sm text-muted-foreground"
        data-testid="plugin-page-missing"
      >
        No page for the plugin “{pluginId}”: it is disabled, not installed, or has none.
      </div>
    );
  }
  return (
    <div
      className="flex h-full min-h-0 w-full min-w-0 flex-col overflow-auto p-2 md:p-3"
      data-testid="plugin-page"
    >
      <ContribBoundary id={page.id} variant="pane">
        <ContribRender render={page.render} />
      </ContribBoundary>
    </div>
  );
}
