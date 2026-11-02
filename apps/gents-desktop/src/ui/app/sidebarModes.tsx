import type { ComponentType, ReactNode } from "react";
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

type MenuItem = ComponentType<{
  role: string;
  "aria-checked": boolean;
  onClick: () => void;
  children: ReactNode;
}>;

/** the modes as a menu's choices, the current one checked: a context
    menu's, or `item`'s */
export function SidebarModeItems({
  item: Item = ContextMenuItem,
}: {
  item?: MenuItem;
}) {
  const mode = useSidebarMode();
  return SIDEBAR_MODES.map(({ value, label }) => (
    <Item
      key={value}
      role="menuitemradio"
      aria-checked={mode === value}
      onClick={() => sidebar.setMode(value)}
    >
      <Check className={cn("size-4", mode !== value && "invisible")} />
      {label}
    </Item>
  ));
}
