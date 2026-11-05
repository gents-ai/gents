import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

export function SessionCompactionItem({
  item,
}: {
  item: Extract<RenderedTimelineItem, { kind: "sessionCompaction" }>;
}) {
  const count = item.messagesCompacted;
  return (
    <details
      className="session-compaction"
      data-testid="session-compaction-event"
    >
      <summary>
        {`Session context compacted${count == null ? "" : ` · ${count.toLocaleString()} ${count === 1 ? "message" : "messages"}`}`}
      </summary>
      <div className="session-compaction-details">
        <p>
          Messages summarized for the model's context. This count is not a count
          of deleted chat messages.
        </p>
        {item.originalTokens != null && item.compactedTokens != null && (
          <p>
            {item.originalTokens.toLocaleString()} →{" "}
            {item.compactedTokens.toLocaleString()} tokens
          </p>
        )}
        {item.timestamp && (
          <time dateTime={item.timestamp}>{item.timestamp}</time>
        )}
      </div>
    </details>
  );
}
