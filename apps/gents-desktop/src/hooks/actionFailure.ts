import { errorText } from "../lib/errorText";

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

export function wasShown(error: unknown): boolean {
  return typeof error === "object" && error !== null && shownFailures.has(error);
}
