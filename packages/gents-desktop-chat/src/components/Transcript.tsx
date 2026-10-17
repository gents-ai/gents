import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { useListWindow } from "@source-inc/gents-desktop-ui";
import { memo, useLayoutEffect, useRef, useState } from "react";

import { AssistantCancelCauseTurn } from "./transcript/MessageItems.js";
import { TimelineItem } from "./transcript/TimelineItem.js";

export const MessageList = memo(function MessageList({
  timelineItems,
  timelineIdentity,
  requestCancelCause,
  scrollElement,
}: {
  timelineItems: RenderedTimelineItem[];
  timelineIdentity?: string | null;
  requestCancelCause?: DerivedCancelCauseView | null;
  scrollElement?: HTMLElement | null;
}) {
  const window = useListWindow(
    timelineItems,
    scrollElement ?? null,
    timelineKey,
    estimateItemSize,
    { enabled: !!scrollElement, anchorTo: "end" },
  );
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
      <ul
        {...window.listProps}
        className="transcript-items"
        aria-label="Messages"
      >
        {window.items.map(({ index, start }) => {
          const item = timelineItems[index]!;
          return (
            <li key={timelineKey(item)} {...window.attributes(index, start)}>
              <TimelineItem
                item={item}
                animateAssistantReveal={item.itemKey === revealAssistantItemKey}
              />
            </li>
          );
        })}
      </ul>
      {requestCancelCause ? (
        <AssistantCancelCauseTurn cause={requestCancelCause} />
      ) : null}
    </>
  );
});

function timelineKey(item: RenderedTimelineItem) {
  return `${item.kind}-${item.itemKey}`;
}

function estimateItemSize() {
  return 120;
}
