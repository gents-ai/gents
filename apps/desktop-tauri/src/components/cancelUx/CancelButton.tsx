import type { JSX } from "react";

const IN_FLIGHT_STATES = new Set([
  "streaming",
  "pending",
  "processing",
  "claimed",
  "waitingForClaim",
  "input_required",
]);

export type CancelButtonProps = {
  activeRequestId: string | null;
  turnState: string | null;
  onInterruptClick: () => void;
  forceVisible?: boolean;
};

export function CancelButton({
  activeRequestId,
  turnState,
  onInterruptClick,
  forceVisible = false,
}: CancelButtonProps): JSX.Element | null {
  const isInFlight = forceVisible || IN_FLIGHT_STATES.has((turnState ?? "").toLowerCase());
  if (!isInFlight) return null;
  const disabled = activeRequestId == null;
  return (
    <button
      type="button"
      className="btn btn-warn cancel-button"
      data-testid="cancel-button"
      disabled={disabled}
      title={disabled ? "Waiting for turn to register" : undefined}
      onClick={disabled ? undefined : onInterruptClick}
    >
      Interrupt
    </button>
  );
}
