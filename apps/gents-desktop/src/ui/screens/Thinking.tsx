import { Spinner } from "@gents/ui/components/spinner";

/* The live line of a run. The kit paints no marker; this app chooses the
   lime one here, and only here: in motion. */
export function Thinking() {
  return (
    <p>
      <span className="highlight inline-flex items-center gap-2">
        <Spinner /> Thinking
      </span>
    </p>
  );
}
