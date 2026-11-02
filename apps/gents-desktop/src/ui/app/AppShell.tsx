import { useEffect, useRef, useState, type ReactNode } from "react";
import { CircleAlert, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import type { History, Route } from "@/lib/router";
import { MorphProvider, useMorphCard } from "@/lib/morph";
import "@/screens/surfaces";
import { useApp } from "./AppContext";
import { PaneBarSlotContext } from "./PaneBar";
import { CustomizeNavDialog } from "./CustomizeNav";
import { Rail } from "./Rail";
import { RailFlyout } from "./RailFlyout";
import { SettingsLink } from "./SettingsLink";
import { PanelPicker, usePanelCommands } from "./panelCommands";
import { useSidebarMorph } from "./sidebarChange";
import { DockColumn, DockSheet } from "./ShellDock";
import { SwipeHandles } from "./SwipeHandles";
import { useShellLayout, type ShellLayout } from "./useShellLayout";
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

/* a hidden sidebar comes back over the page while the pointer rests at the
   window's left edge, and goes when it leaves, as the hover panel does */
const PEEK_AFTER = 150;
const UNPEEK_AFTER = 180;

function SidebarPeek({
  route,
  width,
  settings,
}: {
  route: Route;
  width: number;
  settings: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const timer = useRef<number | null>(null);
  const later = (next: boolean, ms: number) => {
    if (timer.current) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setOpen(next), ms);
  };
  useEffect(
    () => () => {
      if (timer.current) window.clearTimeout(timer.current);
    },
    [],
  );
  return (
    <>
      <div
        aria-hidden
        data-testid="sidebar-edge"
        className="absolute top-12 bottom-0 left-0 z-30 w-2"
        onPointerEnter={(e) => e.pointerType === "mouse" && later(true, PEEK_AFTER)}
        /* the panel slides in under a pointer already resting here, so it
           sees no entry; leaving the edge is what closes it then */
        onPointerLeave={() => later(false, open ? UNPEEK_AFTER : 0)}
      />
      <div
        data-testid="sidebar-peek"
        inert={!open}
        aria-hidden={!open}
        className={cn(
          "absolute top-12 bottom-0 left-0 z-40 transition-transform duration-200 ease-out motion-reduce:transition-none",
          open ? "translate-x-0" : "pointer-events-none -translate-x-full",
        )}
        style={{ width }}
        onPointerEnter={() => later(true, 0)}
        onPointerLeave={() => later(false, UNPEEK_AFTER)}
        onKeyDown={(e) => e.key === "Escape" && setOpen(false)}
      >
        <RailFlyout
          route={route}
          mode="expanded"
          width={width}
          peek
          settings={settings}
        />
      </div>
    </>
  );
}

function NavResizeHandle({ layout }: { layout: ShellLayout }) {
  return (
    <div
      {...layout.navResize.handleProps}
      aria-label="Resize sidebar"
      className="group absolute top-2 -right-1 bottom-2 z-10 flex w-2 cursor-col-resize items-center justify-center outline-none focus-visible:bg-accent"
    >
      <div className="h-10 w-0.5 rounded-full bg-transparent transition-colors group-hover:bg-muted-foreground group-focus-visible:bg-ring" />
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
  usePanelCommands(route, layout);
  const {
    shellRef,
    columns,
    wide,
    docked,
    dockVisible,
    paneCol,
    paneWidth,
    paneInset,
    drawnNav,
    navHidden,
  } = layout;
  const [paneBar, setPaneBar] = useState<HTMLElement | null>(null);
  const settings = (variant: "rail" | "row") => (
    <SettingsLink variant={variant} route={route} />
  );
  /* a change of how the sidebar shows moves its column, the icons in it
     and the page together */
  const shell = useMorphCard();
  useSidebarMorph(shell.morph);
  return (
    <MorphProvider value={shell}>
      <PaneBarSlotContext.Provider value={paneBar}>
        <div
          className="viewport-frame grid min-h-0 overflow-hidden bg-background text-foreground"
          ref={shellRef}
          data-testid="app-shell"
          data-resize-container=""
          style={{
            gridTemplateColumns: columns,
            gridTemplateRows: "auto minmax(0,1fr)",
          }}
        >
          <WindowBar
            route={route}
            history={history}
            layout={layout}
            settings={settings("row")}
            onPaneBar={setPaneBar}
          />
          {wide && !navHidden && (
            <div
              ref={shell.part("sidebar")}
              className="relative col-start-1 row-start-2 min-h-0 [view-transition-class:morph-column_morph-sidebar]"
            >
              <RailFlyout
                route={route}
                mode={drawnNav}
                width={layout.navResize.width}
                settings={settings("row")}
              >
                <Rail route={route} settings={settings("rail")} />
              </RailFlyout>
              <NavResizeHandle layout={layout} />
            </div>
          )}
          {navHidden && (
            <SidebarPeek
              route={route}
              width={layout.navResize.width}
              settings={settings("row")}
            />
          )}
          <section
            ref={shell.part("pane")}
            className={cn(
              "flex min-h-0 min-w-0 flex-col [view-transition-class:morph-column_morph-pane]",
              "md:col-start-2 md:row-start-2 md:mt-2 md:mr-2 md:mb-2 md:overflow-hidden md:rounded-2xl md:border md:border-border/60",
              navHidden && "md:ml-2",
            )}
            data-testid="pane"
          >
            <main
              className="relative min-h-0 min-w-0 flex-1 overflow-hidden"
              style={
                paneWidth !== null
                  ? { width: Math.max(360, paneWidth - paneInset) }
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
          <CustomizeNavDialog />
          <PanelPicker route={route} />
        </div>
      </PaneBarSlotContext.Provider>
    </MorphProvider>
  );
}
