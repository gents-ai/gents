import { Check } from "lucide-react";
import { ContextMenuItem } from "@gents/ui/components/context-menu";
import { cn } from "@gents/ui/lib/utils";
import { useSidebarMode, type SidebarMode } from "@/preferences";
import { sidebar } from "./sidebarChange";

/** The ways the sidebar shows, as Settings, the sidebar's context menu and
    its toggle's context menu all offer them. */
export const SIDEBAR_MODES: { value: SidebarMode; label: string }[] = [
  { value: "hover", label: "Open on hover" },
  { value: "expanded", label: "Always open" },
  { value: "collapsed", label: "Icons only" },
  { value: "hidden", label: "Hidden" },
];

/** the modes as a context menu's choices, the current one checked */
export function SidebarModeItems() {
  const mode = useSidebarMode();
  return SIDEBAR_MODES.map(({ value, label }) => (
    <ContextMenuItem
      key={value}
      role="menuitemradio"
      aria-checked={mode === value}
      onClick={() => sidebar.setMode(value)}
    >
      <Check className={cn("size-4", mode !== value && "invisible")} />
      {label}
    </ContextMenuItem>
  ));
}
