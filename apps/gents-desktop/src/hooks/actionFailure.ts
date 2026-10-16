import { errorText } from "../lib/errorText";

/** How a failed action is reported, once: what failed, as a person would say
    it, and why. */
export function actionFailure(label: string, error: unknown): string {
  return `Couldn’t ${label}: ${errorText(error)}`;
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
