import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { useListWindow } from "@source-inc/gents-desktop-ui";
import type { ReactNode } from "react";
import { Fragment, memo, useMemo } from "react";

import { AssistantCancelCauseTurn } from "./transcript/MessageItems.js";
import { useAssistantReveal } from "./transcript/useAssistantReveal.js";
import { TimelineItem, timelineKey } from "./transcript/TimelineItem.js";
import {
  coalesceToolGroups,
  type CompactTimelineItem,
} from "./transcript/CompactToolGroup.js";

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
  const displayItems = useMemo<CompactTimelineItem[]>(() => {
    return compactTools ? coalesceToolGroups(timelineItems) : timelineItems;
  }, [compactTools, timelineItems]);
  const window = useListWindow(
    displayItems,
    scrollElement ?? null,
    timelineKey,
    estimateItemSize,
    { enabled: !!scrollElement, anchorTo: "end" },
  );
  const revealAssistantItemKey = useAssistantReveal(
    displayItems,
    timelineIdentity,
  );

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
              {item.compactAnchors?.map((anchor) => (
                <Fragment key={timelineKey(anchor)}>
                  {renderAfterItem?.(anchor)}
                </Fragment>
              ))}
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
