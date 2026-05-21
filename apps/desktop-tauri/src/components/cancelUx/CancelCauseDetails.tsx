import type { JSX } from "react";
import type { DerivedCancelCauseView } from "../../lib/types/operations";

export type CancelCauseDetailsProps = {
  cause: DerivedCancelCauseView;
};

export function CancelCauseDetails({ cause }: CancelCauseDetailsProps): JSX.Element {
  return (
    <dl className="cause-details">
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
  );
}
