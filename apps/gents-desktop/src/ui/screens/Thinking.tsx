import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";

export function ActivityLine({ status }: { status: string | null }) {
  const trimmed = status?.trim();
  const said = trimmed && trimmed.toLowerCase() !== "thinking" ? trimmed : null;
  return (
    <div
      role={status ? "status" : undefined}
      aria-hidden={status ? undefined : true}
      data-testid={status ? "activity-status" : "activity-status-idle"}
      className={cn("py-2", !status && "invisible")}
    >
      <p className="flex items-center gap-1.5 text-sm leading-none font-medium text-brand">
        {status ? (
          <Spinner className="-translate-y-[0.5px] shrink-0" />
        ) : (
          <span
            data-slot="ascii-loader"
            className="inline-block h-[1em] w-[1ch] shrink-0 overflow-hidden font-mono text-[1.15em] leading-none [contain:strict]"
          >
            ⣾
          </span>
        )}
        Thinking
      </p>
      <p
        className={cn(
          "mt-1.5 ml-[7px] max-w-prose truncate border-l border-border pl-3 text-xs text-muted-foreground",
          !said && "invisible",
        )}
      >
        {said ?? " "}
      </p>
    </div>
  );
}
