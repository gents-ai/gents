const EDGES = [
  {
    write: (
      <>
        seed <code>ReviewJob</code>
      </>
    ),
    arrow: (
      <>
        → trigger <code>review-recon</code> · agent <code>review-recon</code> · task{" "}
        <code>review-recon-task</code>
      </>
    ),
    path: "schemas/review_job.graphql · graph.json · agents/review_recon/ · tasks/review_recon_task/",
  },
  {
    write: (
      <>
        <code>write_review_area</code> → <code>ReviewArea</code> × N
      </>
    ),
    arrow: (
      <>
        → trigger <code>review-scan</code> (parallel) · agent <code>review-scan</code> · task{" "}
        <code>review-scan-task</code>
      </>
    ),
    path: "schemas/review_area.graphql · graph.json · agents/review_scan/ · tasks/review_scan_task/",
  },
  {
    write: (
      <>
        <code>write_scan_result</code> → <code>ScanResult</code> × N
      </>
    ),
    arrow: (
      <>
        → trigger <code>review-verify</code> (per_group) · agent <code>review-verify</code> ·
        task <code>review-verify-task</code>
      </>
    ),
    path: "schemas/scan_result.graphql · graph.json · agents/review_verify/ · tasks/review_verify_task/",
  },
  {
    write: (
      <>
        <code>write_verification_summary</code> → <code>VerificationSummary</code>
      </>
    ),
    arrow: (
      <>
        → trigger <code>review-triage</code> · agent <code>review-triage</code> · task{" "}
        <code>review-triage-task</code> (report only; findings already written)
      </>
    ),
    path: "schemas/verification_summary.graphql · graph.json · agents/review_triage/ · tasks/review_triage_task/",
  },
];

export function WhatWeWillSee() {
  return (
    <section className="talk-block">
      <p className="eyebrow">What we’ll see</p>
      <p className="talk-lead">
        One seed write. Four document edges. No coordinator process.{" "}
        <code>gents graph run code_review --field base=origin/main --watch</code> creates a <code>ReviewJob</code>; each
        create fires a trigger that materializes that stage’s Task on that stage’s Agent.
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
