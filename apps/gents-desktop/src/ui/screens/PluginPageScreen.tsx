/* The full page a UX plugin owns: what the plugin route (`/plugins/<id>`,
   the main nav's destination for a plugin's row) draws. The plugin with
   that id registered it in the `plugin.pages` area; the page is the first
   it registered there. Mounted behind a boundary, so a throw stays on this
   page; mounted once per registration, so the shell's re-renders reach it
   as props and its drafts survive. The page fills the main pane, edge to
   edge and top to bottom, not the reading column: a plugin page is an
   app surface of its own (a board, a diff viewer) that lays itself out and
   scrolls inside its frame. */
import { SquareArrowOutUpRight } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@gents/ui/components/button";
import { ContribBoundary, ContribRender } from "@/contrib/react/boundary";
import { useUxPluginRecords } from "@/contrib/plugins-store";
import { useContributions } from "@/contrib/react/use-contributions";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";
import { PaneBar } from "@/app/PaneBar";
import { useSelectedNodeDid } from "@/hooks/useClient";
import { errorText } from "../../lib/errorText";
import { inNativeShell, openPluginWindow } from "../../lib/nativeShell";
import { isMobileTauriShell } from "../../lib/shellPlatform";
import { Hint } from "./Hint";

/** `windowed`: the page fills a window of its own, so it offers no other */
export function PluginPageScreen({
  pluginId,
  windowed = false,
}: {
  pluginId: string;
  windowed?: boolean;
}) {
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
      {!windowed && inNativeShell() && !isMobileTauriShell() && (
        <OpenInWindow pluginId={pluginId} />
      )}
      <ContribBoundary id={page.id} variant="pane">
        <ContribRender render={page.render} />
      </ContribBoundary>
    </div>
  );
}

function OpenInWindow({ pluginId }: { pluginId: string }) {
  const name = useUxPluginRecords()[pluginId]?.name ?? pluginId;
  const nodeDid = useSelectedNodeDid();
  return (
    <PaneBar>
      <div className="flex-1" />
      <Hint label="Open in new window">
        <Button
          size="icon-sm"
          variant="ghost"
          aria-label="Open in new window"
          data-testid="plugin-page-window"
          onClick={() =>
            void openPluginWindow({
              pluginId,
              nodeDid,
              title: name.slice(0, 120),
            }).catch((error) =>
              toast.error(`Couldn't open ${name} in a new window: ${errorText(error)}`),
            )
          }
        >
          <SquareArrowOutUpRight />
        </Button>
      </Hint>
    </PaneBar>
  );
}
