import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { useLayoutEffect, useRef, useState } from "react";

export function useAssistantReveal(
  items: readonly RenderedTimelineItem[],
  timelineIdentity?: string | null,
): string | null {
  const previousTailRef = useRef<{
    identity?: string | null;
    itemKey: string;
    kind: RenderedTimelineItem["kind"];
  } | null>(null);
  const tail = items[items.length - 1] ?? null;
  const [revealAssistantItemKey, setRevealAssistantItemKey] = useState<
    string | null
  >(null);
  useLayoutEffect(() => {
    const previousTail = previousTailRef.current;
    setRevealAssistantItemKey(
      tail?.kind === "assistantMessage" &&
        previousTail != null &&
        previousTail.identity === timelineIdentity &&
        previousTail.itemKey !== tail.itemKey &&
        previousTail.kind !== "liveAssistant"
        ? tail.itemKey
        : null,
    );
    previousTailRef.current = tail
      ? { identity: timelineIdentity, itemKey: tail.itemKey, kind: tail.kind }
      : null;
  }, [tail?.itemKey, tail?.kind, timelineIdentity]);

  return revealAssistantItemKey;
}
