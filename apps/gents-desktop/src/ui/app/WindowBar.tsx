import {
  useEffect,
  useRef,
  useState,
  type PointerEvent,
  type ReactNode,
  type TouchEvent,
} from "react";
import { ArrowLeft, ArrowRight, Menu, PanelLeft, Search } from "lucide-react";
import { projectSyncOperationalStatus } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import {
  Sheet,
  SheetContent,
  SheetTitle,
  SheetTrigger,
} from "@gents/ui/components/sheet";
import { cn } from "@gents/ui/lib/utils";
import { sidebar } from "./sidebarChange";
import type { History, Route } from "@/lib/router";
import { useSyncHealth } from "@/hooks/useClient";
import { Hint } from "@/screens/Hint";
import { openSearch, SearchButton } from "@/screens/SearchPalette";
import { useAriaKeys, useKeysHint } from "@/commands/commands";
import { useSidebarMode } from "@/preferences";
import { headerIsWindowBar, isWindowsTauriShell } from "../../lib/shellPlatform";
import { DockTabs } from "./Dock";
import { MarkLink } from "./Mark";
import { useNavFooter } from "./navLayout";
import { SidebarModeItems } from "./sidebarModes";
import { NavPanel } from "./RailFlyout";
import { syncDotClass } from "./SyncHealth";
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
          <ArrowLeft className="rtl:-scale-x-100" />
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
          <ArrowRight className="rtl:-scale-x-100" />
        </Button>
      </Hint>
    </div>
  );
}

/* how long a press on the toggle is held before it opens the mode menu, as
   a held Back button opens its history */
const HOLD_MS = 450;

/* on an expanded sidebar's inside edge, with its other controls; with the
   sidebar hidden or icons only, first in the page's bar, where a hidden
   sidebar is brought back */
