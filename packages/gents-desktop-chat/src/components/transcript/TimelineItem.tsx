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

export function timelineKey(item: RenderedTimelineItem) {
  return `${item.kind}-${item.itemKey}`;
}

export const TimelineItem = memo(function TimelineItem({
  item,
  animateAssistantReveal,
  compactTools,
  onEditUserMessage,
}: {
  item: RenderedTimelineItem;
  animateAssistantReveal?: boolean;
  compactTools?: boolean;
  onEditUserMessage?: (
    item: Extract<RenderedTimelineItem, { kind: "userMessage" }>,
  ) => void;
}) {
  switch (item.kind) {
    case "userMessage":
    case "automatedInput":
      return <UserMessageItem item={item} onEdit={onEditUserMessage} />;
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
    case "requestContextReductionSummary":
      return (
        <div
          className="turn-block"
          data-testid="request-context-reduction-summary"
        >
          <span>
            Request context reductions · {item.reductionCount} model inputs
            affected
          </span>
          <span>
            {" "}
            · {item.compactedCount} compacted · {item.repairedCount} repaired
          </span>
        </div>
      );
    case "sessionCompaction":
      return (
        <div className="turn-block" data-testid="session-compaction-event">
          <span>Session history compacted</span>
          {item.messagesCompacted != null && (
            <span> · {item.messagesCompacted} messages</span>
          )}
        </div>
      );
    default:
      return null;
  }
});
