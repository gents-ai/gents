import type { GraphNode, ReviewSnapshot } from "./types.ts";

export type NodeDocument = {
  collection: string;
  fields: Record<string, unknown>;
};

export function documentForNode(
  node: GraphNode | null,
  snapshot: ReviewSnapshot,
): NodeDocument | null {
  if (!node) {
    return null;
  }
  const areaId = node.id.startsWith("area:")
    ? node.id.slice("area:".length)
    : node.id.startsWith("scan:")
      ? node.id.slice("scan:".length)
      : "";
  switch (node.kind) {
    case "job": {
      const row = snapshot.jobs.find((job) => job.run_id === node.runId);
      return row ? { collection: "ReviewJob", fields: compact(row) } : null;
    }
    case "area": {
      const row = snapshot.areas.find(
        (area) => area.area_id === areaId && area.run_id === node.runId,
      );
      return row ? { collection: "ReviewArea", fields: compact(row) } : null;
    }
    case "scan": {
      const row = snapshot.scans.find(
        (scan) => scan.area_id === areaId && scan.run_id === node.runId,
      );
      return row ? { collection: "ScanResult", fields: compact(row) } : null;
    }
    case "verify": {
      const row = snapshot.summaries.find((summary) => summary.run_id === node.runId);
      return row ? { collection: "VerificationSummary", fields: compact(row) } : null;
    }
    case "verdict": {
      const findingId = node.id.slice("verdict:".length);
      const row = snapshot.verdicts.find(
        (verdict) => verdict.finding_id === findingId && verdict.run_id === node.runId,
      );
      return row ? { collection: "FindingVerdict", fields: compact(row) } : null;
    }
    case "triage": {
      const row = snapshot.reports.find((report) => report.run_id === node.runId);
      return row ? { collection: "TriageReport", fields: compact(row) } : null;
    }
    default:
      return null;
  }
}

function compact(row: object): Record<string, unknown> {
  const fields: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(row)) {
    if (key === "_docID" || value === undefined || value === null || value === "") {
      continue;
    }
    fields[key] = value;
  }
  return fields;
}