function SidebarToggle() {
  const expanded = useSidebarMode() === "expanded";
  const label = expanded ? "Collapse sidebar" : "Expand sidebar";
  const keys = useKeysHint("sidebar.toggle");
  const aria = useAriaKeys("sidebar.toggle");
  const [menu, setMenu] = useState(false);
  const hold = useRef<number | null>(null);
  /* a hold opened the menu, so the click its release makes is not a toggle */
  const held = useRef(false);
  const cancelHold = () => {
    if (hold.current !== null) window.clearTimeout(hold.current);
    hold.current = null;
  };
  useEffect(() => cancelHold, []);
  const startHold = (e: PointerEvent) => {
    if (e.button !== 0) return;
    held.current = false;
    cancelHold();
    hold.current = window.setTimeout(() => {
      hold.current = null;
      held.current = true;
      setMenu(true);
      /* released over one of its choices, the hold chooses it */
      window.addEventListener(
        "pointerup",
        (up) => {
          document
            .elementFromPoint(up.clientX, up.clientY)
            ?.closest<HTMLElement>("[data-sidebar-modes] [role=menuitemradio]")
            ?.click();
          window.setTimeout(() => (held.current = false));
        },
        { once: true, capture: true },
      );
    }, HOLD_MS);
  };
  /* a click switches between always open and icons only; a held press or
     a right-click opens the menu of every mode */
  return (
    <DropdownMenu
      open={menu}
      onOpenChange={(open, details) => {
        if (open && details.reason.startsWith("trigger")) return;
        setMenu(open);
      }}
    >
      <Hint label={label} keys={keys}>
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={label}
              aria-keyshortcuts={aria}
              aria-pressed={expanded}
              className="shrink-0"
              onPointerDown={startHold}
              onPointerUp={cancelHold}
              onPointerLeave={cancelHold}
              onPointerCancel={cancelHold}
              onContextMenu={(e) => {
                e.preventDefault();
                cancelHold();
                setMenu(true);
              }}
              onClick={() => {
                if (held.current) held.current = false;
                else sidebar.toggle();
              }}
            />
          }
        >
          <PanelLeft />
        </DropdownMenuTrigger>
      </Hint>
      <DropdownMenuContent className="w-auto min-w-44" data-sidebar-modes="">
        <SidebarModeItems item={DropdownMenuItem} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/* a left swipe on the menu drags it with the finger and closes it past a
   short distance; a mostly vertical move leaves the list to scroll */
function useSwipeToClose() {
  const [offset, setOffset] = useState(0);
  const from = useRef<{ x: number; y: number; horizontal: boolean | null } | null>(
    null,
  );
  return {
    offset,
    start: (event: TouchEvent) => {
      const touch = event.touches[0];
      from.current = touch
        ? { x: touch.clientX, y: touch.clientY, horizontal: null }
        : null;
    },
    move: (event: TouchEvent) => {
      const touch = event.touches[0];
      const start = from.current;
      if (!touch || !start) return;
      const dx = touch.clientX - start.x;
      const dy = touch.clientY - start.y;
      if (start.horizontal === null && Math.abs(dx) + Math.abs(dy) > 8) {
        start.horizontal = Math.abs(dx) > Math.abs(dy);
      }
      if (start.horizontal) setOffset(Math.min(0, dx));
    },
    end: (close: () => void) => {
      const closing = offset < -60;
      from.current = null;
      setOffset(0);
      if (closing) close();
    },
  };
}

/* the narrow window's menu also holds what its bar has no room for: the
   mark, history and the sync status, whose dot rides on the menu button */
function NavSheet({
  route,
  settings,
  history,
}: {
  route: Route;
  settings: ReactNode;
  history?: History;
}) {
  const sheet = useExclusivePopover();
  const drag = useSwipeToClose();
  const sync = projectSyncOperationalStatus(useSyncHealth());
  const showsStatus = useNavFooter().status;
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
            className="relative shrink-0 md:hidden"
            aria-label="Menu"
          />
        }
      >
        <Menu />
        {showsStatus && (
          <span
            aria-hidden
            data-testid="menu-sync-dot"
            data-state={sync.kind}
            className={cn(
              "absolute top-1 right-1 size-2 rounded-full ring-2 ring-background",
              syncDotClass(sync),
            )}
          />
        )}
      </SheetTrigger>
      <SheetContent
        ref={sheet.popupRef}
        side="left"
        className="nav-sheet flex w-72 flex-col gap-2 border-border/60 bg-raised pt-4 pb-2"
        style={
          drag.offset < 0
            ? { transform: `translateX(${drag.offset}px)`, transition: "none" }
            : undefined
        }
        onTouchStart={drag.start}
        onTouchMove={drag.move}
        onTouchEnd={() => drag.end(() => sheet.onOpenChange(false))}
        onTouchCancel={() => drag.end(() => {})}
        onClickCapture={(e) => {
          if (
            (e.target as HTMLElement).closest(
              'a[href], [data-testid="history-nav"] button',
            )
          )
            sheet.onOpenChange(false);
        }}
      >
        <SheetTitle className="sr-only">Sidebar</SheetTitle>
        <div className="mx-3 flex h-8 items-center gap-1">
          <MarkLink current={route.name === "agents" || route.name === "nodes"} />
          {history && <HistoryNav history={history} />}
        </div>
        <button
          type="button"
          onClick={() => {
            sheet.onOpenChange(false);
            openSearch();
          }}
          className="mx-3 flex h-8 items-center gap-2 rounded-lg px-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        >
          <Search className="size-4" />
          <span className="flex-1 text-start">Search</span>
        </button>
        <NavPanel route={route} settings={settings} syncRow syncInline />
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
            <div className="ms-auto pe-2" data-tauri-drag-region={drag}>
              <SidebarToggle />
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
        <NavSheet route={route} settings={settings} history={history} />
        {wide && !navHoldsHistory && <SidebarToggle />}
        {wide && history && !navHoldsHistory && <HistoryNav history={history} />}
        <ActiveNodeLabel />
        <div
          ref={onPaneBar}
          className="flex h-full min-w-0 flex-1 items-center gap-2"
        />
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
