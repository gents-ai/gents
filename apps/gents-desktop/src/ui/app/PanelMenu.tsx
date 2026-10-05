/* The pane's menu, a dot menu in its bar: what can show in the dock here,
   with what is already showing marked, and a way to hide or show the
   dock. One control, however many surfaces there are. */
import type { ReactNode } from "react";
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
import { Hint } from "@/screens/Hint";
import { listSurfaces } from "./surfaces";
import { inScope } from "./dock-scope";
import { useWorkspace, workspace } from "./workspace";

export function PanelMenu({
  routeName,
  size = "icon-sm",
  actions,
}: {
  routeName: string;
  size?: "icon-xs" | "icon-sm";
  /** the screen's own actions on what it shows (menu items), below the surfaces */
  actions?: ReactNode;
}) {
  const { dock } = useWorkspace();
  const surfaces = listSurfaces("dock").filter((s) => inScope(s, routeName));
  const showing =
    dock.open && dock.tabs.some((id) => surfaces.some((s) => s.id === id));
  return (
    <DropdownMenu>
      <Hint label="More">
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size={size}
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
        className="w-48 text-xs [&_[role=menuitem]]:text-xs"
      >
        <DropdownMenuGroup>
          {surfaces.map((s) => {
            const open = dock.open && dock.tabs.includes(s.id);
            return (
              <DropdownMenuItem
                key={s.id}
                onClick={() =>
                  open && dock.active === s.id
                    ? workspace.closeTab(s.id)
                    : workspace.openSurface(s.id)
                }
              >
                <s.icon className="size-4" />
                <span className="flex-1">{s.title}</span>
                {open && <Check className="size-4 text-muted-foreground" />}
              </DropdownMenuItem>
            );
          })}
        </DropdownMenuGroup>
        {/* hidden tabs are a desktop idea; the phone's sheet simply closes */}
        {dock.tabs.length > 0 && (
          <>
            <DropdownMenuSeparator className="max-md:hidden" />
            <DropdownMenuItem
              className="max-md:hidden"
              onClick={() =>
                dock.open ? workspace.closeDock() : workspace.reopenDock()
              }
            >
              {dock.open ? "Hide panel" : "Show panel"}
            </DropdownMenuItem>
          </>
        )}
        {/* the screen's own actions last: reached for less often than a surface */}
        {actions && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>{actions}</DropdownMenuGroup>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
