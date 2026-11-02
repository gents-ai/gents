import type { ReactNode } from "react";
import { ArrowLeft, ArrowRight, Menu, PanelLeft, Search } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuTrigger,
} from "@gents/ui/components/context-menu";
import {
  Sheet,
  SheetContent,
  SheetTitle,
  SheetTrigger,
} from "@gents/ui/components/sheet";
import { cn } from "@gents/ui/lib/utils";
import type { History, Route } from "@/lib/router";
import { useSyncHealth } from "@/hooks/useClient";
import { Hint } from "@/screens/Hint";
import { openSearch, SearchButton } from "@/screens/SearchPalette";
import { SIDEBAR_KEYS } from "@/screens/shortcutKeys";
import { preferences } from "@/preferences";
import { headerIsWindowBar, isWindowsTauriShell } from "../../lib/shellPlatform";
import { DockTabs } from "./Dock";
import { MarkLink } from "./Mark";
import { SidebarModeItems } from "./sidebarModes";
import { NavPanel } from "./RailFlyout";
import { SyncHealth } from "./SyncHealth";
import type { ShellLayout } from "./useShellLayout";
import { WindowControls } from "./WindowControls";
import { ActiveNodeLabel } from "./ActiveNodeLabel";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";

function HistoryNav({ history }: { history: History }) {
  return (
    <div className="flex shrink-0 items-center gap-0.5" data-testid="history-nav">
      {/* on a phone, search lives in the menu sheet */}
      <SearchButton className="max-md:hidden" />
      <Hint label="Back">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Back"
          disabled={!history.canBack}
          onClick={history.back}
        >
          <ArrowLeft />
        </Button>
      </Hint>
      <Hint label="Forward">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Forward"
          className="max-md:hidden"
          disabled={!history.canForward}
          onClick={history.forward}
        >
          <ArrowRight />
        </Button>
      </Hint>
    </div>
  );
}

/* on an expanded sidebar's inside edge, with its other controls; with the
   sidebar hidden or icons only, first in the page's bar, where a hidden
   sidebar is brought back */
function SidebarToggle({ hidden }: { hidden: boolean }) {
  const label = hidden ? "Show sidebar" : "Hide sidebar";
  /* a click shows or hides it; its context menu (a right-click, or a long
     press) chooses how it shows */
  return (
    <ContextMenu>
      <Hint label={label} keys={SIDEBAR_KEYS}>
        <ContextMenuTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={label}
              aria-pressed={!hidden}
              className="shrink-0"
              onClick={preferences.toggleNavHidden}
            />
          }
        >
          <PanelLeft />
        </ContextMenuTrigger>
      </Hint>
      <ContextMenuContent className="min-w-44">
        <SidebarModeItems />
      </ContextMenuContent>
    </ContextMenu>
  );
}

function NavSheet({ route, settings }: { route: Route; settings: ReactNode }) {
  const sheet = useExclusivePopover();
  return (
    <Sheet
      open={sheet.open}
      onOpenChange={sheet.onOpenChange}
      onOpenChangeComplete={sheet.onOpenChangeComplete}
    >
      <SheetTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            className="shrink-0 md:hidden"
            aria-label="Menu"
          />
        }
      >
        <Menu />
      </SheetTrigger>
      <SheetContent
        ref={sheet.popupRef}
        side="left"
        className="flex w-72 flex-col gap-2 border-border/60 bg-raised pt-4 pb-2"
        onClickCapture={(e) => {
          if ((e.target as HTMLElement).closest("a[href]")) sheet.onOpenChange(false);
        }}
      >
        <SheetTitle className="sr-only">Navigation</SheetTitle>
        <button
          type="button"
          onClick={() => {
            sheet.onOpenChange(false);
            openSearch();
          }}
          className="mx-3 flex h-8 items-center gap-2 rounded-lg px-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        >
          <Search className="size-4" />
          <span className="flex-1 text-left">Search</span>
        </button>
        <NavPanel route={route} settings={settings} syncRow />
      </SheetContent>
    </Sheet>
  );
}

export function WindowBar({
  route,
  history,
  layout,
  settings,
  onPaneBar,
}: {
  route: Route;
  history?: History;
  layout: ShellLayout;
  settings: ReactNode;
  onPaneBar: (slot: HTMLElement | null) => void;
}) {
  const { columns, wide, docked, dockOpen, divider, drawnNav, navHidden } = layout;
  /* an expanded nav's column has room for the page's own controls, so they
     sit over it, by the mark, rather than out past its edge */
  const navHoldsHistory = wide && drawnNav === "expanded" && !navHidden;
  const syncHealth = useSyncHealth();
  const drag = headerIsWindowBar() ? "" : undefined;
  const windows = isWindowsTauriShell();
  const { paneHidden, toEnd } = divider;
  const onNodes = route.name === "agents" || route.name === "nodes";
  return (
    <header
      className="app-titlebar relative col-span-full row-start-1 grid h-12 min-w-0"
      style={{ gridTemplateColumns: columns }}
      data-tauri-drag-region={drag}
      data-testid="window-bar"
    >
      {wide && (
        /* a hidden sidebar's column is empty but kept, so the bar's cells
           stay over their columns */
        <div
          className="flex min-w-0 items-center overflow-hidden"
          data-tauri-drag-region={drag}
        >
          {!navHidden && (
            <div
              className="flex w-14 shrink-0 justify-center"
              data-tauri-drag-region={drag}
            >
              <MarkLink current={onNodes} />
            </div>
          )}
          {navHoldsHistory && history && <HistoryNav history={history} />}
          {navHoldsHistory && (
            <div className="ml-auto pr-2" data-tauri-drag-region={drag}>
              <SidebarToggle hidden={false} />
            </div>
          )}
        </div>
      )}
      <div
        className={cn(
          "app-bar-pane flex h-full min-w-0 items-center gap-3 overflow-hidden pr-3",
          paneHidden ? "pointer-events-none px-0" : "app-bar-first",
          windows && !(docked && dockOpen) && "pr-[138px]",
        )}
        style={toEnd > 0 && !paneHidden ? { opacity: 1 - toEnd } : undefined}
        aria-hidden={paneHidden || undefined}
        data-tauri-drag-region={drag}
      >
        <NavSheet route={route} settings={settings} />
        {wide && !navHoldsHistory && <SidebarToggle hidden={navHidden} />}
        <MarkLink className="md:hidden" current={onNodes} />
        {history && !navHoldsHistory && <HistoryNav history={history} />}
        <ActiveNodeLabel />
        <div
          ref={onPaneBar}
          className="flex h-full min-w-0 flex-1 items-center gap-2"
        />
        {!wide && <SyncHealth syncHealth={syncHealth} compact />}
      </div>
      {docked && (
        <div
          className={cn(
            "app-bar-dock flex h-full min-w-0 items-center overflow-hidden",
            windows && dockOpen && "pr-[138px]",
          )}
          style={toEnd > 0 ? { paddingLeft: `calc(0.75rem * ${toEnd})` } : undefined}
          data-tauri-drag-region={drag}
        >
          {dockOpen && (
            <DockTabs
              sessionId={route.name === "session" ? route.sessionId : null}
              routeName={route.name}
              paneTab={toEnd > 0 ? { onShow: divider.showPane, progress: toEnd } : null}
              expanded={toEnd > 0.5}
              onExpand={toEnd > 0.5 ? divider.showPane : divider.hidePane}
            />
          )}
        </div>
      )}
      {windows && (
        <div className="absolute inset-y-0 right-0 flex">
          <WindowControls />
        </div>
      )}
    </header>
  );
}
