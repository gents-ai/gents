import type {
  DerivedCancelCauseView,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { memo } from "react";

import {
  AssistantMessageItem,
  LiveAssistantItem,
  PendingUserTurnItem,
  UserMessageItem,
} from "./MessageItems.js";
import { ToolGroup } from "./ToolGroup.js";

export const TimelineItem = memo(function TimelineItem({
  item,
  responseCancelCause,
  responseMaterializedSequence,
}: {
  item: RenderedTimelineItem;
  responseCancelCause?: DerivedCancelCauseView | null;
  responseMaterializedSequence?: number | null;
}) {
  switch (item.kind) {
    case "userMessage":
      return <UserMessageItem item={item} />;
    case "assistantMessage":
      return (
        <AssistantMessageItem
          item={item}
          responseCancelCause={responseCancelCause}
          responseMaterializedSequence={responseMaterializedSequence}
        />
      );
    case "toolGroup":
      return (
        <div className="turn-block">
          <ToolGroup tools={item.tools} />
        </div>
      );
    case "pendingUserTurn":
      return <PendingUserTurnItem item={item} />;
    case "liveAssistant":
      return (
        <LiveAssistantItem
          item={item}
          responseCancelCause={responseCancelCause}
        />
      );
    default:
      return null;
  }
});
