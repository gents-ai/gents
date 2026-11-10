import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";

function observedCount(value: number | null): number | null {
  return value != null && Number.isFinite(value) && value >= 0 ? value : null;
}

export function SessionCompactionItem({
  item,
}: {
  item: Extract<RenderedTimelineItem, { kind: "sessionCompaction" }>;
}) {
  const count = observedCount(item.messagesCompacted);
  const before = observedCount(item.originalTokens);
  const after = observedCount(item.compactedTokens);
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
        {before != null && after != null ? (
          <p>
            {before.toLocaleString()} → {after.toLocaleString()} tokens
          </p>
        ) : before != null ? (
          <p>Before: {before.toLocaleString()} tokens</p>
        ) : after != null ? (
          <p>After: {after.toLocaleString()} tokens</p>
        ) : null}
        {item.timestamp && (
          <time dateTime={item.timestamp}>{item.timestamp}</time>
        )}
      </div>
    </details>
  );
}
