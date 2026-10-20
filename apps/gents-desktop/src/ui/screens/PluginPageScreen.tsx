/* The full page a UX plugin owns: what the plugin route (`/plugins/<id>`,
   the main nav's destination for a plugin's row) draws. The plugin with
   that id registered it in the `plugin.pages` area; the page is the first
   it registered there. Mounted behind a boundary, so a throw stays on this
   page; mounted once per registration, so the shell's re-renders reach it
   as props and its drafts survive. */
import { ScrollArea } from "@gents/ui/components/scroll-area";
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
    <ScrollArea className="h-full">
      <div
        className="mx-auto max-w-page px-4 py-6 md:px-8 md:py-8"
        data-testid="plugin-page"
      >
        <ContribBoundary id={page.id} variant="pane">
          <ContribRender render={page.render} />
        </ContribBoundary>
      </div>
    </ScrollArea>
  );
}
