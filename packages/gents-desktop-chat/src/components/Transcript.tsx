import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { memo } from "react";

import {
  AssistantCancelCauseTurn,
  hasVisibleResponseCancelBadgeTarget,
} from "./transcript/MessageItems.js";
import { TimelineItem } from "./transcript/TimelineItem.js";

export const MessageList = memo(function MessageList({
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
      {timelineItems.map((item) => (
        <TimelineItem
          item={item}
          key={`${item.kind}-${item.itemKey}`}
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
