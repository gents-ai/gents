import type { Selected } from "./lineageModel.js";

export function SubagentDetailPanel({ selected }: { selected: Selected }) {
  if (!selected) {
    return (
      <p className="subagent-lineage-empty">
        Select a node in the lineage tree to see request or bridge metadata.
      </p>
    );
  }
  if (selected.kind === "req") {
    const node = selected.node;
    return (
      <dl className="subagent-lineage-detail-grid">
        <DetailRow label="request id" value={node.requestId} />
        <DetailRow
          label="resolved via"
          value={node.resolvedVia ?? "local node"}
        />
        <DetailRow label="session" value={node.sessionId} />
        <DetailRow label="deployment" value={node.agentDid} mono />
        <DetailRow label="behavior" value={node.behaviorId} />
        <DetailRow label="lifecycle" value={node.lifecycleState} badge />
        <DetailRow label="status" value={node.status} />
        <DetailRow
          label="depth"
          value={node.subagentDepth != null ? String(node.subagentDepth) : null}
        />
        <DetailRow label="parent req" value={node.causedByParentRequestId} />
        <DetailRow label="parent tool" value={node.causedByParentToolCallId} />
      </dl>
    );
  }
  const edge = selected.edge;
  return (
    <dl className="subagent-lineage-detail-grid">
      <DetailRow label="tool call id" value={edge.parentToolCallId} />
      <DetailRow label="parent req" value={edge.parentRequestId} />
      <DetailRow label="child req" value={edge.childRequestId} />
      <DetailRow label="tool" value={edge.toolName} />
      <DetailRow label="await mode" value={edge.awaitMode} />
      <DetailRow label="cancel policy" value={edge.cancelPolicy} />
      <DetailRow label="lifecycle" value={edge.lifecycleState} badge />
    </dl>
  );
}

function DetailRow({
  label,
  value,
  badge,
  mono,
}: {
  label: string;
  value: string | null | undefined;
  badge?: boolean;
  mono?: boolean;
}) {
  const hasValue = Boolean(value && value.length > 0);
  return (
    <>
      <dt>{label}</dt>
      <dd className={hasValue ? (mono ? "is-mono" : "") : "is-muted"}>
        {hasValue ? (
          badge ? (
            <span
              className={`subagent-lineage-state subagent-lineage-state-${(value ?? "").toLowerCase()}`}
            >
              {value}
            </span>
          ) : (
            value
          )
        ) : (
          "—"
        )}
      </dd>
    </>
  );
}
