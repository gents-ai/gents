# Storage footprint implementation stack

Original baseline: freshly fetched `origin/main`
`047a4ab9918c8c6f39209de920fcc4f08f09d26b` (2026-09-18). Before landing,
the stack was rebased onto `0deb7659c696e70efc2f87c3aae114ab8f490fe0`,
preserving native tabs (#1527), v0.18 installers (#1551), and Explorer (#1568).
Investigation: gents-ai/gents#1543; implementation issues #1544–#1548.

## Delivery boundaries

1. Quiet idle logs (#1546, #1547): pure wire resolver; ignored-setting
   diagnostics at configuration change; DEBUG for clean known no-op writes.
2. Delegate desktop/runtime log storage to the operating system (#1548), with
   a native per-user runtime service independent of the desktop frontend.
3. Reduce stream writes (#1545): 1000 ms default batching and 4 KiB reasoning
   preview, coordinated with live consumers. Preserve configured overrides,
   first-visible bypass, final transcript, terminal owners and lease semantics.
4. Lossless capture storage and compact provenance (#1544): preserve reduction
   keys, admission joins, accounting, status and irreducible native-message
   facts while eliminating repeated payload storage. Recover the exact canonical
   provider body from versioned durable data; preserve capture-key integrity and
   fail-closed persist-before-send. Keep existing captures readable without
   rewriting them. The model, codec and all readers land together.
5. Regression evidence: measured logical-byte/write-count comparisons and
   integrated validation across the stack.

Implementation commits are accumulated on this branch. Review branch pointers
mark independently buildable boundaries; each PR targets its immediate parent.
Sol owns implementation; Terra owns focused verification and independent review.

Published review boundaries (each PR targets its immediate parent):

- [#1557](https://github.com/gents-ai/gents/pull/1557),
  `storage-footprint-01-quiet-logs`: resolver and write telemetry.
- [#1558](https://github.com/gents-ai/gents/pull/1558),
  `storage-footprint-02-log-rotation`: native per-user service, OS-owned logs,
  and desktop/menu-bar controls. The branch name is retained, but the custom
  rotating writer and detached log supervisor have been removed.
- [#1559](https://github.com/gents-ai/gents/pull/1559),
  `storage-footprint-03-streaming`: cadence and shared reasoning-preview bound.
- [#1560](https://github.com/gents-ai/gents/pull/1560), `storage-footprint`:
  model, lossless codec, sink, consumers and integrated regression evidence.
  `storage-footprint-04-captures` also marks the initial capture boundary.

The PR descriptions associate the implementation issues with their owning PRs.

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

Capture v2 uses the existing `request_json` column as a container for two
independently encoded records: the canonical provider body and the full native
assembly trace. `provenance_json` holds compact metadata. Both kinds of delta
pin the base document's `request_json` field commit. This avoids a new schema
column, frozen predecessor SDL, or a database migration. Readers distinguish
the physical container's commit witness from the recovered provider body.
Deploy the writer and body readers together: older binaries do not understand
new v2 containers. New readers continue to accept existing v1 bodies and v3
provenance without rewriting those rows.

The encoder checkpoints each logical record after at most eight delta edges
and uses a delta only when it saves at least 256 serialized bytes. This trades
additional bounded reads on explicit body inspection (and base selection when
writing) for smaller durable captures. Missing or unusable optional compression
bases produce full new captures; a missing dependency of an existing delta is
an integrity error, never silently replaced. Unsupported formats fail closed.
Any future capture-retention policy must retain referenced base records (or
their complete dependency closures); this change introduces no capture GC.

The Lean model covers witnessed, bounded resolution and the capture/send gate.
Its byte representation is abstract; concrete UTF-8 splices, field removal,
integer bounds, containers and database reader behavior are fenced by Rust
tests. CID correctness and enforcement of the base row's immutable schema
fields remain DefraDB assumptions, not a new Gents hash or transaction owner.

## Review hardening

The first independent Claude Fable and Grok reviews, plus a targeted Terra
lease audit, identified gaps outside the original passing eval catalogs.
Follow-up work preserves the same persistence and lifecycle owners:

- Optional compression-selection failure falls back to a full new capture;
  actual persistence and existing durable-delta decoding remain fail-closed.
- Paired decoding shares validated base rows and field CIDs between provider
  body and native provenance. Full checkpoints avoid unnecessary base reads.
  The cache is local to one decode, not another durable graph or global owner.
- Storage wrappers are peeled as raw JSON before decoding their payloads, so
  container nesting does not consume the original payload's parser budget.
  Full records are read back and compared before they can be persisted. The
  codec retains bounded parsing and explicitly enables exact finite-float
  round trips; it does not normalize away a failed equality check.
- Live seed verification uses the shared Rust decoder. The Python context
  probe uses the existing decoded CLI surface, not a Python codec. CLI decoded
  provenance is canonical JSON text for both old and new captures.
  Optional UI context-detail decoding degrades to unavailable details with a
  debug diagnostic on corrupt or missing dependencies; ownership checks and
  authoritative writer/export integrity failures still propagate.
- Pending stream progress is eligible after the smaller of the configured
  batching interval and half the owned lease. Only the existing nonempty
  semantic-progress write renews a lease; silence and empty deltas do not.
- Each progress mutation includes only fields changed from the writer's own
  persisted snapshot. DefraDB creates field-history blocks for supplied update
  keys, even when their values are unchanged; omitting an unchanged large
  answer during reasoning progress avoids that history without another cache
  or persistence owner. A real database test checks the content-field CID
  remains unchanged, and Grok coverage checks partial-field history projection.
- The reasoning preview remains 4 KiB. A larger burst can lose provable live
  overlap, especially with a five-second batching override; Grok then defers
  that segment until the exact authoritative final reasoning is available.
  Regression coverage checks exact, once-only final reconciliation rather
  than increasing durable preview storage or adding another stream channel.
  The Codex shim retains its existing different presentation contract: an
  unprovable gap starts a new reasoning item, and terminal hydration completes
  that item with the full durable reasoning. Earlier partial items can therefore
  overlap the final item. The smaller window makes this existing segmentation
  behavior more likely; it does not truncate the authoritative final thought.
- The initial rotating writer and raw-output supervisor were rejected in
  architectural review. The replacement sends existing tracing events to
  native logging and moves runtime process ownership to launchd/systemd.
  See [native runtime service](native-runtime-service.md) for lifecycle,
  installation, diagnostics, and validation boundaries.

Body exports retain their fail-closed whole-command error contract when a
selected durable capture is corrupt. Per-row error objects would be a new
export protocol; no silent partial-success format is introduced here.

## Validation and limits

Original integrated validation (before follow-up hardening):

- `cargo test -p gents --no-fail-fast`: 3,040 passed, 8 ignored, including
  the final missing-version fail-closed regression.
- `cargo test -p gents-protocol rendered_request`: 14 passed.
- `cargo test -p gents-desktop --bin gents-desktop`: 3 passed;
  `cargo test -p gents-desktop-core logging::tests`: 7 passed.
- `lake build` and the Lean contract JSON generator passed, including all five
  generated capture-storage cases.
- The full CLI run passed its unit, configuration, graph, HTTP, seeded, trace,
  and allocator suites, but enrollment and runtime harnesses exposed the
  defects recorded below. Do not describe that full invocation as green.
- After the scoped test corrections, the enrollment harness passed all 6 tests
  and the runtime harness passed all 40. The unresolved #1555 remains recorded.
- `cargo check --workspace --all-targets` passed during integration; repeat it
  after finalizing the commit stack. Formatting and whitespace checks passed.
- At `33bf617f4`, four live N=1/C=1 catalogs passed against
  GLM-5.3-Flash-NVFP4: progressive configurator 4/4, mailbox 5/5, host steward
  10/10, and host maintenance 8/8. The host image was built from that exact
  clean revision. These results do not validate later hardening changes.

Run focused logger, streaming and consumer tests, capture conformance and real
HTTP capture tests. Run `lake build` for proof changes. Before pushing, run
`cargo test -p gents`, `cargo check --workspace --all-targets`, and affected CLI
and desktop suites. Record failures and their disposition rather than masking
them with retries.

Measure equal synthetic workloads before/after: retained serialized payload
bytes and write counts are distinct from physical SST bytes. Storage estimates
from #1543 are targets, not measured results for this implementation.

Measured regression workloads:

- Streaming: replaying 1,001 eight-byte deltas at 10 ms intervals through the
  actual pending-snapshot path produces 11 progress snapshots / 32,528 selected
  payload bytes at the new default, versus 101 / 307,200 at 100 ms. Both use the
  same 4 KiB preview bound, isolating a 9.44x cadence-only byte reduction. Final
  persistence is checked separately. The preview limit itself falls 16x, from
  64 KiB to 4 KiB; these ratios are not a combined storage benchmark.
- Capture: ten real DefraDB captures with growing provider bodies and native
  message traces retain less than half the serialized bytes of full bodies plus
  v3 manifests. Both payloads round-trip exactly, turns 1–8 use deltas, and turn
  9 checkpoints. This assertion measures logical serialized fields, not SST
  compaction, replication traffic, or total runtime disk usage.

Validation uncovered CLI test defects, kept distinct from storage changes:

- #1553: offline Tools export raced a still-running server's database ownership.
  The fixture now stops and reaps that server before exercising offline export.
- #1554: the paced streaming fixture required a flush within 500 ms despite the
  new 1,000 ms cadence. Only that case now allows 2 seconds; the ordinary
  first-visible bound remains 500 ms and both keep provider completion gated.
- #1555: aged-runtime enrollment exceeded its unchanged 30-second sync budget
  in the full suite, then passed alone. Existing fixture serialization rules out
  another enrollment-test mutex as a fix. This remains an unresolved P2P
  history-recovery investigation; an isolated pass does not clear the failure.
- #1556: status inspection raced a legitimate transition from `idle` to
  `debouncing`. The test now requires a recognized diagnostic phase while
  preserving exact readiness, runnable-count, identity and endpoint assertions.
- #1574: the invalid-port rejection fixture's five-second deadline could expire
  while macOS was still in `_dyld_start`, before application code ran. It now
  uses the ordinary 30-second process-startup budget while retaining rejection,
  actionable-error, and no-readiness assertions. Runtime deadlines are unchanged.

This stack reduces new growth. It does not reclaim existing DefraDB history.
No database wipe, historical row conversion, alternate streaming channel,
answer-content truncation, or independent lease-renewal policy is included.
Reprofile after these changes before choosing the next storage work.

The follow-on canonical transcript/refactoring epic is #1571, coordinated with
the `_docID` reference investigation in #1425. Persist-once streaming chunks and
the associated schema/Lean/conformance refactor are not implemented by this stack.

Native logging leaves retention to the user's OS policy; it does not promise
a private 10 MiB/five-archive quota. Existing desktop logs are not deleted or
rewritten. Foreground CLI output, optional shim traces, and explicit scenario
artifacts remain separate. The app no longer creates a log-supervisor process
or its own rotating file writer.
