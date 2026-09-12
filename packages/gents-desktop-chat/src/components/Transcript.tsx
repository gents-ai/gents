import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { memo, useRef } from "react";

import {
  AssistantCancelCauseTurn,
  hasVisibleResponseCancelBadgeTarget,
} from "./transcript/MessageItems.js";
import { TimelineItem } from "./transcript/TimelineItem.js";

export const MessageList = memo(function MessageList({
  timelineItems,
  timelineIdentity,
  responseCancelCause,
  responseMaterializedSequence,
}: {
  timelineItems: RenderedTimelineItem[];
  timelineIdentity?: string | null;
  responseCancelCause?: DerivedCancelCauseView | null;
  responseMaterializedSequence?: number | null;
}) {
  const previousTailRef = useRef<{
    identity?: string | null;
    itemKey: string;
    kind: RenderedTimelineItem["kind"];
  } | null>(null);
  const tail = timelineItems[timelineItems.length - 1] ?? null;
  const previousTail = previousTailRef.current;
  const revealAssistantItemKey =
    tail?.kind === "assistantMessage" &&
    previousTail != null &&
    previousTail.identity === timelineIdentity &&
    previousTail.itemKey !== tail.itemKey &&
    previousTail.kind !== "liveAssistant"
      ? tail.itemKey
      : null;
  previousTailRef.current = tail
    ? { identity: timelineIdentity, itemKey: tail.itemKey, kind: tail.kind }
    : null;

  const shouldRenderStandaloneCancelCause =
    responseCancelCause != null &&
    !timelineItems.some((item) =>
      hasVisibleResponseCancelBadgeTarget(item, responseMaterializedSequence),
    );

  return (
    <>
      {timelineItems.map((item) => (
        <TimelineItem
          item={item}
          key={`${item.kind}-${item.itemKey}`}
          animateAssistantReveal={item.itemKey === revealAssistantItemKey}
          responseCancelCause={responseCancelCause}
          responseMaterializedSequence={responseMaterializedSequence}
        />
      ))}
      {shouldRenderStandaloneCancelCause ? (
        <AssistantCancelCauseTurn cause={responseCancelCause} />
      ) : null}
    </>
  );
});
