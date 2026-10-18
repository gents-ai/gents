import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { memo } from "react";

import {
  AssistantMessageItem,
  LiveAssistantItem,
  PendingUserTurnItem,
  UserMessageItem,
} from "./MessageItems.js";
import { ToolGroup } from "./ToolGroup.js";
import { CompactToolGroup } from "./CompactToolGroup.js";

export const TimelineItem = memo(function TimelineItem({
  item,
  animateAssistantReveal,
  compactTools,
}: {
  item: RenderedTimelineItem;
  animateAssistantReveal?: boolean;
  compactTools?: boolean;
}) {
  switch (item.kind) {
    case "userMessage":
    case "automatedInput":
      return <UserMessageItem item={item} />;
    case "assistantMessage":
      return (
        <AssistantMessageItem
          item={item}
          animateReveal={animateAssistantReveal}
        />
      );
    case "toolGroup":
      return (
        <div className="turn-block">
          {compactTools ? (
            <CompactToolGroup tools={item.tools} />
          ) : (
            <ToolGroup tools={item.tools} />
          )}
        </div>
      );
    case "pendingUserTurn":
      return <PendingUserTurnItem item={item} />;
    case "liveAssistant":
      return <LiveAssistantItem item={item} />;
    default:
      return null;
  }
});
