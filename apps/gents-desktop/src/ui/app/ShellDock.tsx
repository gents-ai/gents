import { useEffect } from "react";
import { Sheet, SheetContent, SheetTitle } from "@gents/ui/components/sheet";
import type { Route } from "@/lib/router";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { Dock } from "./Dock";
import type { ShellLayout } from "./useShellLayout";

const sessionOf = (route: Route) => (route.name === "session" ? route.sessionId : null);

export function DockColumn({ route, layout }: { route: Route; layout: ShellLayout }) {
  const { divider, dockVisible, dockWidth } = layout;
  return (
    <div
      className="relative col-start-3 row-start-2 min-h-0 min-w-0 pt-2 pr-2 pb-2"
      data-testid="dock-cell"
      aria-hidden={!dockVisible}
    >
      <div className="h-full overflow-hidden">
        <div className="h-full" style={{ width: Math.max(divider.min, dockWidth) }}>
          {dockVisible && <Dock sessionId={sessionOf(route)} routeName={route.name} />}
        </div>
      </div>
      {dockVisible && (
        <div
          {...divider.handleProps}
          aria-label="Resize panel"
          className="group absolute top-0 -left-2 flex h-full w-2 cursor-col-resize items-center justify-center outline-none focus-visible:bg-accent"
        >
          <div className="h-10 w-0.5 rounded-full bg-border transition-colors group-hover:bg-muted-foreground group-focus-visible:bg-ring" />
        </div>
      )}
    </div>
  );
}

export function DockSheet({ route, layout }: { route: Route; layout: ShellLayout }) {
  const { docked, dockOpen, closeDock } = layout;
  const sheet = useExclusivePopover(docked ? undefined : closeDock);
  const wanted = !docked && dockOpen;
  const { open, onOpenChange } = sheet;
  useEffect(() => {
    if (wanted !== open) onOpenChange(wanted);
  }, [wanted, open, onOpenChange]);
  if (docked) return null;
  return (
    <Sheet
      open={sheet.open}
      onOpenChange={sheet.onOpenChange}
      onOpenChangeComplete={sheet.onOpenChangeComplete}
    >
      <SheetContent
        ref={sheet.popupRef}
        side="bottom"
        showCloseButton={false}
        className="rounded-t-2xl border-t border-border/60 bg-background p-0 data-[side=bottom]:h-[85dvh]"
      >
        <SheetTitle className="sr-only">Side panel</SheetTitle>
        <Dock sessionId={sessionOf(route)} routeName={route.name} placement="sheet" />
      </SheetContent>
    </Sheet>
  );
}
