/* The app's frame: a window bar with the mark and the screen's own bar; a
   rail with the agent, new session, mailbox and sessions; the content slot
   on the ground. Chrome and canvas share the
   ground; content that needs a surface brings its own. */
import { useState, type ReactNode } from "react";
import { CircleAlert, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import type { History, Route } from "@/lib/router";
/* the surfaces this app registers, wherever the shell is rendered */
import "@/screens/surfaces";
import { useApp } from "./AppContext";
import { PaneBarSlotContext } from "./PaneBar";
import { Rail } from "./Rail";
import { RailFlyout } from "./RailFlyout";
import { SettingsMenu } from "./SettingsMenu";
import { DockColumn, DockSheet } from "./ShellDock";
import { SwipeHandles } from "./SwipeHandles";
import { useShellLayout } from "./useShellLayout";
import { WindowBar } from "./WindowBar";

/* the client's error, over the canvas until dismissed */
function ErrorBanner() {
  const { stores, lifecycle } = useApp();
  const error = stores.client.use.error();
  if (!error) return null;
  return (
    <div
      role="alert"
      data-testid="error-banner"
      className="absolute inset-x-6 top-3 z-30 flex items-center gap-3 rounded-2xl border border-destructive/30 bg-raised px-4 py-2.5 shadow-md"
    >
      <CircleAlert className="size-4 shrink-0 text-destructive" />
      <p className="min-w-0 flex-1 truncate text-sm">{error}</p>
      <Button
        size="icon-xs"
        variant="quiet"
        aria-label="Dismiss"
        data-testid="error-banner-dismiss"
        onClick={() => lifecycle.setError(null)}
      >
        <X />
      </Button>
    </div>
  );
}

export function AppShell({
  route,
  history,
  children,
}: {
  route: Route;
  /** where the person came from and went back from: the bar's own back and forward */
  history?: History;
  children: ReactNode;
}) {
  const layout = useShellLayout(route);
  const { shellRef, columns, wide, docked, dockVisible, paneCol, paneWidth, shownNav } =
    layout;
  /* the pane bar's slot: screens fill it through a portal */
  const [paneBar, setPaneBar] = useState<HTMLElement | null>(null);
  const settings = (variant: "rail" | "row") => (
    <SettingsMenu variant={variant} showNav={wide} />
  );
  return (
    <PaneBarSlotContext.Provider value={paneBar}>
      <div
        className="viewport-frame grid min-h-0 overflow-hidden bg-background text-foreground"
        ref={shellRef}
        data-testid="app-shell"
        data-resize-container=""
        style={{ gridTemplateColumns: columns, gridTemplateRows: "auto minmax(0,1fr)" }}
      >
        <WindowBar
          route={route}
          history={history}
          layout={layout}
          settings={settings("row")}
          onPaneBar={setPaneBar}
        />
        {/* the rail runs the window's full height below the bar; below md the
            bar's menu opens the same panel as a sheet */}
        {wide && (
          <div className="col-start-1 row-start-2 min-h-0">
            <RailFlyout route={route} mode={shownNav} settings={settings("row")}>
              <Rail route={route} settings={settings("rail")} />
            </RailFlyout>
          </div>
        )}
        {/* the pane: a card with no header of its own; its title, marks and
            actions live in the window bar's pane region */}
        <section
          className={cn(
            "flex min-h-0 min-w-0 flex-col",
            "md:col-start-2 md:row-start-2 md:mt-2 md:mr-2 md:mb-2 md:overflow-hidden md:rounded-2xl md:border md:border-border/60",
          )}
          data-testid="pane"
          /* the card's width less its right margin, while the divider settles */
          style={paneWidth === null ? undefined : { width: paneWidth - 8 }}
        >
          <main
            className="@container relative min-h-0 min-w-0 flex-1 overflow-hidden"
            /* past the divider's range the pane keeps its minimum width and
               slides under the dock rather than reflowing narrower */
            style={
              docked && dockVisible && paneCol - 8 < 360 ? { width: 360 } : undefined
            }
          >
            <SwipeHandles />
            <ErrorBanner />
            {children}
          </main>
        </section>
        {docked && <DockColumn route={route} layout={layout} />}
        <DockSheet route={route} layout={layout} />
      </div>
    </PaneBarSlotContext.Provider>
  );
}
