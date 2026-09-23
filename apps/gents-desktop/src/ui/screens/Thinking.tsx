import { Spinner } from "@gents/ui/components/spinner";

/* The live line of a run, drawn as a running tool step: the loader in the
   mark's 16px slot and the label beside it, so it reads as the run's
   status rather than as a control. */
export function Thinking({ label = "Thinking" }: { label?: string }) {
  return (
    <p
      role="status"
      data-testid="activity-status"
      className="flex cursor-default items-center gap-3 py-1.5 text-sm font-medium text-foreground select-none"
    >
      <span className="grid size-4 shrink-0 place-items-center">
        <Spinner />
      </span>
      {label}
    </p>
  );
}
