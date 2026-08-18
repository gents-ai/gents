const EDGES = [
  {
    write: (
      <>
        seed <code>ReviewJob</code>
      </>
    ),
    arrow: (
      <>
        → trigger <code>review-recon</code> · behavior <code>review-recon</code> · task{" "}
        <code>review-recon-task</code>
      </>
    ),
    path: "schemas/review_job.graphql · event_triggers/review-recon/ · agent-behaviors/review-recon/ · tasks/review-recon-task/",
  },
  {
    write: (
      <>
        <code>write_review_area</code> → <code>ReviewArea</code> × N
      </>
    ),
    arrow: (
      <>
        → trigger <code>review-scan</code> (parallel) · behavior <code>review-scan</code> · task{" "}
        <code>review-scan-task</code>
      </>
    ),
    path: "schemas/review_area.graphql · event_triggers/review-scan/ · agent-behaviors/review-scan/ · tasks/review-scan-task/",
  },
  {
    write: (
      <>
        <code>write_scan_result</code> → <code>ScanResult</code> × N
      </>
    ),
    arrow: (
      <>
        → trigger <code>review-verify</code> (per_group) · behavior <code>review-verify</code> ·
        task <code>review-verify-task</code>
      </>
    ),
    path: "schemas/scan_result.graphql · event_triggers/review-verify/ · agent-behaviors/review-verify/ · tasks/review-verify-task/",
  },
  {
    write: (
      <>
        <code>write_verification_summary</code> → <code>VerificationSummary</code>
      </>
    ),
    arrow: (
      <>
        → trigger <code>review-triage</code> · behavior <code>review-triage</code> · task{" "}
        <code>review-triage-task</code> (report only; findings already written)
      </>
    ),
    path: "schemas/verification_summary.graphql · event_triggers/review-triage/ · agent-behaviors/review-triage/ · tasks/review-triage-task/",
  },
];

export function WhatWeWillSee() {
  return (
    <section className="talk-block">
      <p className="eyebrow">What we’ll see</p>
      <p className="talk-lead">
        One seed write. Four document edges. No coordinator process. <code>make review</code>{" "}
        creates a <code>ReviewJob</code>; each create fires a trigger that materializes that
        stage’s Task on that stage’s Behavior.
      </p>
      <ol className="edge-list">
        {EDGES.map((edge) => (
          <li key={edge.path} className="edge-step">
            <div className="edge-write">{edge.write}</div>
            <div className="edge-arrow">{edge.arrow}</div>
            <div className="edge-path">{edge.path}</div>
          </li>
        ))}
      </ol>
    </section>
  );
}
