import { useState, type ReactNode } from "react";
import { ArrowLeft, ArrowRight, Menu, Search } from "lucide-react";
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
import { openSearch, SearchButton } from "@/screens/SearchPalette";
import { headerIsWindowBar, isWindowsTauriShell } from "../../lib/shellPlatform";
import { DockTabs } from "./Dock";
import { MarkLink } from "./Mark";
import { NavPanel } from "./RailFlyout";
import { SyncHealth } from "./SyncHealth";
import type { ShellLayout } from "./useShellLayout";
import { WindowControls } from "./WindowControls";

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
          if ((e.target as HTMLElement).closest("a[href]")) setOpen(false);
        }}
      >
        <SheetTitle className="sr-only">Navigation</SheetTitle>
        <button
          type="button"
          onClick={() => {
            setOpen(false);
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
        <div className="flex items-center" data-tauri-drag-region={drag}>
          <div
            className="flex w-14 shrink-0 justify-center"
            data-tauri-drag-region={drag}
          >
            <MarkLink />
          </div>
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
        <MarkLink className="md:hidden" />
        {history && <HistoryNav history={history} />}
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
