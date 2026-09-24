import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { memo, useLayoutEffect, useRef, useState } from "react";

import { AssistantCancelCauseTurn } from "./transcript/MessageItems.js";
import { TimelineItem } from "./transcript/TimelineItem.js";

export const MessageList = memo(function MessageList({
  timelineItems,
  timelineIdentity,
  requestCancelCause,
}: {
  timelineItems: RenderedTimelineItem[];
  timelineIdentity?: string | null;
  requestCancelCause?: DerivedCancelCauseView | null;
}) {
  const previousTailRef = useRef<{
    identity?: string | null;
    itemKey: string;
    kind: RenderedTimelineItem["kind"];
  } | null>(null);
  const tail = timelineItems[timelineItems.length - 1] ?? null;
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

  return (
    <>
      {timelineItems.map((item) => (
        <TimelineItem
          item={item}
          key={`${item.kind}-${item.itemKey}`}
          animateAssistantReveal={item.itemKey === revealAssistantItemKey}
        />
      ))}
      {requestCancelCause ? (
        <AssistantCancelCauseTurn cause={requestCancelCause} />
      ) : null}
    </>
  );
});
