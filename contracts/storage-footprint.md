# Storage footprint implementation stack

Baseline: `storage-footprint` starts at freshly fetched `origin/main`
`047a4ab9918c8c6f39209de920fcc4f08f09d26b` (2026-09-18).
Investigation: gents-ai/gents#1543; implementation issues #1544–#1548.

## Delivery boundaries

1. Quiet idle logs (#1546, #1547): pure wire resolver; ignored-setting
   diagnostics at configuration change; DEBUG for clean known no-op writes.
2. Bound desktop logs (#1548): stable active path, size rotation and bounded
   archives, including launcher-captured child output and fallback paths.
3. Reduce stream writes (#1545): 1000 ms default batching and 4 KiB reasoning
   preview, coordinated with live consumers. Preserve configured overrides,
   first-visible bypass, final transcript, terminal owners and lease semantics.
4. Compact provenance (#1544): preserve reduction keys, admission joins,
   accounting, status and irreducible native-message facts while eliminating
   repeated payload storage.
5. Lossless capture storage (#1544): recover the exact canonical provider body
   from versioned durable data; preserve capture-key integrity and fail-closed
   persist-before-send. Keep existing captures readable without rewriting them.

Implementation commits are accumulated on this branch. Review branch pointers
mark independently buildable boundaries; each PR targets its immediate parent.
Sol owns implementation; Terra owns focused verification and independent review.

## Capture constraints

The existing `RenderedCapture` contract binds canonical JSON values, not HTTP
whitespace or key order. A hash alone is not a replacement for durable exact
body recovery. Extend the model before conformance and implementation whenever
the durable representation changes the capture contract.

Lossless references must bind immutable base data, reject missing or malformed
dependencies, and impose bounded decoding work. Use full values when compression
is not beneficial. Keep one persistence owner and one body decoder for the
capture sink, trace command and context-details reader. The source request's
`request_commit_cid` remains a separate provenance edge from the stored capture
representation's commit witness.

Native multipart tool results and request-local assembly rewrites may not be
recoverable from provider JSON. Preserve their exact information rather than
silently dropping it. Metadata readers must not reconstruct large body payloads
just to inspect status, reduction keys or admission identity.

## Validation and limits

Run focused logger, streaming and consumer tests, capture conformance and real
HTTP capture tests. Run `lake build` for proof changes. Before pushing, run
`cargo test -p gents`, `cargo check --workspace --all-targets`, and affected CLI
and desktop suites. Record failures and their disposition rather than masking
them with retries.

Measure equal synthetic workloads before/after: retained serialized payload
bytes and write counts are distinct from physical SST bytes. Storage estimates
from #1543 are targets, not measured results for this implementation.

This stack reduces new growth. It does not reclaim existing DefraDB history.
No database wipe, historical row conversion, alternate streaming channel,
answer-content truncation, or independent lease-renewal policy is included.
Reprofile after these changes before choosing the next storage work.
