/* The pane bar: the row at the top of the main pane that a screen fills
   with its own title, marks and actions. The shell owns the slot (and
   keeps the sync chip and window controls at its end); a screen puts
   its content there through a portal, so headers belong to panes and
   the shell never knows what a screen is. */
import { createContext, useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";

export const PaneBarSlotContext = createContext<HTMLElement | null>(null);

export function PaneBar({ children }: { children: ReactNode }) {
  const slot = useContext(PaneBarSlotContext);
  /* a screen rendered without the shell (a test, a preview) keeps its bar
     in its own flow, so what belongs in the bar is still there */
  if (!slot)
    return (
      <div className="flex h-12 items-center gap-2" data-testid="pane-bar">
        {children}
      </div>
    );
  return createPortal(children, slot);
}
