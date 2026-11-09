import { useEffect, useState } from "react";
import type { ChatDraft } from "./chatDrafts";
import { nextSendDeadline } from "./chatDeliveryView";

/** The time a send view derives its status at, advanced when the earliest
 * saved send deadline passes so a pending message shows failed then. */
export function useSendDeadlineClock(drafts: readonly ChatDraft[]): number {
  const [now, setNow] = useState(() => Date.now());
  const next = nextSendDeadline(drafts, now);
  useEffect(() => {
    if (next === null) return;
    const timer = setTimeout(() => setNow(Date.now()), Math.max(0, next - Date.now()));
    return () => clearTimeout(timer);
  }, [next]);
  // A draft saved after the last tick derives against the current time.
  return Math.max(now, Date.now());
}
