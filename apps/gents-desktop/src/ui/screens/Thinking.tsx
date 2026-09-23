import { Spinner } from "@gents/ui/components/spinner";

/* The live line of a run. The kit paints no marker; this app chooses the
   lime one here, and only here: in motion. It is built from the marker
   tokens rather than the `highlight` utility, which sizes its padding in
   em for a run of prose and would fight a chip's own. The braille frames
   carry their mass low in the em box, so the spinner is nudged up onto
   the word's optical center. The chip is a line of the assistant's own
   flow, so its box sits on the text column: aligning its spinner to the
   activity marks instead would pull it left of every paragraph. */
export function Thinking() {
  return (
    <p>
      <span className="inline-flex items-center gap-1.5 rounded-sm bg-lime px-2.5 py-1 text-sm leading-none text-marker-foreground">
        <Spinner className="-translate-y-[0.5px]" /> Thinking
      </span>
    </p>
  );
}
