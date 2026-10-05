import { Spinner } from "@gents/ui/components/spinner";

/* The live line of a run.

   A filled pill is the shape of a badge or a button: it reads as an object
   that ought to respond to a click, be selectable, or carry a dismiss. This
   is neither — it is the turn saying it is still going. So the colour moves
   from the ground to the type, and the line sits in the assistant's own
   flow like any other sentence it writes.

   `brand` rather than lime: the role is the standout for on-states, and it
   resolves to green in light and lime in dark, so the word stays readable
   on both grounds where a single lime would wash out on paper. Medium
   weight because that is already how the kit sets a step in motion, and a
   turn that is still going is the same fact one level up. The braille
   frames carry their mass low in the em box, so the spinner is nudged up
   onto the word's optical center. */
export function Thinking({ label }: { label?: string | null }) {
  /* a status that only repeats the word above it says nothing */
  const trimmed = label?.trim();
  const said = trimmed && trimmed.toLowerCase() !== "thinking" ? trimmed : null;
  return (
    /* it stands away from the run above it: what is happening now is not
       another row of what already happened */
    <div role="status" data-testid="activity-status" className="py-2">
      <p className="flex items-center gap-1.5 text-sm leading-none font-medium text-brand">
        <Spinner className="-translate-y-[0.5px] shrink-0" />
        Thinking
      </p>
      {/* what it last said it was doing, under the line and subordinate to
          it. Nested the way an open step nests — a hairline and an indent,
          not a glyph — and clipped to one line so the height is the same
          whatever it says and the page cannot move while it swaps. */}
      {said && (
        /* it does not run the width of the session: a status is a glance,
           and a line that reaches the far edge asks to be read */
        <p className="mt-1.5 ml-[7px] max-w-prose truncate border-l border-border pl-3 text-xs text-muted-foreground">
          {said}
        </p>
      )}
    </div>
  );
}
