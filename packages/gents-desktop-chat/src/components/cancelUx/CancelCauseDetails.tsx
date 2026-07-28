import type { JSX } from "react";
import type { DerivedCancelCauseView } from "@source-inc/gents-desktop-client";

export type CancelCauseDetailsProps = {
  cause: DerivedCancelCauseView;
};

const CAUSE_LABELS: Record<DerivedCancelCauseView["cause"], string> = {
  userCancelled: "Cancelled by you",
  interrupted: "Interrupted",
  deadline: "Hit its deadline",
  unknown: "Stopped — cause unknown",
};

const SOURCE_LABELS: Record<DerivedCancelCauseView["source"], string> = {
  requestInterrupt: "a direct interrupt on this request",
  parentCascade: "a cascade from the parent request",
  deadline: "the runtime deadline",
  toolLifecycle: "the tool's lifecycle",
  responseInterruptedAt: "the response's interrupt marker",
  unresolved: "an unresolved source",
};

/// Operator-facing summary first; the raw derivation (enums, confidence,
/// evidence rows) stays available behind a disclosure for bug reports.
export function CancelCauseDetails({
  cause,
}: CancelCauseDetailsProps): JSX.Element {
  const at = cause.at ? new Date(cause.at) : null;
  const atLabel =
    at && !Number.isNaN(at.getTime()) ? at.toLocaleTimeString() : null;
  return (
    <div className="cause-details">
      <p className="cause-headline">
        {CAUSE_LABELS[cause.cause]} · via {SOURCE_LABELS[cause.source]}
        {atLabel ? ` · ${atLabel}` : ""}
      </p>
      <details className="cause-evidence">
        <summary>Technical details</summary>
        <dl>
          <dt>cause</dt>
          <dd>{cause.cause}</dd>
          <dt>confidence</dt>
          <dd>{cause.confidence}</dd>
          <dt>source</dt>
          <dd>{cause.source}</dd>
          {cause.at ? (
            <>
              <dt>at</dt>
              <dd>{cause.at}</dd>
            </>
          ) : null}
          {cause.evidence.map((line, i) => (
            <span key={i} style={{ display: "contents" }}>
              <dt>evidence</dt>
              <dd>{line}</dd>
            </span>
          ))}
        </dl>
      </details>
    </div>
  );
}
