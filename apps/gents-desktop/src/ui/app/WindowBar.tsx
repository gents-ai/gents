import { useState, type ReactNode } from "react";
import { ArrowLeft, ArrowRight, Menu } from "lucide-react";
import { Button } from "@gents/ui/components/button";
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
import { headerIsWindowBar, isWindowsTauriShell } from "../../lib/shellPlatform";
import { DockTabs } from "./Dock";
import { MarkLink } from "./Mark";
import { NavPanel } from "./RailFlyout";
import { SyncHealth } from "./SyncHealth";
import type { ShellLayout } from "./useShellLayout";
import { WindowControls } from "./WindowControls";

/* back and forward, the shell's: the way to where the person came from, so
   no screen draws its own back arrow */
function HistoryNav({ history }: { history: History }) {
  return (
    <div className="flex shrink-0 items-center gap-0.5" data-testid="history-nav">
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
          disabled={!history.canForward}
          onClick={history.forward}
        >
          <ArrowRight />
        </Button>
      </Hint>
    </div>
  );
}

/* below md the rail is gone; the menu opens the same panel as a sheet */
function NavSheet({ route, settings }: { route: Route; settings: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <Sheet open={open} onOpenChange={setOpen}>
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
        side="left"
        className="flex w-72 flex-col gap-2 border-border/60 bg-raised pt-4 pb-2"
        onClickCapture={(e) => {
          /* a link in the panel navigates; the sheet goes with it */
          if ((e.target as HTMLElement).closest("a[href]")) setOpen(false);
        }}
      >
        <SheetTitle className="sr-only">Navigation</SheetTitle>
        <NavPanel route={route} settings={settings} syncRow />
      </SheetContent>
    </Sheet>
  );
}

/**
 * The window bar: the shell's own strip across the top, on the same tracks
 * as the row below. On Windows the caption buttons end it; a press on it
 * drags the window. The pane region is the slot a screen fills (the
 * session's back arrow, title, marks, meter and menu), the dock region
 * carries the dock's tabs. Below md it is the app's header: the menu and
 * the mark, then the slot.
 */
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
  /** the settings menu, as a row in the phone's nav sheet */
  settings: ReactNode;
  /** receives the slot screens fill through a portal */
  onPaneBar: (slot: HTMLElement | null) => void;
}) {
  const { columns, wide, docked, dockOpen, divider } = layout;
  const syncHealth = useSyncHealth();
  const drag = headerIsWindowBar() ? "" : undefined;
  const windows = isWindowsTauriShell();
  const { paneHidden, toEnd } = divider;
  return (
    <header
      className="app-titlebar relative col-span-full row-start-1 grid h-12 min-w-0"
      style={{ gridTemplateColumns: columns }}
      data-tauri-drag-region={drag}
      data-testid="window-bar"
    >
      {wide && (
        <div className="flex items-center justify-center" data-tauri-drag-region={drag}>
          <MarkLink />
        </div>
      )}
      <div
        className={cn(
          /* clipped: while the pane is hidden its column is 0 and the
             bar's contents must not spill over the dock's tabs */
          "app-bar-pane flex h-full min-w-0 items-center gap-3 overflow-hidden pr-3",
          /* hidden: no padding either, or a sliver of it would show the arrow's edge */
          paneHidden ? "pointer-events-none px-0" : "app-bar-first",
          /* Windows: whichever region ends the bar leaves room for the caption buttons */
          windows && !(docked && dockOpen) && "pr-[138px]",
        )}
        /* the bar cross-fades as the dock takes the pane's last stretch: the
           pane's controls go, the session tab comes */
        style={toEnd > 0 && !paneHidden ? { opacity: 1 - toEnd } : undefined}
        aria-hidden={paneHidden || undefined}
        data-tauri-drag-region={drag}
      >
        <NavSheet route={route} settings={settings} />
        <MarkLink className="md:hidden" />
        {history && <HistoryNav history={history} />}
        {/* what the screen puts here: its title, marks and actions */}
        <div
          ref={onPaneBar}
          className="flex h-full min-w-0 flex-1 items-center gap-2"
        />
        {/* below md the rail is gone: the dot keeps the status in reach */}
        {!wide && <SyncHealth syncHealth={syncHealth} compact />}
      </div>
      {docked && (
        <div
          className={cn(
            "app-bar-dock flex h-full min-w-0 items-center overflow-hidden",
            windows && dockOpen && "pr-[138px]",
          )}
          /* first in the bar once the pane is hidden, it takes the pane's
             inset, grown over the same last stretch as the session tab: a
             step at either end would jog the tabs sideways */
          style={toEnd > 0 ? { paddingLeft: `calc(0.75rem * ${toEnd})` } : undefined}
          data-tauri-drag-region={drag}
        >
          {dockOpen && (
            <DockTabs
              sessionId={route.name === "session" ? route.sessionId : null}
              routeName={route.name}
              paneTab={toEnd > 0 ? { onShow: divider.showPane, progress: toEnd } : null}
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
