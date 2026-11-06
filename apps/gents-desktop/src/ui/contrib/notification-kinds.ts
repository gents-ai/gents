/* The kinds of notification a plugin's module declares beside its id. The
   declaration crosses from the frame as data and is checked here, once,
   when the plugin loads: a malformed one fails the load on the plugin's
   row, as an undeclared area does. */
import type { UxNotificationKind } from "@/sandbox/contract";

export const NOTIFICATION_KIND_RE = /^[a-z][a-z0-9-]{0,31}$/;
export const MAX_NOTIFICATION_KINDS = 8;
export const KIND_LABEL_MAX = 60;
export const KIND_DESCRIPTION_MAX = 160;

const plainText = (value: unknown, max: number): string | null => {
  if (typeof value !== "string") return null;
  const text = value.replace(/\s+/g, " ").trim();
  return text.length > 0 && text.length <= max ? text : null;
};

/** The declared kinds, or why the declaration is refused. Absent declares
    none. */
export function parseNotificationKinds(
  value: unknown,
): { kinds: UxNotificationKind[] } | { error: string } {
  if (value === undefined || value === null) return { kinds: [] };
  if (!Array.isArray(value))
    return { error: "notifications must be a list of { kind, label, description? }" };
  if (value.length > MAX_NOTIFICATION_KINDS)
    return {
      error: `notifications declares ${value.length} kinds; at most ${MAX_NOTIFICATION_KINDS}`,
    };
  const kinds: UxNotificationKind[] = [];
  for (const entry of value as unknown[]) {
    const { kind, label, description } = (entry ?? {}) as Record<string, unknown>;
    if (typeof kind !== "string" || !NOTIFICATION_KIND_RE.test(kind))
      return {
        error: `notification kind ${JSON.stringify(kind)} must be lowercase letters, digits and dashes, starting with a letter, at most 32`,
      };
    if (kinds.some((k) => k.kind === kind))
      return { error: `notification kind "${kind}" is declared twice` };
    const shownLabel = plainText(label, KIND_LABEL_MAX);
    if (!shownLabel)
      return {
        error: `notification kind "${kind}" needs a label of at most ${KIND_LABEL_MAX} characters`,
      };
    if (description !== undefined && !plainText(description, KIND_DESCRIPTION_MAX))
      return {
        error: `notification kind "${kind}" has a description over ${KIND_DESCRIPTION_MAX} characters, or an empty one`,
      };
    kinds.push({
      kind,
      label: shownLabel,
      ...(description !== undefined
        ? { description: plainText(description, KIND_DESCRIPTION_MAX)! }
        : {}),
    });
  }
  return { kinds };
}
