import Proofs.Configuration

namespace Configuration.Affected

/-- Each document lists the direct configuration documents that its existing
validator inspects. This includes shared inputs whose contents, not just
existence, affect validation. The list must be derived by that same owner. -/
structure Document where
  key : String
  required : List String
  localValid : Bool
  deriving Repr, DecidableEq

/-- The snapshot is the authenticated actor's native-visible configuration.
It cannot establish that hidden documents or hidden referrers do not exist. -/
def valid (visible : List Document) (document : Document) : Bool :=
  document.localValid && document.required.all fun key => visible.any fun d => d.key == key

def affected (changed : List String) (document : Document) : Bool :=
  changed.contains document.key || document.required.any changed.contains

def acceptedDocument (visible : List Document) (changed : List String)
    (document : Document) : Bool :=
  !affected changed document || valid visible document

def accepted (visible : List Document) (changed : List String) : Bool :=
  visible.all (acceptedDocument visible changed)

/-- Selected surfaces may not expose the same tool name twice. -/
def uniqueToolNames (names : List String) : Bool := decide names.Nodup

/-- When a visible document is targeted for removal, the native mutation
must report that exact physical document once. An empty or ambiguous result
does not establish a delete, including when native policy returns no rows.
An actor-scoped lookup cannot distinguish an absent document from a hidden
one; this predicate applies only after a physical target was observed. -/
def removalConfirmed (target : String) (returned : List String) : Bool :=
  returned == [target]

theorem removal_requires_exact_document :
    [removalConfirmed "doc" ["doc"], removalConfirmed "doc" [],
     removalConfirmed "doc" ["other"], removalConfirmed "doc" ["doc", "doc"]] =
      [true, false, false, false] := by decide

theorem affected_document_must_validate (visible : List Document) (changed : List String)
    (document : Document) (check : acceptedDocument visible changed document = true)
    (isAffected : affected changed document = true) : valid visible document = true := by
  simp [acceptedDocument, isAffected] at check
  exact check

theorem unchanged_unaffected_document_is_not_revalidated
    (visible : List Document) (changed : List String) (document : Document)
    (unaffected : affected changed document = false) :
    acceptedDocument visible changed document = true := by
  simp [acceptedDocument, unaffected]

/-- Preservation is conditional on unaffected visible documents remaining valid
in the candidate; invisible documents are outside this actor-scoped snapshot. -/
theorem accepted_preserves_visible_validity (visible : List Document) (changed : List String)
    (check : accepted visible changed = true)
    (preserved : ∀ document ∈ visible, affected changed document = false →
      valid visible document = true) :
    visible.all (valid visible) = true := by
  simp only [accepted, List.all_eq_true] at check
  simp only [List.all_eq_true]
  intro document member
  cases impact : affected changed document with
  | false => exact preserved document member impact
  | true => exact affected_document_must_validate visible changed document (check document member) impact

/-- Every visible referrer of a changed or removed key is affected. This covers
shared configuration: changing a target cannot bypass its visible consumers'
own validators, even if the consumer itself was not written. -/
theorem direct_referrer_is_affected (changed : List String) (document : Document)
    (key : String) (referenced : key ∈ document.required) (changedKey : key ∈ changed) :
    affected changed document = true := by
  simp only [affected, Bool.or_eq_true, List.any_eq_true, List.contains_iff_mem]
  exact Or.inr ⟨key, referenced, changedKey⟩

def missingSchedule : Document := ⟨"trigger", ["task", "schedule"], true⟩
def task : Document := ⟨"task", ["agent"], true⟩
def agent : Document := ⟨"agent", [], true⟩
def tools : Document := ⟨"tools", [], true⟩
def schedule : Document := ⟨"schedule", [], true⟩

theorem hidden_unrelated_schedule_does_not_block_tools_edit :
    accepted [missingSchedule, task, agent, tools] ["tools"] = true := by decide

theorem changed_trigger_must_find_its_schedule :
    accepted [missingSchedule, task, agent, tools] ["trigger"] = false := by decide

theorem changing_a_shared_task_validates_its_visible_trigger :
    accepted [missingSchedule, task, agent, tools] ["task"] = false := by decide

theorem deleting_a_referenced_schedule_is_rejected :
    accepted [missingSchedule, task, agent, tools] ["schedule"] = false := by decide

theorem visible_required_schedule_allows_the_trigger :
    accepted [missingSchedule, task, agent, tools, schedule] ["trigger"] = true := by decide

end Configuration.Affected
