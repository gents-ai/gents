export function actionFailure(label: string, error: unknown): string {
  return `Couldn’t ${label}: ${error instanceof Error ? error.message : String(error)}`;
}

const shownFailures = new WeakSet<object>();

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
