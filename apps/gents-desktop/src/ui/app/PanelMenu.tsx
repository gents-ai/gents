import type { ComponentProps, ReactNode } from "react";
import { Check, EllipsisVertical } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { Hint } from "@/screens/Hint";
import { useSurfaces } from "./surfaces";
import { inScope } from "./dock-scope";
import { useDock } from "./workspace";

export function PanelMenu({
  routeName,
  finalFocus,
  children,
}: {
  routeName: string;
  finalFocus?: ComponentProps<typeof DropdownMenuContent>["finalFocus"];
  /** the items of what the pane shows, before its panels */
  children?: ReactNode;
}) {
  const { dock, closeTab, openSurface, closeDock, reopenDock } = useDock();
  const surfaces = useSurfaces().filter(
    (s) => s.placements.includes("dock") && inScope(s, routeName),
  );
  const showing =
    dock.open && dock.tabs.some((id) => surfaces.some((s) => s.id === id));
  return (
    <DropdownMenu>
      <Hint label="More">
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="More"
              aria-pressed={showing}
              className={showing ? "bg-accent text-foreground" : undefined}
            />
          }
        >
          <EllipsisVertical />
        </DropdownMenuTrigger>
      </Hint>
      <DropdownMenuContent
        align="end"
        finalFocus={finalFocus}
        className={cn(
          children ? "w-56" : "w-48",
          "text-xs [&_[role=menuitem]]:text-xs",
        )}
      >
        {children}
        {children && surfaces.length > 0 && <DropdownMenuSeparator />}
        <DropdownMenuGroup>
          {surfaces.map((s) => {
            const open = dock.open && dock.tabs.includes(s.id);
            return (
              <DropdownMenuItem
                key={s.id}
                onClick={() =>
                  open && dock.active === s.id ? closeTab(s.id) : openSurface(s.id)
                }
              >
                <s.icon className="size-4" />
                <span className="flex-1">{s.title}</span>
                {open && <Check className="size-4 text-muted-foreground" />}
              </DropdownMenuItem>
            );
          })}
        </DropdownMenuGroup>
        {dock.tabs.length > 0 && (
          <>
            <DropdownMenuSeparator className="max-md:hidden" />
            <DropdownMenuItem
              className="max-md:hidden"
              onClick={() => (dock.open ? closeDock() : reopenDock())}
            >
              {dock.open ? "Hide panel" : "Show panel"}
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
