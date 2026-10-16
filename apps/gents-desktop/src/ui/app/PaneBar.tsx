import { createContext, useContext, type ReactNode } from "react";
import { createPortal } from "react-dom";

export const PaneBarSlotContext = createContext<HTMLElement | null>(null);

export function PaneBar({ children }: { children: ReactNode }) {
  const slot = useContext(PaneBarSlotContext);
  if (!slot)
    return (
      <div className="flex h-12 items-center gap-2" data-testid="pane-bar">
        {children}
      </div>
    );
  return createPortal(children, slot);
}
