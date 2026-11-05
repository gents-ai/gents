import type { EndedAccessView } from "@source-inc/gents-desktop-client";

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
/* how near its end a lease is worth a warning rather than a fact */
export const LEASE_SOON_MS = 3 * DAY_MS;

const count = (n: number, unit: string) => `${n} ${unit}${n === 1 ? "" : "s"}`;

/** How long until a joined node's access runs out, rounded to the unit a
    person plans by; null when it can't be read. `soon` asks for a warning. */
export function leaseLeft(
  expiresAt: string | null | undefined,
  now = Date.now(),
): { text: string; soon: boolean; ended: boolean } | null {
  if (!expiresAt) return null;
  const end = Date.parse(expiresAt);
  if (Number.isNaN(end)) return null;
  const left = end - now;
  if (left <= 0) return { text: "Access ended", soon: true, ended: true };
  const text =
    left < HOUR_MS
      ? count(Math.max(1, Math.ceil(left / MINUTE_MS)), "minute")
      : left < DAY_MS
        ? count(Math.floor(left / HOUR_MS), "hour")
        : count(Math.floor(left / DAY_MS), "day");
  return { text: `Access ends in ${text}`, soon: left < LEASE_SOON_MS, ended: false };
}

/** What ended a joined node's access, said as the person would put it. */
export function endedAccessTitle(ended: EndedAccessView): string {
  return ended.reason === "expired"
    ? `Your access to ${ended.label} ran out`
    : `Your access to ${ended.label} was revoked`;
}

/** What happened and what the person can do about it. */
export function endedAccessDetail(ended: EndedAccessView): string {
  const cause =
    ended.reason === "expired"
      ? "The time its owner allowed is over."
      : "Its owner ended this app's access.";
  return `${cause} To use ${ended.label} again, ask its owner to let you in, then add it again.`;
}
