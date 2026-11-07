/* The full page a UX plugin owns: what the plugin route (`/plugins/<id>`,
   the main nav's destination for a plugin's row) draws. The plugin with
   that id registered it in the `plugin.pages` area; the page is the first
   it registered there. Mounted behind a boundary, so a throw stays on this
   page; mounted once per registration, so the shell's re-renders reach it
   as props and its drafts survive. The page fills the main pane, edge to
   edge and top to bottom, not the reading column: a plugin page is an
   app surface of its own (a board, a diff viewer) that lays itself out and
   scrolls inside its frame. While the plugin has a window of its own open,
   the page lives there alone, and this one stands aside for it. */
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
import {
  closePluginWindow,
  inNativeShell,
  openPluginWindow,
} from "../../lib/nativeShell";
import { usePluginWindowOpen } from "@/app/pluginWindow";
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
  const native = !windowed && inNativeShell() && !isMobileTauriShell();
  const elsewhere = usePluginWindowOpen(pluginId, native);
  const page = pages.find((c) => c.source === `plugin:${pluginId}` && c.render);
  if (elsewhere) return <OpenElsewhere pluginId={pluginId} />;
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
      {native && <OpenInWindow pluginId={pluginId} />}
      <ContribBoundary id={page.id} variant="pane">
        <ContribRender render={page.render} />
      </ContribBoundary>
    </div>
  );
}

/** opens the page in its own window, or brings that window forward */
function useOpenInWindow(pluginId: string) {
  const name = useUxPluginRecords()[pluginId]?.name ?? pluginId;
  const nodeDid = useSelectedNodeDid();
  return {
    name,
    open: () =>
      void openPluginWindow({
        pluginId,
        nodeDid,
        title: name.slice(0, 120),
      }).catch((error) =>
        toast.error(`Couldn't open ${name} in a new window: ${errorText(error)}`),
      ),
  };
}

function OpenElsewhere({ pluginId }: { pluginId: string }) {
  const { name, open } = useOpenInWindow(pluginId);
  const showHere = () =>
    void closePluginWindow(pluginId).catch((error) =>
      toast.error(`Couldn't show ${name} here: ${errorText(error)}`),
    );
  return (
    <div
      className="flex h-full min-h-0 w-full items-center justify-center p-6"
      data-testid="plugin-page-elsewhere"
    >
      <section
        aria-label={`${name} is open in its own window`}
        className="grid justify-items-center gap-2 rounded-xl border border-dashed border-border/60 px-6 py-10 text-center"
      >
        <span className="grid size-12 place-items-center rounded-2xl bg-raised text-muted-foreground">
          <SquareArrowOutUpRight className="size-6" />
        </span>
        <h2 className="mt-2 font-heading text-lg font-medium text-heading">
          {name} is open in its own window
        </h2>
        <p className="max-w-md text-sm text-muted-foreground">
          Its page, and the notifications it sends, are there until you close that
          window.
        </p>
        <div className="mt-2 flex flex-wrap justify-center gap-2">
          <Button size="sm" onClick={open}>
            Bring it forward
          </Button>
          <Button size="sm" variant="outline" onClick={showHere}>
            Show it here
          </Button>
        </div>
      </section>
    </div>
  );
}

function OpenInWindow({ pluginId }: { pluginId: string }) {
  const { open } = useOpenInWindow(pluginId);
  return (
    <PaneBar>
      <div className="flex-1" />
      <Hint label="Open in new window">
        <Button
          size="icon-sm"
          variant="ghost"
          className="text-muted-foreground hover:text-foreground"
          aria-label="Open in new window"
          data-testid="plugin-page-window"
          onClick={open}
        >
          <SquareArrowOutUpRight />
        </Button>
      </Hint>
    </PaneBar>
  );
}
