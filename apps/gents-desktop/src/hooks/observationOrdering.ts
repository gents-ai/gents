export function acceptsAsyncResult(
  currentGeneration: number,
  capturedGeneration: number,
) {
  return currentGeneration === capturedGeneration;
}
