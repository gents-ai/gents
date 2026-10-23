import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { useListWindow } from "@source-inc/gents-desktop-ui";
import type { ReactNode } from "react";
import { memo, useLayoutEffect, useMemo, useRef, useState } from "react";

import { AssistantCancelCauseTurn } from "./transcript/MessageItems.js";
import { TimelineItem, timelineKey } from "./transcript/TimelineItem.js";
import { coalesceToolGroups } from "./transcript/CompactToolGroup.js";

export const MessageList = memo(function MessageList({
  timelineItems,
  timelineIdentity,
  requestCancelCause,
  scrollElement,
  compactTools = false,
  onEditUserMessage,
  renderAfterItem,
}: {
  timelineItems: RenderedTimelineItem[];
  renderAfterItem?: (item: RenderedTimelineItem) => ReactNode;
  timelineIdentity?: string | null;
  requestCancelCause?: DerivedCancelCauseView | null;
  scrollElement?: HTMLElement | null;
  compactTools?: boolean;
  onEditUserMessage?: (
    item: Extract<RenderedTimelineItem, { kind: "userMessage" }>,
  ) => void;
}) {
  const displayItems = useMemo(() => {
    return compactTools ? coalesceToolGroups(timelineItems) : timelineItems;
  }, [compactTools, timelineItems]);
  const window = useListWindow(
    displayItems,
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
  const tail = displayItems[displayItems.length - 1] ?? null;
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
          const item = displayItems[index]!;
          return (
            <li key={timelineKey(item)} {...window.attributes(index, start)}>
              <TimelineItem
                item={item}
                onEditUserMessage={onEditUserMessage}
                compactTools={compactTools}
                animateAssistantReveal={item.itemKey === revealAssistantItemKey}
              />
              {renderAfterItem?.(item)}
            </li>
          );
        })}
      </ul>
      {requestCancelCause ? (
        <AssistantCancelCauseTurn
          cause={requestCancelCause}
          compact={compactTools}
        />
      ) : null}
    </>
  );
});

const estimateItemSize = () => 120;
