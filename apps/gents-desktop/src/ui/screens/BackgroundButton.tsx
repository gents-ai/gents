import { Terminal } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { dockScopeOf, useDockFor } from "@/app/workspace";
import { Hint } from "@/screens/Hint";
import { useBackgroundCount } from "./background";

const SURFACE = "background";

export function BackgroundButton({
  sessionId,
  className,
}: {
  sessionId: string;
  className?: string;
}) {
  const { running } = useBackgroundCount(sessionId);
  const { dock, openSurface, closeTab } = useDockFor(dockScopeOf("session", sessionId));
  const showing = dock.open && dock.active === SURFACE;
  /* shown while something runs, and while its tab is open so it can close it */
  if (running === 0 && !showing) return null;
  const label = `${running} running in background`;
  return (
    <Hint label={label}>
      <Button
        variant="ghost"
        size="sm"
        aria-label={label}
        aria-pressed={showing}
        className={cn(
          "relative h-7 shrink-0 gap-1 px-2 text-xs text-muted-foreground tabular-nums",
          showing && "bg-accent text-foreground",
          className,
        )}
        onClick={() => (showing ? closeTab(SURFACE) : openSurface(SURFACE))}
      >
        <Terminal className="size-3.5" />
        {running}
        {running > 0 && (
          <span
            aria-hidden="true"
            data-testid="background-running"
            className="absolute top-1 right-1 size-1.5 rounded-full bg-brand motion-safe:animate-pulse"
          />
        )}
      </Button>
    </Hint>
  );
}
