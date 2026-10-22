import { useState, type ReactNode } from "react";
import { CircleAlert, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import type { History, Route } from "@/lib/router";
import "@/screens/surfaces";
import { useApp } from "./AppContext";
import { PaneBarSlotContext } from "./PaneBar";
import { Rail } from "./Rail";
import { RailFlyout } from "./RailFlyout";
import { SettingsLink } from "./SettingsLink";
import { DockColumn, DockSheet } from "./ShellDock";
import { SwipeHandles } from "./SwipeHandles";
import { useShellLayout } from "./useShellLayout";
import { WindowBar } from "./WindowBar";

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
  history?: History;
  children: ReactNode;
}) {
  const layout = useShellLayout(route);
  const { shellRef, columns, wide, docked, dockVisible, paneCol, paneWidth, shownNav } =
    layout;
  const [paneBar, setPaneBar] = useState<HTMLElement | null>(null);
  const settings = (variant: "rail" | "row") => (
    <SettingsLink variant={variant} route={route} />
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
        {wide && (
          <div className="col-start-1 row-start-2 min-h-0">
            <RailFlyout route={route} mode={shownNav} settings={settings("row")}>
              <Rail route={route} settings={settings("rail")} />
            </RailFlyout>
          </div>
        )}
        <section
          className={cn(
            "flex min-h-0 min-w-0 flex-col",
            "md:col-start-2 md:row-start-2 md:mt-2 md:mr-2 md:mb-2 md:overflow-hidden md:rounded-2xl md:border md:border-border/60",
          )}
          data-testid="pane"
        >
          <main
            className="relative min-h-0 min-w-0 flex-1 overflow-hidden"
            style={
              paneWidth !== null
                ? { width: Math.max(360, paneWidth - 10) }
                : docked && dockVisible && paneCol - 8 < 360
                  ? { width: 360 }
                  : undefined
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
