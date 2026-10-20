import { SlidersHorizontal } from "lucide-react";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { cn } from "@gents/ui/lib/utils";
import { href, type Route } from "@/lib/router";

/** the way to the app's settings screen: an icon on the rail, a row in the
    navigation panel */
export function SettingsLink({
  variant,
  route,
}: {
  variant: "rail" | "row";
  route: Route;
}) {
  const active = route.name === "settings";
  const to = href({ name: "settings" });
  if (variant === "rail")
    return (
      <Tooltip>
        <TooltipTrigger
          render={
            <a
              href={to}
              aria-label="Settings"
              aria-current={active ? "page" : undefined}
              className={cn(
                "grid size-8 place-items-center rounded-lg border border-transparent text-muted-foreground transition-colors hover:text-foreground",
                active && "border-border/60 bg-raised text-ink shadow-xs",
              )}
            />
          }
        >
          <SlidersHorizontal className="size-4" />
        </TooltipTrigger>
        <TooltipContent side="right">Settings</TooltipContent>
      </Tooltip>
    );
  return (
    <a
      href={to}
      aria-current={active ? "page" : undefined}
      className={cn(
        "mx-3 flex h-8 w-[calc(100%-1.5rem)] items-center gap-2 rounded-lg border border-transparent pr-2 text-left text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
        active && "border-border/60 bg-raised text-ink shadow-xs",
      )}
    >
      <span className="grid size-[30px] shrink-0 place-items-center">
        <SlidersHorizontal className="size-4" />
      </span>
      <span className="min-w-0 flex-1 truncate">Settings</span>
    </a>
  );
}
