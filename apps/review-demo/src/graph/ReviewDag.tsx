import type { GraphNode, ReviewGraph } from "./types.ts";

type ReviewDagProps = {
  graph: ReviewGraph;
  selectedId: string | null;
  onSelect: (node: GraphNode) => void;
};

export function ReviewDag({ graph, selectedId, onSelect }: ReviewDagProps) {
  const job = graph.nodes.find((node) => node.kind === "job");
  const areas = graph.nodes.filter((node) => node.kind === "area");
  const scans = new Map(
    graph.nodes
      .filter((node) => node.kind === "scan")
      .map((node) => [node.id.replace(/^scan:/, "area:"), node]),
  );
  const verify = graph.nodes.find((node) => node.kind === "verify");
  const verdicts = graph.nodes.filter((node) => node.kind === "verdict");
  const triage = graph.nodes.find((node) => node.kind === "triage");

  return (
    <div className="dag" data-testid="review-dag">
      {job ? <DagNode node={job} selected={selectedId === job.id} onSelect={onSelect} /> : null}
      {areas.length > 0 ? (
        <>
          <div className="dag-join down" />
          <div className="dag-fan">
            {areas.map((area) => {
              const scan = scans.get(area.id);
              return (
                <div key={area.id} className="dag-col">
                  <DagNode node={area} selected={selectedId === area.id} onSelect={onSelect} />
                  <div className="dag-edge short" />
                  {scan ? (
                    <DagNode node={scan} selected={selectedId === scan.id} onSelect={onSelect} />
                  ) : null}
                </div>
              );
            })}
          </div>
          <div className="dag-join up" />
        </>
      ) : null}
      {verify ? (
        <DagNode node={verify} selected={selectedId === verify.id} onSelect={onSelect} />
      ) : null}
      {verdicts.length > 0 ? (
        <>
          <div className="dag-join down" />
          <div className="dag-fan">
            {verdicts.map((verdict) => (
              <DagNode
                key={verdict.id}
                node={verdict}
                selected={selectedId === verdict.id}
                onSelect={onSelect}
              />
            ))}
          </div>
          <div className="dag-join up" />
        </>
      ) : (
        <div className="dag-edge" />
      )}
      {triage ? (
        <DagNode node={triage} selected={selectedId === triage.id} onSelect={onSelect} />
      ) : null}
    </div>
  );
}

function DagNode({
  node,
  selected,
  onSelect,
}: {
  node: GraphNode;
  selected: boolean;
  onSelect: (node: GraphNode) => void;
}) {
  return (
    <button
      type="button"
      title={node.detail || node.label}
      className={`dag-node kind-${node.kind} state-${node.state}${selected ? " selected" : ""}`}
      onClick={() => onSelect(node)}
    >
      <span className="dag-label">{node.label}</span>
      {node.badges.length > 0 ? (
        <span className="dag-badges">
          {node.badges.map((badge) => (
            <span key={badge} className="chip">
              {badge}
            </span>
          ))}
        </span>
      ) : null}
    </button>
  );
}
