import Proofs.Triggers.Dispatch

/-!
# Trigger Lineage

Lineage consistency for materialized trigger request seeds.
-/

/-- Consistency predicate tying a `RequestSeed`'s `causedBy` tuple to
    the scheduler-level `ExecutionOrigin` that the admission path will
    carry.

Mapping rules:

* Manual intents never reference a trigger document, and land on the
  interactive execution path — the one humans drive synchronously.
* Schedule and event fires both inherit the `scheduled` execution
  origin: the existing state machine already treats event-triggered
  work as scheduler-managed because both are produced without a
  foreground session holding the request. -/
def consistentLineage (seed : RequestSeed) (origin : ExecutionOrigin) : Prop :=
  (seed.causedByTriggerKind = .manual ∧ seed.causedByTriggerId = none ∧
    origin = .interactive) ∨
  (seed.causedByTriggerKind = .schedule ∧ origin = .scheduled) ∨
  (seed.causedByTriggerKind = .event ∧ origin = .scheduled)

/-- **Theorem T4 (lineage shape characterization).**

The `consistentLineage` predicate fully characterizes admissible
`(seed, origin)` pairs at the predicate layer.

This theorem is intentionally definitional: it states the three accepted
lineage shapes as an `iff`, so downstream proofs can pattern-match on the
manual, schedule, and event cases without reopening `consistentLineage`.
The dispatch-specific substance lives one layer up in
`dispatch_materializedTriggerRequest_consistentLineage`, which proves that
actual `dispatch` output and the materialized request origin satisfy this
predicate. -/
theorem T4_lineage_completeness
    (seed : RequestSeed) (origin : ExecutionOrigin) :
    consistentLineage seed origin ↔
      ((seed.causedByTriggerKind = .manual ∧
          seed.causedByTriggerId = none ∧
          origin = .interactive) ∨
       (seed.causedByTriggerKind = .schedule ∧ origin = .scheduled) ∨
       (seed.causedByTriggerKind = .event ∧ origin = .scheduled)) := by
  unfold consistentLineage
  exact Iff.rfl
