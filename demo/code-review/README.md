# Code-review fan-out/fan-in pack

This pack reviews the current repository with a real three-stage document graph:

```text
ReviewJob -> recon -> N x ReviewArea -> scan -> Finding* + N x ScanResult
                                                    |
                                                    `-> triage -> one TriageReport
```

`ReviewArea`, `Finding`, `ScanResult`, `AgentRequest`, and `TriageReport` all carry the same `run_id`. Recon decides the closed area list before its first write and stamps one consistent `expected_total`. Each scan writes exactly one sentinel. The triage trigger groups those sentinels by `run_id`, compares their stored counts without an inference gate, and fires once with deterministic `group.docs` ordering.

The scan write tools hide `run_id` and `expected_total` from model input. `fill: correlation` stamps the run, while `fill: {"source_field":"expected_total"}` copies the immutable source snapshot persisted on the scan request. Triage can query only `Finding`; scan has read-only file and shell access rooted at `${GENTS_REVIEW_ROOT:-.}`.

Run it from the repository root:

```bash
gents config validate --root demo/code-review
gents demo run code-review --prompt "Review for correctness and durability"
```

The runner's deterministic acceptance checks require:

- all `ReviewArea` rows for the run to agree on `expected_total`, equal to their actual count;
- exactly that many tagged `ScanResult` rows with the same immutable count;
- exactly one `AgentRequest` for `(review-triage, event, run_id)`;
- exactly one tagged `TriageReport`.

Finding quality remains model-dependent; graph shape and durable outputs do not.
