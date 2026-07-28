import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";

import {
  AssistantCancelCauseTurn,
  hasVisibleResponseCancelBadgeTarget,
} from "./transcript/MessageItems.js";
import { TimelineItem } from "./transcript/TimelineItem.js";

export function MessageList({
  timelineItems,
  responseCancelCause,
  responseMaterializedSequence,
}: {
  timelineItems: RenderedTimelineItem[];
  responseCancelCause?: DerivedCancelCauseView | null;
  responseMaterializedSequence?: number | null;
}) {
  const shouldRenderStandaloneCancelCause =
    responseCancelCause != null &&
    !timelineItems.some((item) =>
      hasVisibleResponseCancelBadgeTarget(item, responseMaterializedSequence),
    );

  return (
    <>
      {timelineItems.map((item, index) => (
        <TimelineItem
          item={item}
          key={`${item.kind}-${item.itemKey}-${index}`}
          responseCancelCause={responseCancelCause}
          responseMaterializedSequence={responseMaterializedSequence}
        />
      ))}
      {shouldRenderStandaloneCancelCause ? (
        <AssistantCancelCauseTurn cause={responseCancelCause} />
      ) : null}
    </>
  );
}
