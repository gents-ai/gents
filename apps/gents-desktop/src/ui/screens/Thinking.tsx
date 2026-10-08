import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";

/* The live line of a run, in a place kept for it at the foot of the
   transcript. The place keeps its height whether the run is thinking,
   running a step (which shows its own loader, so the line is blank), or
   done: the page under a reader following the foot never moves because the
   line came, went, or changed what it says. Only what is drawn in it
   changes. The braille frames carry their mass low in the em box, so the
   spinner is nudged up onto the word's optical center. */
export function ActivityLine({ status }: { status: string | null }) {
  /* a status that only repeats the word above it says nothing */
  const trimmed = status?.trim();
  const said = trimmed && trimmed.toLowerCase() !== "thinking" ? trimmed : null;
  return (
    /* it stands away from the run above it: what is happening now is not
       another row of what already happened */
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
          /* the spinner's box, still, so an idle line is exactly as tall */
          <span
            data-slot="ascii-loader"
            className="inline-block w-[1ch] shrink-0 font-mono text-[1.15em] leading-none"
          >
            ⣾
          </span>
        )}
        Thinking
      </p>
      {/* what it last said it was doing, under the line and subordinate to
          it. Nested the way an open step nests — a hairline and an indent,
          not a glyph — and clipped to one line; kept, blank, when there is
          nothing to say. It does not run the width of the session: a status
          is a glance, and a line that reaches the far edge asks to be read. */}
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
