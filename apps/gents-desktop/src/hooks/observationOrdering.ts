/* The native owner of Proofs/ClientShell/ObservationOrdering. */

/** Keep async presentation effects attached to the UI intent that started them. */
export function acceptsAsyncResult(
  currentGeneration: number,
  capturedGeneration: number,
) {
  return currentGeneration === capturedGeneration;
}
