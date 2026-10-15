/** How a failed action is reported, once: what failed, as a person would say
    it, and why. */
export function actionFailure(label: string, error: unknown): string {
  return `Couldn’t ${label}: ${error instanceof Error ? error.message : String(error)}`;
}

/* Failures an action has already shown the person. A caller that catches
   one still learns that it failed, to keep what was typed, but does not
   report it again. The bridge rejects with strings, which cannot be
   marked, so those become errors with the same message. */
const shownFailures = new WeakSet<object>();

/** `error`, marked as already shown; rethrow this. */
export function shownFailure(error: unknown): object {
  const failure =
    typeof error === "object" && error !== null ? error : new Error(String(error));
  shownFailures.add(failure);
  return failure;
}

/** Whether an action already showed this failure. */
export function wasShown(error: unknown): boolean {
  return typeof error === "object" && error !== null && shownFailures.has(error);
}
