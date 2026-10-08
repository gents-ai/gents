import { useEffect } from "react";
import { Sheet, SheetContent, SheetTitle } from "@gents/ui/components/sheet";
import type { Route } from "@/lib/router";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { Dock } from "./Dock";
import type { ShellLayout } from "./useShellLayout";

const sessionOf = (route: Route) => (route.name === "session" ? route.sessionId : null);

/** The dock as a column beside the pane, where the window has the room. */
export function DockColumn({ route, layout }: { route: Route; layout: ShellLayout }) {
  const { divider, dockVisible, dockWidth } = layout;
  return (
    /* the surface keeps its width inside a clipping cell, so it slides rather than squashes */
    <div
      className="relative col-start-3 row-start-2 min-h-0 min-w-0 pt-2 pr-2 pb-2"
      data-testid="dock-cell"
      aria-hidden={!dockVisible}
    >
      <div className="h-full overflow-hidden">
        {/* below its minimum the card keeps that width and slides under the edge */}
        <div className="h-full" style={{ width: Math.max(divider.min, dockWidth) }}>
          {dockVisible && <Dock sessionId={sessionOf(route)} routeName={route.name} />}
        </div>
      </div>
      {/* the drag handle lives in the gap between the cards: a hairline
          that darkens on hover; arrow keys resize too */}
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

/**
 * The dock as a bottom sheet showing one surface at a time, where the window
 * is too narrow for the column. It takes its turn with the shell's popovers
 * like any other dialog (#1778): the store says it is wanted, the popover
 * owner says when it is on screen. Whether the dock is open is the store's
 * alone: a dismissal, or another popover taking the turn, closes it there;
 * the window growing past the sheet only changes how it is shown, so the
 * column picks it up. Mounted in every layout so that change closes the
 * sheet through its owner.
 */
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
