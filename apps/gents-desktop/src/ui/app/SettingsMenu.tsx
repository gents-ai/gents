import { SlidersHorizontal } from "lucide-react";
import { toast } from "sonner";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import type { ThemePreference } from "@/theme";
import type { NavMode } from "@/nav";
import { preferences, useNavMode, useTheme } from "@/preferences";
import { useApp } from "./AppContext";

/** Global settings at the foot of the nav: an icon on the rail, a row in the
    panel. The theme, the side nav where there is one, and the runtime's DB
    explorer where the bridge has it. */
export function SettingsMenu({
  variant,
  showNav,
}: {
  variant: "rail" | "row";
  /** the side nav choices only make sense where there is a side nav */
  showNav: boolean;
}) {
  const { api } = useApp();
  const theme = useTheme();
  const nav = useNavMode();
  const openDbExplorer = () => {
    void api.openDbExplorer?.().catch((e: unknown) => {
      toast(`DB explorer failed to open: ${String(e)}`);
    });
  };
  return (
    <DropdownMenu>
      {variant === "rail" ? (
        <DropdownMenuTrigger
          render={
            <button
              type="button"
              aria-label="Settings"
              className="grid size-8 cursor-pointer place-items-center rounded-lg border border-transparent text-muted-foreground transition-colors hover:text-foreground"
            />
          }
        >
          <SlidersHorizontal className="size-4" />
        </DropdownMenuTrigger>
      ) : (
        <DropdownMenuTrigger
          render={
            <button
              type="button"
              aria-label="Settings"
              className="mx-3 flex h-8 w-[calc(100%-1.5rem)] cursor-pointer items-center gap-2 rounded-lg border border-transparent pr-2 text-left text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
            />
          }
        >
          <span className="grid size-[30px] shrink-0 place-items-center">
            <SlidersHorizontal className="size-4" />
          </span>
          <span className="min-w-0 flex-1 truncate">Settings</span>
        </DropdownMenuTrigger>
      )}
      <DropdownMenuContent align="start" side="top">
        <DropdownMenuGroup>
          <DropdownMenuLabel>Theme</DropdownMenuLabel>
          <DropdownMenuRadioGroup
            value={theme}
            onValueChange={(v) => preferences.setTheme(v as ThemePreference)}
          >
            <DropdownMenuRadioItem value="light">Light</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="dark">Dark</DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
        </DropdownMenuGroup>
        {showNav && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel>Side nav</DropdownMenuLabel>
              <DropdownMenuRadioGroup
                value={nav}
                onValueChange={(v) => preferences.setNav(v as NavMode)}
              >
                <DropdownMenuRadioItem value="hover">
                  Show on hover
                </DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="expanded">
                  Always expanded
                </DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="collapsed">
                  Collapsed
                </DropdownMenuRadioItem>
              </DropdownMenuRadioGroup>
            </DropdownMenuGroup>
          </>
        )}
        {api.openDbExplorer && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel>Developer</DropdownMenuLabel>
              <DropdownMenuItem onClick={openDbExplorer}>DB Explorer</DropdownMenuItem>
            </DropdownMenuGroup>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
