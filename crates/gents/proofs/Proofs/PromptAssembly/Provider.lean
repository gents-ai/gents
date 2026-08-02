import Proofs.PromptAssembly.Content
import Proofs.PromptAssembly.Properties

/-!
# The provider-bound sanitizer, content and all

`Proofs.PromptAssembly.Executable.sanitize` models the inner two stages of the
Rust provider sanitizer:

```
sanitize = dropUnpairedCalls ∘ dropOrphanedResults
```

Production (`sanitize_history_for_provider`, `src/compaction.rs`) runs THREE:

```
normalize_assistant_content_order ∘ drop_unpaired_tool_calls ∘ drop_orphaned_tool_results
```

This file closes that gap by lifting the row model to carry assistant content,
so the whole composition is modeled.

## The divergence this exposed

Enriching the model made a real Rust/Lean disagreement visible for the first
time. On an assistant message carrying **text plus a tool call that never
resolved**:

* Rust `drop_unpaired_tool_calls` filters the content list, keeps every
  non-call item unconditionally, and keeps the message when anything survives.
  The message stays, carrying its text.
* Lean `filterCallsBy` sees `callIds ∩ resolved = ∅` and drops the row whole.

Assistant-text-plus-tool-calls is the *common* production shape —
`AssistantTurnAccumulator::build_message` writes exactly that. The old pure-row
model could not express the case, which is why the social fence never caught
it.

**Rust is right**: dropping the row would silently delete assistant prose from
the provider-bound history. So `filterCallsByP` below adopts Rust's rule — keep
the row when non-call content survives, demoting its kind to `.ordinary` — and
soundness is re-proven against it. `sanitize` and its theorems are left
untouched; `project_sanitizeForProvider_eq_sanitize` relates the two.

## Empty messages

The same enrichment exposed a second divergence. Rust drops empty messages, and
does it asymmetrically: `drop_orphaned_tool_results` pushes a user message only
when content survives, while assistant messages ride through and are pruned by
`drop_unpaired_tool_calls`. The row-only model has no notion of an empty message
and kept every `.ordinary` row. `emptyUserRow` / `emptyAssistantRow` model the
two prunes, `NonDegenerate` names the invariant they establish, and the fixpoint
theorems take it as a hypothesis — an input still carrying an empty row is not a
fixpoint, because sanitizing it removes that row.

## Model boundary: call-occurrence multiplicity

`MessageKind.assistantToolCalls` carries a `Finset ToolCallId`, so the row model
cannot express the *same* call id appearing twice in one turn: two occurrences
and one collapse to the same row. `Coherent` inherits that blindness, equating a
content list's call *set* with `callIds`.

This is a genuine limit, not an oversight, and it is why the model did not catch
the duplicate-key defect fixed alongside this file: Rust paired through a
`HashSet`, so a turn announcing the same id twice was closed by a single result
while both calls survived — provider-invalid output from the function whose job
is to prevent exactly that. `drop_unpaired_tool_calls` now drops duplicate
occurrences within a turn, which restores the correspondence by making the set
abstraction *true* of production output rather than merely assumed.

Modeling multiplicity properly would mean replacing `Finset` in the shared
`Transcript.MessageKind`, which every pairing theorem in `Transcript` and
`PairingReconcile` is stated over. That is a larger change than this one. The
occurrence-level behaviour is fenced in Rust instead, by
`compaction::tests::duplicate_call_keys_in_one_turn_do_not_leave_a_dangling_call`
and `::call_key_reuse_across_turns_survives`.

## Model boundary: global vs per-turn resolution

`resolvedInP` is a single set over the whole transcript, while Rust scopes
resolution to the *active turn* (`resolved_keys_per_turn`). The two coincide
exactly under `UniqueCallIds` — the hypothesis of `sanitizeForProvider_sound`,
which forbids a call id announced by one turn from appearing anywhere in the
rest — so the model and production agree on every input the theorems speak
about, and `witnessesHaveUniqueCallIds` discharges that for every emitted
witness.

They diverge only when an id is *reused across turns*, which `UniqueCallIds`
excludes but arbitrary loaded history does not. A global set lets an earlier
turn's result resolve a later turn's reuse, stranding a dangling call — the
second defect review found. Rust must be correct without the precondition, so it
scopes per turn; the reused-id shape is fenced by
`compaction::tests::incomplete_second_turn_reusing_a_key_is_not_resolved_by_the_first`.
-/

namespace PromptAssembly.Provider

open Transcript (MessageRow MessageKind MessageRole)
open PromptAssembly.Content (Item)

/-- A provider-bound row: the abstract transcript row plus the content list
that sits inside the message. -/
structure ProviderRow where
  row : MessageRow
  content : List Item
  deriving DecidableEq

/-- Forget the content, recovering the row model the pairing theorems are
stated over. -/
def project (rows : List ProviderRow) : List MessageRow :=
  rows.map ProviderRow.row

@[simp] theorem project_nil : project [] = [] := rfl

@[simp] theorem project_cons (pr : ProviderRow) (rest : List ProviderRow) :
    project (pr :: rest) = pr.row :: project rest := rfl

/-- A row's kind announces exactly the tool calls its content carries. -/
def Coherent (pr : ProviderRow) : Prop :=
  match pr.row.kind with
  | .assistantToolCalls callIds => Content.callsOf pr.content = callIds
  | .ordinary => Content.callsOf pr.content = ∅
  | .toolResult _ _ => Content.callsOf pr.content = ∅

instance (pr : ProviderRow) : Decidable (Coherent pr) := by
  unfold Coherent
  cases pr.row.kind <;> infer_instance

/-- `UniqueCallIds` is the other premise of `sanitizeForProvider_sound`. Making
it decidable lets the contract witnesses discharge it by `decide`, so a witness
that reuses a call id fails the build rather than silently voiding the soundness
claim the emitted rows rest on. -/
instance decidableDisjointCallIds (s t : Finset ToolExecution.ToolCallId) :
    Decidable (Disjoint s t) :=
  decidable_of_iff (s ∩ t = ∅) Finset.disjoint_iff_inter_eq_empty.symm

instance decidableUniqueCallIds :
    (rows : List Transcript.MessageRow) → Decidable (UniqueCallIds rows)
  | [] => .isTrue trivial
  | row :: rest =>
    let _ := decidableUniqueCallIds rest
    match hk : row.kind with
    | .assistantToolCalls callIds =>
        decidable_of_iff
          (Disjoint callIds (callsIn rest) ∧ UniqueCallIds rest)
          (uniqueCallIds_cons_assistant row rest callIds hk).symm
    | .toolResult callId key =>
        decidable_of_iff (UniqueCallIds rest)
          (uniqueCallIds_cons_result row rest callId key hk).symm
    | .ordinary =>
        decidable_of_iff (UniqueCallIds rest)
          (uniqueCallIds_cons_ordinary row rest hk).symm

abbrev AllCoherent (rows : List ProviderRow) : Prop :=
  ∀ pr ∈ rows, Coherent pr

/-- A row whose message would be *empty* at the orphan stage.

Rust drops empty messages, and the two stages do it asymmetrically:
`drop_orphaned_tool_results` pushes a user message only when content survives
(so an empty user message goes there), while assistant messages are carried
forward unconditionally and pruned in `drop_unpaired_tool_calls`. A
`.toolResult` row denotes a user message carrying exactly one tool result, so it
is never empty in this sense — the orphan branch already removes it when the
result is unpaired. -/
def emptyUserRow (pr : ProviderRow) : Bool :=
  match pr.row.role with
  | .user => pr.content.isEmpty
  | .assistant => false

/-- An assistant row carrying nothing. A coherent `.ordinary` row announces no
calls, so the content filter is the identity on it and emptiness is decided by
the content alone. -/
def emptyAssistantRow (pr : ProviderRow) : Bool :=
  match pr.row.role with
  | .assistant => pr.content.isEmpty
  | .user => false

/-- A row that denotes a non-empty message. Rust drops empty messages, so this
is what the fixpoint theorems need: an input that still carries empty rows is
not a fixpoint of the sanitizer, because sanitizing it removes them.

`.toolResult` rows are always non-degenerate — the row denotes a user message
carrying exactly one tool result, which is its content. -/
def NonDegenerate (pr : ProviderRow) : Prop :=
  match pr.row.kind with
  | .toolResult _ _ => True
  | _ => pr.content ≠ []

instance (pr : ProviderRow) : Decidable (NonDegenerate pr) := by
  unfold NonDegenerate
  cases pr.row.kind <;> infer_instance

abbrev AllNonDegenerate (rows : List ProviderRow) : Prop :=
  ∀ pr ∈ rows, NonDegenerate pr

theorem not_emptyUserRow_of_nonDegenerate {pr : ProviderRow}
    (h : NonDegenerate pr) (hk : pr.row.kind = .ordinary) : emptyUserRow pr = false := by
  unfold NonDegenerate at h
  rw [hk] at h
  unfold emptyUserRow
  cases pr.row.role with
  | user => simpa using h
  | assistant => rfl

theorem not_emptyAssistantRow_of_nonDegenerate {pr : ProviderRow}
    (h : NonDegenerate pr) (hk : pr.row.kind = .ordinary) :
    emptyAssistantRow pr = false := by
  unfold NonDegenerate at h
  rw [hk] at h
  unfold emptyAssistantRow
  cases pr.row.role with
  | user => rfl
  | assistant => simpa using h

/-! ## Stage 3 — content ordering -/

def normalizeRow (pr : ProviderRow) : ProviderRow :=
  { pr with content := Content.normalize pr.content }

@[simp] theorem normalizeRow_row (pr : ProviderRow) :
    (normalizeRow pr).row = pr.row := rfl

def normalizeOrder (rows : List ProviderRow) : List ProviderRow :=
  rows.map normalizeRow

@[simp] theorem normalizeOrder_nil : normalizeOrder [] = [] := rfl

@[simp] theorem normalizeOrder_cons (pr : ProviderRow) (rest : List ProviderRow) :
    normalizeOrder (pr :: rest) = normalizeRow pr :: normalizeOrder rest := rfl

/-- Ordering is invisible to the row abstraction. -/
@[simp] theorem project_normalizeOrder (rows : List ProviderRow) :
    project (normalizeOrder rows) = project rows := by
  induction rows with
  | nil => rfl
  | cons pr rest ih => simpa using ih

theorem coherent_normalizeRow {pr : ProviderRow} (h : Coherent pr) :
    Coherent (normalizeRow pr) := by
  unfold Coherent at h ⊢
  simp only [normalizeRow_row]
  cases hk : pr.row.kind <;>
    · rw [hk] at h
      simpa [normalizeRow, Content.callsOf_normalize] using h

theorem allCoherent_normalizeOrder {rows : List ProviderRow}
    (h : AllCoherent rows) : AllCoherent (normalizeOrder rows) := by
  intro pr hpr
  obtain ⟨source, hsource, rfl⟩ := List.mem_map.mp hpr
  exact coherent_normalizeRow (h source hsource)

theorem normalizeOrder_idempotent (rows : List ProviderRow) :
    normalizeOrder (normalizeOrder rows) = normalizeOrder rows := by
  induction rows with
  | nil => rfl
  | cons pr rest ih =>
    simp only [normalizeOrder_cons, ih, List.cons.injEq, and_true]
    simp [normalizeRow, Content.normalize_idempotent]

/-! ## Stage 1 — orphaned tool results

Structurally identical to `dropOrphanedFrom`; content rides along untouched,
because a tool-result row carries no assistant content and an assistant row's
content is not filtered by this stage. -/

def dropOrphanedFromP (pending : Finset ToolExecution.ToolCallId) :
    List ProviderRow → List ProviderRow
  | [] => []
  | pr :: rest =>
    match pr.row.kind with
    | .toolResult callId _ =>
        if callId ∈ pending then pr :: dropOrphanedFromP (pending.erase callId) rest
        else dropOrphanedFromP pending rest
    | .assistantToolCalls callIds => pr :: dropOrphanedFromP callIds rest
    | .ordinary =>
        -- An empty user message does *not* end the active tool-call turn: Rust
        -- clears `pending_calls` only when the message carries plain content,
        -- and an empty message carries none. Recursing with `∅` here would
        -- strand a valid call/result pair that merely had an empty message
        -- between them. A *non-empty* ordinary row does end the turn.
        if emptyUserRow pr then dropOrphanedFromP pending rest
        else pr :: dropOrphanedFromP ∅ rest

def dropOrphanedResultsP (rows : List ProviderRow) : List ProviderRow :=
  dropOrphanedFromP ∅ rows

section DropOrphanedReduction

variable (pr : ProviderRow) (rest : List ProviderRow)
variable (pending callIds : Finset ToolExecution.ToolCallId)
variable (callId : ToolExecution.ToolCallId) (key : Transcript.ToolResultKey)

@[simp] theorem dropOrphanedFromP_nil : dropOrphanedFromP pending [] = [] := rfl

theorem dropOrphanedFromP_cons_result (h : pr.row.kind = .toolResult callId key) :
    dropOrphanedFromP pending (pr :: rest) =
      if callId ∈ pending then pr :: dropOrphanedFromP (pending.erase callId) rest
      else dropOrphanedFromP pending rest := by
  simp only [dropOrphanedFromP, h]

theorem dropOrphanedFromP_cons_assistant
    (h : pr.row.kind = .assistantToolCalls callIds) :
    dropOrphanedFromP pending (pr :: rest) =
      pr :: dropOrphanedFromP callIds rest := by
  simp only [dropOrphanedFromP, h]

theorem dropOrphanedFromP_cons_ordinary (h : pr.row.kind = .ordinary) :
    dropOrphanedFromP pending (pr :: rest) =
      if emptyUserRow pr then dropOrphanedFromP pending rest
      else pr :: dropOrphanedFromP ∅ rest := by
  simp only [dropOrphanedFromP, h]

end DropOrphanedReduction

/-- Stage 1 commutes with projection **on non-degenerate rows**.

It does not commute in general: this stage drops empty user messages (Rust
`drop_orphaned_tool_results` pushes a user message only when content survives)
while the row-only `dropOrphanedFrom` has no notion of an empty message and
keeps every `.ordinary` row. Soundness below therefore does not use this lemma;
it uses the two set-level facts that follow, which hold unconditionally because
an empty row carries neither calls nor results. -/
theorem project_dropOrphanedFromP (rows : List ProviderRow)
    (hnd : AllNonDegenerate rows) :
    ∀ pending,
      project (dropOrphanedFromP pending rows) =
        dropOrphanedFrom pending (project rows) := by
  induction rows with
  | nil => intro pending; rfl
  | cons pr rest ih =>
    intro pending
    have hhead : NonDegenerate pr := hnd pr (List.mem_cons_self _ _)
    have hrest : AllNonDegenerate rest := fun r hr => hnd r (List.mem_cons_of_mem _ hr)
    cases hk : pr.row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFromP_cons_result pr rest pending callId key hk,
        project_cons,
        dropOrphanedFrom_cons_result pr.row (project rest) pending callId key hk]
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem, if_pos hmem, project_cons, ih hrest (pending.erase callId)]
      · rw [if_neg hmem, if_neg hmem, ih hrest pending]
    | assistantToolCalls callIds =>
      rw [dropOrphanedFromP_cons_assistant pr rest pending callIds hk,
        project_cons, project_cons,
        dropOrphanedFrom_cons_assistant pr.row (project rest) pending callIds hk,
        ih hrest callIds]
    | ordinary =>
      rw [dropOrphanedFromP_cons_ordinary pr rest pending hk,
        if_neg (by simp [not_emptyUserRow_of_nonDegenerate hhead hk]),
        project_cons, project_cons,
        dropOrphanedFrom_cons_ordinary pr.row (project rest) pending hk, ih hrest ∅]

theorem project_dropOrphanedResultsP (rows : List ProviderRow)
    (hnd : AllNonDegenerate rows) :
    project (dropOrphanedResultsP rows) = dropOrphanedResults (project rows) :=
  project_dropOrphanedFromP rows hnd ∅

/-- Dropping empty rows removes no announcements: they carry no calls. -/
theorem callsIn_project_dropOrphanedFromP (rows : List ProviderRow) :
    ∀ pending,
      callsIn (project (dropOrphanedFromP pending rows)) = callsIn (project rows) := by
  induction rows with
  | nil => intro pending; rfl
  | cons pr rest ih =>
    intro pending
    cases hk : pr.row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFromP_cons_result pr rest pending callId key hk, project_cons,
        callsIn_cons_result pr.row (project rest) callId key hk]
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem, project_cons,
          callsIn_cons_result pr.row _ callId key hk, ih (pending.erase callId)]
      · rw [if_neg hmem, ih pending]
    | assistantToolCalls callIds =>
      rw [dropOrphanedFromP_cons_assistant pr rest pending callIds hk, project_cons,
        project_cons, callsIn_cons_assistant pr.row _ callIds hk,
        callsIn_cons_assistant pr.row (project rest) callIds hk, ih callIds]
    | ordinary =>
      rw [dropOrphanedFromP_cons_ordinary pr rest pending hk, project_cons,
        callsIn_cons_ordinary pr.row (project rest) hk]
      by_cases hempty : emptyUserRow pr
      · rw [if_pos hempty, ih pending]
      · rw [if_neg hempty, project_cons, callsIn_cons_ordinary pr.row _ hk, ih ∅]

/-- Dropping empty rows resolves nothing new: they carry no results. -/
theorem resolvedIn_project_dropOrphanedFromP_subset (rows : List ProviderRow) :
    ∀ pending,
      resolvedIn (project (dropOrphanedFromP pending rows))
        ⊆ pending ∪ callsIn (project rows) := by
  induction rows with
  | nil => intro pending c hc; simp at hc
  | cons pr rest ih =>
    intro pending c hc
    cases hk : pr.row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFromP_cons_result pr rest pending callId key hk] at hc
      rw [project_cons, callsIn_cons_result pr.row (project rest) callId key hk]
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem, project_cons,
          resolvedIn_cons_result pr.row _ callId key hk] at hc
        rcases Finset.mem_insert.mp hc with rfl | hcTail
        · exact Finset.mem_union_left _ hmem
        · exact (Finset.mem_union.mp ((ih (pending.erase callId)) hcTail)).elim
            (fun hp => Finset.mem_union_left _ (Finset.mem_of_mem_erase hp))
            (fun hcall => Finset.mem_union_right _ hcall)
      · rw [if_neg hmem] at hc
        exact (ih pending) hc
    | assistantToolCalls callIds =>
      rw [dropOrphanedFromP_cons_assistant pr rest pending callIds hk, project_cons,
        resolvedIn_cons_assistant pr.row _ callIds hk] at hc
      rw [project_cons, callsIn_cons_assistant pr.row (project rest) callIds hk]
      exact (Finset.mem_union.mp ((ih callIds) hc)).elim
        (fun hp => Finset.mem_union_right _ (Finset.mem_union_left _ hp))
        (fun hcall => Finset.mem_union_right _ (Finset.mem_union_right _ hcall))
    | ordinary =>
      rw [dropOrphanedFromP_cons_ordinary pr rest pending hk] at hc
      rw [project_cons, callsIn_cons_ordinary pr.row (project rest) hk]
      by_cases hempty : emptyUserRow pr
      · -- The turn stays open across an empty message, so `pending` survives
        -- and the bound is the induction hypothesis at the same `pending`.
        rw [if_pos hempty] at hc
        exact (ih pending) hc
      · rw [if_neg hempty, project_cons,
          resolvedIn_cons_ordinary pr.row _ hk] at hc
        exact Finset.mem_union_right _
          ((Finset.mem_union.mp ((ih ∅) hc)).elim (by intro h; simp at h) id)

theorem allCoherent_dropOrphanedFromP {rows : List ProviderRow}
    (h : AllCoherent rows) : ∀ pending, AllCoherent (dropOrphanedFromP pending rows) := by
  induction rows with
  | nil => intro pending pr hpr; simp at hpr
  | cons row rest ih =>
    intro pending pr hpr
    have hhead : Coherent row := h row (List.mem_cons_self _ _)
    have hrest : AllCoherent rest := fun r hr => h r (List.mem_cons_of_mem _ hr)
    cases hk : row.row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFromP_cons_result row rest pending callId key hk] at hpr
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · exact hhead
        · exact ih hrest (pending.erase callId) pr htail
      · rw [if_neg hmem] at hpr
        exact ih hrest pending pr hpr
    | assistantToolCalls callIds =>
      rw [dropOrphanedFromP_cons_assistant row rest pending callIds hk] at hpr
      rcases List.mem_cons.mp hpr with rfl | htail
      · exact hhead
      · exact ih hrest callIds pr htail
    | ordinary =>
      rw [dropOrphanedFromP_cons_ordinary row rest pending hk] at hpr
      by_cases hempty : emptyUserRow row
      · rw [if_pos hempty] at hpr
        exact ih hrest pending pr hpr
      · rw [if_neg hempty] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · exact hhead
        · exact ih hrest ∅ pr htail

/-! ## Stage 2 — unpaired tool calls

This is where the model follows Rust rather than the other way round. -/

/-- Keep every non-call item; keep a call only when it resolved. Mirrors the
closure in Rust `drop_unpaired_tool_calls`. -/
def keepItem (resolved : Finset ToolExecution.ToolCallId) : Item → Bool
  | .call callId => decide (callId ∈ resolved)
  | _ => true

def restrictContent (resolved : Finset ToolExecution.ToolCallId)
    (items : List Item) : List Item :=
  items.filter (keepItem resolved)

theorem callsOf_restrictContent (resolved : Finset ToolExecution.ToolCallId)
    (items : List Item) :
    Content.callsOf (restrictContent resolved items) =
      Content.callsOf items ∩ resolved := by
  induction items with
  | nil => simp [restrictContent, Content.callsOf]
  | cons item rest ih =>
    cases item with
    | text index => simpa [restrictContent, keepItem] using ih
    | other index => simpa [restrictContent, keepItem] using ih
    | call callId =>
      by_cases hmem : callId ∈ resolved
      · have hkeep : restrictContent resolved (Item.call callId :: rest)
            = Item.call callId :: restrictContent resolved rest := by
          simp [restrictContent, keepItem, hmem]
        rw [hkeep, Content.callsOf_cons_call, ih, Content.callsOf_cons_call,
          Finset.insert_inter_of_mem hmem]
      · have hdrop : restrictContent resolved (Item.call callId :: rest)
            = restrictContent resolved rest := by
          simp [restrictContent, keepItem, hmem]
        rw [hdrop, ih, Content.callsOf_cons_call,
          Finset.insert_inter_of_not_mem hmem]

/-- The kind an assistant row takes once its unresolved calls are filtered out:
`.ordinary` when nothing is left to announce. -/
def restrictedKind (resolved callIds : Finset ToolExecution.ToolCallId) : MessageKind :=
  if callIds ∩ resolved = ∅ then .ordinary
  else .assistantToolCalls (callIds ∩ resolved)

def restrictRow (resolved : Finset ToolExecution.ToolCallId)
    (pr : ProviderRow) (callIds : Finset ToolExecution.ToolCallId) : ProviderRow :=
  { row := { pr.row with kind := restrictedKind resolved callIds }
  , content := restrictContent resolved pr.content }

/-- Mirrors Rust `drop_unpaired_tool_calls`: an assistant row survives exactly
when content survives the filter, and its kind is demoted to `.ordinary` when
no announced call resolved. -/
def filterCallsByP (resolved : Finset ToolExecution.ToolCallId) :
    List ProviderRow → List ProviderRow
  | [] => []
  | pr :: rest =>
    match pr.row.kind with
    | .assistantToolCalls callIds =>
        if restrictContent resolved pr.content = [] then filterCallsByP resolved rest
        else restrictRow resolved pr callIds :: filterCallsByP resolved rest
    | .ordinary =>
        -- Rust prunes an assistant message here when nothing survives the
        -- content filter, including one that never announced a call. User rows
        -- were already pruned in stage 1.
        if emptyAssistantRow pr then filterCallsByP resolved rest
        else pr :: filterCallsByP resolved rest
    | .toolResult _ _ => pr :: filterCallsByP resolved rest

def resolvedInP (rows : List ProviderRow) : Finset ToolExecution.ToolCallId :=
  resolvedIn (project rows)

def dropUnpairedCallsP (rows : List ProviderRow) : List ProviderRow :=
  filterCallsByP (resolvedInP rows) rows

/-- The full production composition. -/
def sanitizeForProvider (rows : List ProviderRow) : List ProviderRow :=
  normalizeOrder (dropUnpairedCallsP (dropOrphanedResultsP rows))

section FilterReduction

variable (pr : ProviderRow) (rest : List ProviderRow)
variable (resolved callIds : Finset ToolExecution.ToolCallId)
variable (callId : ToolExecution.ToolCallId) (key : Transcript.ToolResultKey)

@[simp] theorem filterCallsByP_nil : filterCallsByP resolved [] = [] := rfl

theorem filterCallsByP_cons_assistant
    (h : pr.row.kind = .assistantToolCalls callIds) :
    filterCallsByP resolved (pr :: rest) =
      if restrictContent resolved pr.content = [] then filterCallsByP resolved rest
      else restrictRow resolved pr callIds :: filterCallsByP resolved rest := by
  simp only [filterCallsByP, h]

theorem filterCallsByP_cons_result (h : pr.row.kind = .toolResult callId key) :
    filterCallsByP resolved (pr :: rest) = pr :: filterCallsByP resolved rest := by
  simp only [filterCallsByP, h]

theorem filterCallsByP_cons_ordinary (h : pr.row.kind = .ordinary) :
    filterCallsByP resolved (pr :: rest) =
      if emptyAssistantRow pr then filterCallsByP resolved rest
      else pr :: filterCallsByP resolved rest := by
  simp only [filterCallsByP, h]

end FilterReduction

/-! ## Coherence is preserved -/

theorem coherent_restrictRow {pr : ProviderRow}
    {callIds : Finset ToolExecution.ToolCallId}
    (resolved : Finset ToolExecution.ToolCallId)
    (hcoh : Coherent pr) (hk : pr.row.kind = .assistantToolCalls callIds) :
    Coherent (restrictRow resolved pr callIds) := by
  have hcontent : Content.callsOf pr.content = callIds := by
    unfold Coherent at hcoh; rwa [hk] at hcoh
  have hrestrict :
      Content.callsOf (restrictContent resolved pr.content) = callIds ∩ resolved := by
    rw [callsOf_restrictContent, hcontent]
  unfold Coherent restrictRow restrictedKind
  by_cases hempty : callIds ∩ resolved = ∅
  · simp only [hempty, if_pos]
    simpa [hempty] using hrestrict
  · simp only [hempty, if_neg, not_false_iff]
    simpa using hrestrict

/-- An assistant row whose filtered content is empty announced nothing that
resolved. This is what makes the content-driven drop agree with the
call-set-driven drop of the row model. -/
theorem inter_eq_empty_of_restrictContent_nil {pr : ProviderRow}
    {callIds resolved : Finset ToolExecution.ToolCallId}
    (hcoh : Coherent pr) (hk : pr.row.kind = .assistantToolCalls callIds)
    (hnil : restrictContent resolved pr.content = []) :
    callIds ∩ resolved = ∅ := by
  have hcontent : Content.callsOf pr.content = callIds := by
    unfold Coherent at hcoh; rwa [hk] at hcoh
  have := callsOf_restrictContent resolved pr.content
  rw [hnil, hcontent] at this
  simpa using this.symm

theorem allCoherent_filterCallsByP {rows : List ProviderRow}
    (resolved : Finset ToolExecution.ToolCallId) (h : AllCoherent rows) :
    AllCoherent (filterCallsByP resolved rows) := by
  induction rows with
  | nil => intro pr hpr; simp at hpr
  | cons row rest ih =>
    have hhead : Coherent row := h row (List.mem_cons_self _ _)
    have hrest : AllCoherent rest := fun r hr => h r (List.mem_cons_of_mem _ hr)
    intro pr hpr
    cases hk : row.row.kind with
    | assistantToolCalls callIds =>
      rw [filterCallsByP_cons_assistant row rest resolved callIds hk] at hpr
      by_cases hnil : restrictContent resolved row.content = []
      · rw [if_pos hnil] at hpr
        exact ih hrest pr hpr
      · rw [if_neg hnil] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · exact coherent_restrictRow resolved hhead hk
        · exact ih hrest pr htail
    | toolResult callId key =>
      rw [filterCallsByP_cons_result row rest resolved callId key hk] at hpr
      rcases List.mem_cons.mp hpr with rfl | htail
      · exact hhead
      · exact ih hrest pr htail
    | ordinary =>
      rw [filterCallsByP_cons_ordinary row rest resolved hk] at hpr
      by_cases hempty : emptyAssistantRow row
      · rw [if_pos hempty] at hpr
        exact ih hrest pr hpr
      · rw [if_neg hempty] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · exact hhead
        · exact ih hrest pr htail

/-! ## Irrelevance: resolved ids a list never announces cannot change the filter -/

theorem restrictContent_union_of_disjoint
    (extra resolved : Finset ToolExecution.ToolCallId) (items : List Item)
    (h : Disjoint extra (Content.callsOf items)) :
    restrictContent (extra ∪ resolved) items = restrictContent resolved items := by
  induction items with
  | nil => rfl
  | cons item rest ih =>
    cases item with
    | text index =>
      have hrest : Disjoint extra (Content.callsOf rest) := by simpa using h
      simpa [restrictContent, keepItem] using ih hrest
    | other index =>
      have hrest : Disjoint extra (Content.callsOf rest) := by simpa using h
      simpa [restrictContent, keepItem] using ih hrest
    | call callId =>
      have hmemCalls : callId ∈ Content.callsOf (Item.call callId :: rest) := by
        simp
      have hnotExtra : callId ∉ extra := fun hc =>
        Finset.disjoint_left.mp h hc hmemCalls
      have hrest : Disjoint extra (Content.callsOf rest) := by
        refine Finset.disjoint_left.mpr fun c hc hcall => ?_
        exact Finset.disjoint_left.mp h hc
          (by simp only [Content.callsOf_cons_call]; exact Finset.mem_insert_of_mem hcall)
      have hmem : (callId ∈ extra ∪ resolved) = (callId ∈ resolved) := by
        simp [hnotExtra]
      simp only [restrictContent, List.filter_cons, keepItem, hmem]
      rw [show List.filter (keepItem (extra ∪ resolved)) rest
            = List.filter (keepItem resolved) rest from ih hrest]

/-- Call ids a row announces are call ids the projected list announces. -/
theorem callsOf_subset_callsIn {rows : List ProviderRow} (hcoh : AllCoherent rows)
    {pr : ProviderRow} (hpr : pr ∈ rows) :
    Content.callsOf pr.content ⊆ callsIn (project rows) := by
  induction rows with
  | nil => simp at hpr
  | cons row rest ih =>
    have hhead : Coherent row := hcoh row (List.mem_cons_self _ _)
    have hrest : AllCoherent rest := fun r hr => hcoh r (List.mem_cons_of_mem _ hr)
    rcases List.mem_cons.mp hpr with rfl | htail
    · cases hk : pr.row.kind with
      | assistantToolCalls callIds =>
        have hcontent : Content.callsOf pr.content = callIds := by
          unfold Coherent at hhead; rwa [hk] at hhead
        rw [hcontent, project_cons, callsIn_cons_assistant pr.row (project rest) callIds hk]
        exact Finset.subset_union_left
      | toolResult callId key =>
        have hcontent : Content.callsOf pr.content = ∅ := by
          unfold Coherent at hhead; rwa [hk] at hhead
        rw [hcontent]; exact Finset.empty_subset _
      | ordinary =>
        have hcontent : Content.callsOf pr.content = ∅ := by
          unfold Coherent at hhead; rwa [hk] at hhead
        rw [hcontent]; exact Finset.empty_subset _
    · refine (ih hrest htail).trans ?_
      rw [project_cons]
      cases hk : row.row.kind with
      | assistantToolCalls callIds =>
        rw [callsIn_cons_assistant row.row (project rest) callIds hk]
        exact Finset.subset_union_right
      | toolResult callId key =>
        rw [callsIn_cons_result row.row (project rest) callId key hk]
      | ordinary =>
        rw [callsIn_cons_ordinary row.row (project rest) hk]

theorem filterCallsByP_irrelevant (rows : List ProviderRow) :
    ∀ extra resolved, AllCoherent rows →
      Disjoint extra (callsIn (project rows)) →
      filterCallsByP (extra ∪ resolved) rows = filterCallsByP resolved rows := by
  induction rows with
  | nil => intro extra resolved _ _; rfl
  | cons row rest ih =>
    intro extra resolved hcoh hdisj
    have hhead : Coherent row := hcoh row (List.mem_cons_self _ _)
    have hrest : AllCoherent rest := fun r hr => hcoh r (List.mem_cons_of_mem _ hr)
    have hdisjHead : Disjoint extra (Content.callsOf row.content) :=
      hdisj.mono_right (callsOf_subset_callsIn hcoh (List.mem_cons_self _ _))
    cases hk : row.row.kind with
    | assistantToolCalls callIds =>
      have hdisjRest : Disjoint extra (callsIn (project rest)) := by
        rw [project_cons, callsIn_cons_assistant row.row (project rest) callIds hk,
          Finset.disjoint_union_right] at hdisj
        exact hdisj.2
      have hcontent :
          restrictContent (extra ∪ resolved) row.content
            = restrictContent resolved row.content :=
        restrictContent_union_of_disjoint extra resolved row.content hdisjHead
      have hkindInter : callIds ∩ (extra ∪ resolved) = callIds ∩ resolved := by
        have hcallIds : Content.callsOf row.content = callIds := by
          unfold Coherent at hhead; rwa [hk] at hhead
        have : Disjoint extra callIds := by rwa [hcallIds] at hdisjHead
        rw [Finset.inter_union_distrib_left,
          Finset.disjoint_iff_inter_eq_empty.mp this.symm, Finset.empty_union]
      rw [filterCallsByP_cons_assistant row rest (extra ∪ resolved) callIds hk,
        filterCallsByP_cons_assistant row rest resolved callIds hk,
        hcontent, ih extra resolved hrest hdisjRest]
      by_cases hnil : restrictContent resolved row.content = []
      · rw [if_pos hnil, if_pos hnil]
      · rw [if_neg hnil, if_neg hnil]
        simp only [restrictRow, restrictedKind, hcontent, hkindInter]
    | toolResult callId key =>
      have hdisjRest : Disjoint extra (callsIn (project rest)) := by
        rw [project_cons, callsIn_cons_result row.row (project rest) callId key hk] at hdisj
        exact hdisj
      rw [filterCallsByP_cons_result row rest (extra ∪ resolved) callId key hk,
        filterCallsByP_cons_result row rest resolved callId key hk,
        ih extra resolved hrest hdisjRest]
    | ordinary =>
      have hdisjRest : Disjoint extra (callsIn (project rest)) := by
        rw [project_cons, callsIn_cons_ordinary row.row (project rest) hk] at hdisj
        exact hdisj
      rw [filterCallsByP_cons_ordinary row rest (extra ∪ resolved) hk,
        filterCallsByP_cons_ordinary row rest resolved hk,
        ih extra resolved hrest hdisjRest]

/-! ## Soundness

Mirrors `activeBlockValid_filterOrphanedFrom`. The one case that differs is the
assistant row whose announced calls all went unresolved: the row model drops it,
this model keeps it as `.ordinary` whenever non-call content survives. Both are
sound, and for the same reason — at that position the pending set is already
empty (`hstart`), which is exactly the `.ordinary` clause's obligation. -/

theorem activeBlockValidFrom_filterCallsByP (rows : List ProviderRow) :
    ∀ pending,
      UniqueCallIds (project rows) →
      Disjoint pending (callsIn (project rows)) →
      AllCoherent rows →
      ActiveBlockValidFrom
        (pending ∩ resolvedIn (project (dropOrphanedFromP pending rows)))
        (project
          (filterCallsByP (resolvedIn (project (dropOrphanedFromP pending rows)))
            (dropOrphanedFromP pending rows))) := by
  induction rows with
  | nil =>
    intro pending _ _ _
    simp [ActiveBlockValidFrom]
  | cons row rest ih =>
    intro pending huniq hdisj hcoh
    have hhead : Coherent row := hcoh row (List.mem_cons_self _ _)
    have hrestCoh : AllCoherent rest := fun r hr => hcoh r (List.mem_cons_of_mem _ hr)
    rw [project_cons] at huniq hdisj
    cases hk : row.row.kind with
    | toolResult callId key =>
      have huniq' : UniqueCallIds (project rest) :=
        (uniqueCallIds_cons_result row.row (project rest) callId key hk).mp huniq
      have hdisj' : Disjoint pending (callsIn (project rest)) := by
        rwa [callsIn_cons_result row.row (project rest) callId key hk] at hdisj
      rw [dropOrphanedFromP_cons_result row rest pending callId key hk]
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem]
        have hnotCall : callId ∉ callsIn (project rest) := fun hcall =>
          Finset.disjoint_left.mp hdisj' hmem hcall
        have htailCoh : AllCoherent (dropOrphanedFromP (pending.erase callId) rest) :=
          allCoherent_dropOrphanedFromP hrestCoh _
        have hcallsTail :
            callsIn (project (dropOrphanedFromP (pending.erase callId) rest))
              = callsIn (project rest) :=
          callsIn_project_dropOrphanedFromP rest (pending.erase callId)
        have hirrel :
            Disjoint {callId}
              (callsIn (project (dropOrphanedFromP (pending.erase callId) rest))) := by
          rw [hcallsTail]
          exact Finset.disjoint_singleton_left.mpr hnotCall
        have hfilter :
            filterCallsByP
                (insert callId
                  (resolvedIn (project (dropOrphanedFromP (pending.erase callId) rest))))
                (dropOrphanedFromP (pending.erase callId) rest)
              = filterCallsByP
                  (resolvedIn (project (dropOrphanedFromP (pending.erase callId) rest)))
                  (dropOrphanedFromP (pending.erase callId) rest) := by
          simpa using
            filterCallsByP_irrelevant (dropOrphanedFromP (pending.erase callId) rest)
              {callId}
              (resolvedIn (project (dropOrphanedFromP (pending.erase callId) rest)))
              htailCoh hirrel
        rw [project_cons,
          resolvedIn_cons_result row.row
            (project (dropOrphanedFromP (pending.erase callId) rest)) callId key hk,
          filterCallsByP_cons_result row
            (dropOrphanedFromP (pending.erase callId) rest) _ callId key hk,
          project_cons,
          activeBlockValidFrom_cons_result row.row _ _ callId key hk]
        refine ⟨Finset.mem_inter.mpr ⟨hmem, Finset.mem_insert_self _ _⟩, ?_⟩
        rw [hfilter, erase_inter_insert_eq pending _ callId]
        exact ih (pending.erase callId) huniq' (by
          rw [Finset.disjoint_left]
          intro c hc hcall
          exact Finset.disjoint_left.mp hdisj' (Finset.mem_of_mem_erase hc) hcall)
          hrestCoh
      · rw [if_neg hmem]
        exact ih pending huniq' hdisj' hrestCoh
    | assistantToolCalls callIds =>
      have huniqPair :=
        (uniqueCallIds_cons_assistant row.row (project rest) callIds hk).mp huniq
      have hdisjPair : Disjoint pending callIds ∧ Disjoint pending (callsIn (project rest)) := by
        rw [callsIn_cons_assistant row.row (project rest) callIds hk,
          Finset.disjoint_union_right] at hdisj
        exact hdisj
      rw [dropOrphanedFromP_cons_assistant row rest pending callIds hk]
      have hRsub :
          resolvedIn (project (dropOrphanedFromP callIds rest))
            ⊆ callIds ∪ callsIn (project rest) :=
        resolvedIn_project_dropOrphanedFromP_subset rest callIds
      have hstart : pending ∩ resolvedIn (project (dropOrphanedFromP callIds rest)) = ∅ :=
        Finset.disjoint_iff_inter_eq_empty.mp
          ((Finset.disjoint_union_right.mpr ⟨hdisjPair.1, hdisjPair.2⟩).mono_right hRsub)
      rw [project_cons,
        resolvedIn_cons_assistant row.row
          (project (dropOrphanedFromP callIds rest)) callIds hk,
        filterCallsByP_cons_assistant row (dropOrphanedFromP callIds rest) _ callIds hk]
      have htail := ih callIds huniqPair.2 huniqPair.1 hrestCoh
      by_cases hnil :
          restrictContent (resolvedIn (project (dropOrphanedFromP callIds rest)))
            row.content = []
      · -- No announced call resolved AND nothing else survives: the row goes,
        -- exactly as the row model has it.
        have hempty :
            callIds ∩ resolvedIn (project (dropOrphanedFromP callIds rest)) = ∅ :=
          inter_eq_empty_of_restrictContent_nil hhead hk hnil
        rw [if_pos hnil, hstart]
        rwa [hempty] at htail
      · rw [if_neg hnil]
        by_cases hempty :
            callIds ∩ resolvedIn (project (dropOrphanedFromP callIds rest)) = ∅
        · -- Nothing announced resolved, but assistant prose survives. Rust keeps
          -- the message; the row is demoted to `.ordinary`. Sound because the
          -- pending set is already empty here (`hstart`).
          have hkindOrd :
              (restrictRow (resolvedIn (project (dropOrphanedFromP callIds rest)))
                row callIds).row.kind = .ordinary := by
            simp [restrictRow, restrictedKind, hempty]
          rw [project_cons, activeBlockValidFrom_cons_ordinary _ _ _ hkindOrd]
          refine ⟨hstart, ?_⟩
          rwa [hempty] at htail
        · have hkindCalls :
              (restrictRow (resolvedIn (project (dropOrphanedFromP callIds rest)))
                row callIds).row.kind
                = .assistantToolCalls
                    (callIds ∩ resolvedIn (project (dropOrphanedFromP callIds rest))) := by
            simp [restrictRow, restrictedKind, hempty]
          rw [project_cons, activeBlockValidFrom_cons_assistant _ _ _ _ hkindCalls]
          exact ⟨hstart, htail⟩
    | ordinary =>
      have huniq' : UniqueCallIds (project rest) :=
        (uniqueCallIds_cons_ordinary row.row (project rest) hk).mp huniq
      have hdisj' : Disjoint pending (callsIn (project rest)) := by
        rwa [callsIn_cons_ordinary row.row (project rest) hk] at hdisj
      rw [dropOrphanedFromP_cons_ordinary row rest pending hk]
      have hRsub : resolvedIn (project (dropOrphanedFromP ∅ rest)) ⊆ callsIn (project rest) := by
        simpa using resolvedIn_project_dropOrphanedFromP_subset rest ∅
      have hstart : pending ∩ resolvedIn (project (dropOrphanedFromP ∅ rest)) = ∅ :=
        Finset.disjoint_iff_inter_eq_empty.mp (hdisj'.mono_right hRsub)
      have htail := ih ∅ huniq' (Finset.disjoint_empty_left _) hrestCoh
      by_cases hemptyUser : emptyUserRow row
      · -- An empty user message does not end the turn, so the recursion carries
        -- `pending` through unchanged and the goal *is* the induction
        -- hypothesis at this `pending`.
        rw [if_pos hemptyUser]
        exact ih pending huniq' hdisj' hrestCoh
      · rw [if_neg hemptyUser, project_cons,
          resolvedIn_cons_ordinary row.row (project (dropOrphanedFromP ∅ rest)) hk,
          filterCallsByP_cons_ordinary row (dropOrphanedFromP ∅ rest) _ hk]
        by_cases hemptyAssistant : emptyAssistantRow row
        · -- An empty assistant message: carried through stage 1, pruned here.
          rw [if_pos hemptyAssistant, hstart]
          simpa using htail
        · rw [if_neg hemptyAssistant, project_cons,
            activeBlockValidFrom_cons_ordinary row.row _ _ hk]
          exact ⟨hstart, htail⟩

/-- **Soundness of the full three-stage production sanitizer.** -/
theorem sanitizeForProvider_sound {rows : List ProviderRow}
    (huniq : UniqueCallIds (project rows)) (hcoh : AllCoherent rows) :
    ProviderValid (project (sanitizeForProvider rows)) := by
  unfold sanitizeForProvider dropUnpairedCallsP dropOrphanedResultsP resolvedInP
  constructor
  rw [project_normalizeOrder]
  simpa using
    activeBlockValidFrom_filterCallsByP rows ∅ huniq (Finset.disjoint_empty_left _) hcoh

/-- Split-stability: a suffix of a unique-id transcript sanitizes to valid
provider input on its own, with no view of what preceded it. -/
theorem sanitizeForProvider_split_stable {old recent : List ProviderRow}
    (huniq : UniqueCallIds (project (old ++ recent)))
    (hcoh : AllCoherent recent) :
    ProviderValid (project (sanitizeForProvider recent)) := by
  refine sanitizeForProvider_sound ?_ hcoh
  refine UniqueCallIds.of_append_right (a := project old) ?_
  simpa [project, List.map_append] using huniq

/-! ## Fixpoint and idempotence -/

theorem restrictContent_eq_self {resolved : Finset ToolExecution.ToolCallId}
    {items : List Item} (h : Content.callsOf items ⊆ resolved) :
    restrictContent resolved items = items := by
  induction items with
  | nil => rfl
  | cons item rest ih =>
    cases item with
    | text index =>
      have hrest : Content.callsOf rest ⊆ resolved := by simpa using h
      simpa [restrictContent, keepItem] using ih hrest
    | other index =>
      have hrest : Content.callsOf rest ⊆ resolved := by simpa using h
      simpa [restrictContent, keepItem] using ih hrest
    | call callId =>
      have hmem : callId ∈ resolved := h (by simp)
      have hrest : Content.callsOf rest ⊆ resolved := fun c hc =>
        h (by simp only [Content.callsOf_cons_call]; exact Finset.mem_insert_of_mem hc)
      simp only [restrictContent, List.filter_cons, keepItem, hmem, decide_true,
        cond_true, List.cons.injEq, true_and]
      simpa [restrictContent] using ih hrest

theorem dropOrphanedFromP_eq_self (rows : List ProviderRow)
    (hnd : AllNonDegenerate rows) :
    ∀ pending, ActiveBlockValidFrom pending (project rows) →
      dropOrphanedFromP pending rows = rows := by
  induction rows with
  | nil => intro pending _; rfl
  | cons row rest ih =>
    intro pending hvalid
    have hhead : NonDegenerate row := hnd row (List.mem_cons_self _ _)
    have hrestNd : AllNonDegenerate rest := fun r hr => hnd r (List.mem_cons_of_mem _ hr)
    rw [project_cons] at hvalid
    cases hk : row.row.kind with
    | toolResult callId key =>
      have h := (activeBlockValidFrom_cons_result row.row (project rest) pending
        callId key hk).mp hvalid
      rw [dropOrphanedFromP_cons_result row rest pending callId key hk, if_pos h.1,
        ih hrestNd (pending.erase callId) h.2]
    | assistantToolCalls callIds =>
      have h := (activeBlockValidFrom_cons_assistant row.row (project rest) pending
        callIds hk).mp hvalid
      rw [dropOrphanedFromP_cons_assistant row rest pending callIds hk,
        ih hrestNd callIds h.2]
    | ordinary =>
      have h := (activeBlockValidFrom_cons_ordinary row.row (project rest) pending hk).mp hvalid
      rw [dropOrphanedFromP_cons_ordinary row rest pending hk,
        if_neg (by simp [not_emptyUserRow_of_nonDegenerate hhead hk]),
        ih hrestNd ∅ h.2]

theorem filterCallsByP_eq_self (rows : List ProviderRow)
    (hnd : AllNonDegenerate rows) :
    ∀ (R pending : Finset ToolExecution.ToolCallId),
      ActiveBlockValidFrom pending (project rows) →
      NonemptyAnnouncements (project rows) →
      AllCoherent rows →
      resolvedIn (project rows) ⊆ R →
      filterCallsByP R rows = rows := by
  induction rows with
  | nil => intro R pending _ _ _ _; rfl
  | cons row rest ih =>
    intro R pending hvalid hne hcoh hres
    have hhead : Coherent row := hcoh row (List.mem_cons_self _ _)
    have hheadNd : NonDegenerate row := hnd row (List.mem_cons_self _ _)
    have hrestCoh : AllCoherent rest := fun r hr => hcoh r (List.mem_cons_of_mem _ hr)
    have hrestNd : AllNonDegenerate rest := fun r hr => hnd r (List.mem_cons_of_mem _ hr)
    rw [project_cons] at hvalid hne hres
    have hres' : resolvedIn (project rest) ⊆ R :=
      (resolvedIn_subset_cons row.row (project rest)).trans hres
    cases hk : row.row.kind with
    | assistantToolCalls callIds =>
      have h := (activeBlockValidFrom_cons_assistant row.row (project rest) pending
        callIds hk).mp hvalid
      have hne' := (nonemptyAnnouncements_cons_assistant row.row (project rest)
        callIds hk).mp hne
      have hsub : callIds ⊆ R :=
        (activeBlockValid_pending_subset_resolved (project rest) callIds h.2).trans hres'
      have hcontent : Content.callsOf row.content = callIds := by
        unfold Coherent at hhead; rwa [hk] at hhead
      have hkeep : restrictContent R row.content = row.content :=
        restrictContent_eq_self (by rw [hcontent]; exact hsub)
      have hnotNil : restrictContent R row.content ≠ [] := by
        rw [hkeep]
        intro hnil
        rw [hnil] at hcontent
        exact hne'.1 hcontent.symm
      have hinter : callIds ∩ R = callIds := Finset.inter_eq_left.mpr hsub
      have hrow : restrictRow R row callIds = row := by
        have hkind : restrictedKind R callIds = row.row.kind := by
          unfold restrictedKind
          rw [hinter, if_neg hne'.1, hk]
        cases row with
        | mk innerRow innerContent =>
          simp only [restrictRow, hkeep]
          cases innerRow
          simp_all
      rw [filterCallsByP_cons_assistant row rest R callIds hk, if_neg hnotNil, hrow,
        ih hrestNd R callIds h.2 hne'.2 hrestCoh hres']
    | toolResult callId key =>
      have h := (activeBlockValidFrom_cons_result row.row (project rest) pending
        callId key hk).mp hvalid
      have hne' := (nonemptyAnnouncements_cons_other row.row (project rest)
        (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)).mp hne
      rw [filterCallsByP_cons_result row rest R callId key hk,
        ih hrestNd R (pending.erase callId) h.2 hne' hrestCoh hres']
    | ordinary =>
      have h := (activeBlockValidFrom_cons_ordinary row.row (project rest) pending hk).mp hvalid
      have hne' := (nonemptyAnnouncements_cons_other row.row (project rest)
        (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)).mp hne
      rw [filterCallsByP_cons_ordinary row rest R hk,
        if_neg (by simp [not_emptyAssistantRow_of_nonDegenerate hheadNd hk]),
        ih hrestNd R ∅ h.2 hne' hrestCoh hres']

/-- Every announcement the sanitizer emits is non-empty: a row that announced
nothing that resolved is either dropped or demoted to `.ordinary`. -/
theorem nonemptyAnnouncements_filterCallsByP
    (R : Finset ToolExecution.ToolCallId) (rows : List ProviderRow) :
    NonemptyAnnouncements (project (filterCallsByP R rows)) := by
  induction rows with
  | nil => simp
  | cons row rest ih =>
    cases hk : row.row.kind with
    | assistantToolCalls callIds =>
      rw [filterCallsByP_cons_assistant row rest R callIds hk]
      by_cases hnil : restrictContent R row.content = []
      · rw [if_pos hnil]; exact ih
      · rw [if_neg hnil, project_cons]
        by_cases hempty : callIds ∩ R = ∅
        · have hkindOrd : (restrictRow R row callIds).row.kind = .ordinary := by
            simp [restrictRow, restrictedKind, hempty]
          rw [nonemptyAnnouncements_cons_other _ _
            (by intro c hc; rw [hkindOrd] at hc; exact MessageKind.noConfusion hc)]
          exact ih
        · have hkindCalls : (restrictRow R row callIds).row.kind
              = .assistantToolCalls (callIds ∩ R) := by
            simp [restrictRow, restrictedKind, hempty]
          rw [nonemptyAnnouncements_cons_assistant _ _ _ hkindCalls]
          exact ⟨hempty, ih⟩
    | toolResult callId key =>
      rw [filterCallsByP_cons_result row rest R callId key hk, project_cons,
        nonemptyAnnouncements_cons_other _ _
          (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)]
      exact ih
    | ordinary =>
      rw [filterCallsByP_cons_ordinary row rest R hk]
      by_cases hempty : emptyAssistantRow row
      · rw [if_pos hempty]; exact ih
      · rw [if_neg hempty, project_cons,
          nonemptyAnnouncements_cons_other _ _
            (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)]
        exact ih

theorem nonemptyAnnouncements_sanitizeForProvider (rows : List ProviderRow) :
    NonemptyAnnouncements (project (sanitizeForProvider rows)) := by
  unfold sanitizeForProvider dropUnpairedCallsP
  rw [project_normalizeOrder]
  exact nonemptyAnnouncements_filterCallsByP _ _

theorem allCoherent_sanitizeForProvider {rows : List ProviderRow}
    (hcoh : AllCoherent rows) : AllCoherent (sanitizeForProvider rows) := by
  unfold sanitizeForProvider dropUnpairedCallsP dropOrphanedResultsP
  exact allCoherent_normalizeOrder
    (allCoherent_filterCallsByP _ (allCoherent_dropOrphanedFromP hcoh ∅))

/-! ### The sanitizer's output carries no empty messages

This is what makes the fixpoint hypothesis discharge for `sanitizeForProvider`'s
own output, and therefore what makes idempotence hold: an output that still
carried an empty row would not be a fixpoint, because a second pass would drop
it. -/

/-- What stage 1 guarantees about `.ordinary` rows: no empty *user* message
survives it. Stage 2 removes the assistant counterpart, and the two together
give `NonDegenerate`. -/
def OrphanStagePruned (pr : ProviderRow) : Prop :=
  match pr.row.kind with
  | .ordinary => emptyUserRow pr = false
  | _ => True

theorem nonDegenerate_of_pruned {pr : ProviderRow} (hk : pr.row.kind = .ordinary)
    (hu : emptyUserRow pr = false) (ha : emptyAssistantRow pr = false) :
    NonDegenerate pr := by
  unfold NonDegenerate
  rw [hk]
  unfold emptyUserRow at hu
  unfold emptyAssistantRow at ha
  cases hrole : pr.row.role with
  | user => rw [hrole] at hu; simpa using hu
  | assistant => rw [hrole] at ha; simpa using ha

theorem orphanStagePruned_dropOrphanedFromP (rows : List ProviderRow) :
    ∀ pending, ∀ pr ∈ dropOrphanedFromP pending rows, OrphanStagePruned pr := by
  induction rows with
  | nil => intro pending pr hpr; simp at hpr
  | cons row rest ih =>
    intro pending pr hpr
    cases hk : row.row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFromP_cons_result row rest pending callId key hk] at hpr
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · unfold OrphanStagePruned; rw [hk]; trivial
        · exact ih (pending.erase callId) pr htail
      · rw [if_neg hmem] at hpr
        exact ih pending pr hpr
    | assistantToolCalls callIds =>
      rw [dropOrphanedFromP_cons_assistant row rest pending callIds hk] at hpr
      rcases List.mem_cons.mp hpr with rfl | htail
      · unfold OrphanStagePruned; rw [hk]; trivial
      · exact ih callIds pr htail
    | ordinary =>
      rw [dropOrphanedFromP_cons_ordinary row rest pending hk] at hpr
      by_cases hempty : emptyUserRow row
      · rw [if_pos hempty] at hpr
        exact ih pending pr hpr
      · rw [if_neg hempty] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · unfold OrphanStagePruned; rw [hk]; simpa using hempty
        · exact ih ∅ pr htail

theorem allNonDegenerate_filterCallsByP {rows : List ProviderRow}
    (R : Finset ToolExecution.ToolCallId)
    (hpruned : ∀ pr ∈ rows, OrphanStagePruned pr) :
    AllNonDegenerate (filterCallsByP R rows) := by
  induction rows with
  | nil => intro pr hpr; simp at hpr
  | cons row rest ih =>
    have hhead : OrphanStagePruned row := hpruned row (List.mem_cons_self _ _)
    have hrest : ∀ r ∈ rest, OrphanStagePruned r := fun r hr =>
      hpruned r (List.mem_cons_of_mem _ hr)
    intro pr hpr
    cases hk : row.row.kind with
    | assistantToolCalls callIds =>
      rw [filterCallsByP_cons_assistant row rest R callIds hk] at hpr
      by_cases hnil : restrictContent R row.content = []
      · rw [if_pos hnil] at hpr
        exact ih hrest pr hpr
      · rw [if_neg hnil] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · unfold NonDegenerate
          by_cases hempty : callIds ∩ R = ∅
          · rw [show (restrictRow R row callIds).row.kind = MessageKind.ordinary by
              simp [restrictRow, restrictedKind, hempty]]
            simpa [restrictRow] using hnil
          · rw [show (restrictRow R row callIds).row.kind
                = MessageKind.assistantToolCalls (callIds ∩ R) by
              simp [restrictRow, restrictedKind, hempty]]
            simpa [restrictRow] using hnil
        · exact ih hrest pr htail
    | toolResult callId key =>
      rw [filterCallsByP_cons_result row rest R callId key hk] at hpr
      rcases List.mem_cons.mp hpr with rfl | htail
      · unfold NonDegenerate; rw [hk]; trivial
      · exact ih hrest pr htail
    | ordinary =>
      rw [filterCallsByP_cons_ordinary row rest R hk] at hpr
      by_cases hempty : emptyAssistantRow row
      · rw [if_pos hempty] at hpr
        exact ih hrest pr hpr
      · rw [if_neg hempty] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · have hu : emptyUserRow pr = false := by
            unfold OrphanStagePruned at hhead; rwa [hk] at hhead
          exact nonDegenerate_of_pruned hk hu (by simpa using hempty)
        · exact ih hrest pr htail

theorem nonDegenerate_normalizeRow {pr : ProviderRow} (h : NonDegenerate pr) :
    NonDegenerate (normalizeRow pr) := by
  unfold NonDegenerate at h ⊢
  simp only [normalizeRow_row]
  cases hk : pr.row.kind with
  | toolResult callId key => trivial
  | assistantToolCalls callIds =>
    rw [hk] at h
    simp only [normalizeRow]
    intro hnil
    exact h (by
      have := Content.length_normalize pr.content
      rw [hnil] at this
      simpa using this.symm)
  | ordinary =>
    rw [hk] at h
    simp only [normalizeRow]
    intro hnil
    exact h (by
      have := Content.length_normalize pr.content
      rw [hnil] at this
      simpa using this.symm)

theorem allNonDegenerate_normalizeOrder {rows : List ProviderRow}
    (h : AllNonDegenerate rows) : AllNonDegenerate (normalizeOrder rows) := by
  intro pr hpr
  obtain ⟨source, hsource, rfl⟩ := List.mem_map.mp hpr
  exact nonDegenerate_normalizeRow (h source hsource)

theorem allNonDegenerate_sanitizeForProvider (rows : List ProviderRow) :
    AllNonDegenerate (sanitizeForProvider rows) := by
  unfold sanitizeForProvider dropUnpairedCallsP dropOrphanedResultsP
  exact allNonDegenerate_normalizeOrder
    (allNonDegenerate_filterCallsByP _ (orphanStagePruned_dropOrphanedFromP rows ∅))

/-- **Fixpoint.** Already-narrowed, already-ordered provider input is untouched. -/
theorem sanitizeForProvider_fixpoint {rows : List ProviderRow}
    (hvalid : ProviderValid (project rows))
    (hne : NonemptyAnnouncements (project rows))
    (hcoh : AllCoherent rows)
    (hnd : AllNonDegenerate rows)
    (hordered : normalizeOrder rows = rows) :
    sanitizeForProvider rows = rows := by
  unfold sanitizeForProvider dropUnpairedCallsP dropOrphanedResultsP resolvedInP
  rw [dropOrphanedFromP_eq_self rows hnd ∅ hvalid.activeBlockValid]
  rw [filterCallsByP_eq_self rows hnd (resolvedIn (project rows)) ∅
    hvalid.activeBlockValid hne hcoh (Finset.Subset.refl _)]
  exact hordered

/-- **Idempotence.** A second pass at the provider boundary is a no-op. -/
theorem sanitizeForProvider_idempotent {rows : List ProviderRow}
    (huniq : UniqueCallIds (project rows)) (hcoh : AllCoherent rows) :
    sanitizeForProvider (sanitizeForProvider rows) = sanitizeForProvider rows := by
  refine sanitizeForProvider_fixpoint
    (sanitizeForProvider_sound huniq hcoh)
    (nonemptyAnnouncements_sanitizeForProvider rows)
    (allCoherent_sanitizeForProvider hcoh)
    (allNonDegenerate_sanitizeForProvider rows) ?_
  unfold sanitizeForProvider
  exact normalizeOrder_idempotent _

/-! ## Refinement: how this relates to the row-only `sanitize`

`Proofs.PromptAssembly.Executable.sanitize` is the coarser model. The two agree
exactly on the fragment the coarser model can faithfully describe — assistant
rows whose content is nothing but tool calls.

Outside that fragment they genuinely differ, and the difference is the finding
this file documents: on an assistant message carrying text alongside a tool call
that never resolved, `sanitize` drops the row and production keeps it. The
refinement is therefore stated *conditionally*, on purpose. Stating it
unconditionally would be false. -/

/-- An assistant row whose content is nothing but tool calls — the fragment the
row-only model describes faithfully. -/
def CallsOnlyAssistant (pr : ProviderRow) : Prop :=
  match pr.row.kind with
  | .assistantToolCalls _ => ∀ item ∈ pr.content, item.isCall
  | _ => True

abbrev AllCallsOnly (rows : List ProviderRow) : Prop :=
  ∀ pr ∈ rows, CallsOnlyAssistant pr

theorem restrictContent_eq_nil_of_inter_empty
    {R : Finset ToolExecution.ToolCallId} {items : List Item}
    (honly : ∀ item ∈ items, item.isCall)
    (hempty : Content.callsOf items ∩ R = ∅) :
    restrictContent R items = [] := by
  refine List.filter_eq_nil_iff.mpr ?_
  intro item hitem
  have hcall := honly item hitem
  cases hitemKind : item with
  | text index => rw [hitemKind] at hcall; simp at hcall
  | other index => rw [hitemKind] at hcall; simp at hcall
  | call callId =>
    have hmemCalls : callId ∈ Content.callsOf items :=
      Content.mem_callsOf_of_mem (by rw [← hitemKind]; exact hitem)
    have hnotR : callId ∉ R := by
      intro hR
      have : callId ∈ Content.callsOf items ∩ R := Finset.mem_inter.mpr ⟨hmemCalls, hR⟩
      rw [hempty] at this
      simp at this
    simp [keepItem, hitemKind, hnotR]

theorem allCallsOnly_dropOrphanedFromP {rows : List ProviderRow}
    (h : AllCallsOnly rows) : ∀ pending, AllCallsOnly (dropOrphanedFromP pending rows) := by
  induction rows with
  | nil => intro pending pr hpr; simp at hpr
  | cons row rest ih =>
    intro pending pr hpr
    have hhead : CallsOnlyAssistant row := h row (List.mem_cons_self _ _)
    have hrest : AllCallsOnly rest := fun r hr => h r (List.mem_cons_of_mem _ hr)
    cases hk : row.row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFromP_cons_result row rest pending callId key hk] at hpr
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · exact hhead
        · exact ih hrest (pending.erase callId) pr htail
      · rw [if_neg hmem] at hpr
        exact ih hrest pending pr hpr
    | assistantToolCalls callIds =>
      rw [dropOrphanedFromP_cons_assistant row rest pending callIds hk] at hpr
      rcases List.mem_cons.mp hpr with rfl | htail
      · exact hhead
      · exact ih hrest callIds pr htail
    | ordinary =>
      rw [dropOrphanedFromP_cons_ordinary row rest pending hk] at hpr
      by_cases hempty : emptyUserRow row
      · rw [if_pos hempty] at hpr
        exact ih hrest pending pr hpr
      · rw [if_neg hempty] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · exact hhead
        · exact ih hrest ∅ pr htail

/-- Stage 1 only ever drops rows, so it preserves non-degeneracy. -/
theorem allNonDegenerate_dropOrphanedFromP {rows : List ProviderRow}
    (h : AllNonDegenerate rows) :
    ∀ pending, AllNonDegenerate (dropOrphanedFromP pending rows) := by
  induction rows with
  | nil => intro pending pr hpr; simp at hpr
  | cons row rest ih =>
    intro pending pr hpr
    have hhead : NonDegenerate row := h row (List.mem_cons_self _ _)
    have hrest : AllNonDegenerate rest := fun r hr => h r (List.mem_cons_of_mem _ hr)
    cases hk : row.row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFromP_cons_result row rest pending callId key hk] at hpr
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · exact hhead
        · exact ih hrest (pending.erase callId) pr htail
      · rw [if_neg hmem] at hpr
        exact ih hrest pending pr hpr
    | assistantToolCalls callIds =>
      rw [dropOrphanedFromP_cons_assistant row rest pending callIds hk] at hpr
      rcases List.mem_cons.mp hpr with rfl | htail
      · exact hhead
      · exact ih hrest callIds pr htail
    | ordinary =>
      rw [dropOrphanedFromP_cons_ordinary row rest pending hk] at hpr
      by_cases hempty : emptyUserRow row
      · rw [if_pos hempty] at hpr
        exact ih hrest pending pr hpr
      · rw [if_neg hempty] at hpr
        rcases List.mem_cons.mp hpr with rfl | htail
        · exact hhead
        · exact ih hrest ∅ pr htail

/-- On the calls-only fragment, stage 2 projects onto the row model's `filterCallsBy`.

Non-degeneracy is needed for the same reason stage 1 needed it: the row-only
model keeps every `.ordinary` row, while this stage prunes empty assistant
messages the way Rust does. -/
theorem project_filterCallsByP_of_callsOnly (rows : List ProviderRow)
    (R : Finset ToolExecution.ToolCallId)
    (hcoh : AllCoherent rows) (honly : AllCallsOnly rows)
    (hnd : AllNonDegenerate rows) :
    project (filterCallsByP R rows) = filterCallsBy R (project rows) := by
  induction rows with
  | nil => rfl
  | cons row rest ih =>
    have hhead : Coherent row := hcoh row (List.mem_cons_self _ _)
    have hheadOnly : CallsOnlyAssistant row := honly row (List.mem_cons_self _ _)
    have hheadNd : NonDegenerate row := hnd row (List.mem_cons_self _ _)
    have hrestCoh : AllCoherent rest := fun r hr => hcoh r (List.mem_cons_of_mem _ hr)
    have hrestOnly : AllCallsOnly rest := fun r hr => honly r (List.mem_cons_of_mem _ hr)
    have hrestNd : AllNonDegenerate rest := fun r hr => hnd r (List.mem_cons_of_mem _ hr)
    cases hk : row.row.kind with
    | assistantToolCalls callIds =>
      have hcontent : Content.callsOf row.content = callIds := by
        unfold Coherent at hhead; rwa [hk] at hhead
      have hitems : ∀ item ∈ row.content, item.isCall := by
        unfold CallsOnlyAssistant at hheadOnly; rwa [hk] at hheadOnly
      rw [filterCallsByP_cons_assistant row rest R callIds hk, project_cons,
        filterCallsBy_cons_assistant row.row (project rest) R callIds hk]
      by_cases hempty : callIds ∩ R = ∅
      · have hnil : restrictContent R row.content = [] :=
          restrictContent_eq_nil_of_inter_empty hitems (by rw [hcontent]; exact hempty)
        rw [if_pos hnil, if_pos hempty, ih hrestCoh hrestOnly hrestNd]
      · have hnil : restrictContent R row.content ≠ [] := by
          intro hnil
          exact hempty (inter_eq_empty_of_restrictContent_nil hhead hk hnil)
        rw [if_neg hnil, if_neg hempty, project_cons, ih hrestCoh hrestOnly hrestNd]
        congr 1
        simp [restrictRow, restrictedKind, hempty, withKind]
    | toolResult callId key =>
      rw [filterCallsByP_cons_result row rest R callId key hk, project_cons, project_cons,
        filterCallsBy_cons_result row.row (project rest) R callId key hk,
        ih hrestCoh hrestOnly hrestNd]
    | ordinary =>
      rw [filterCallsByP_cons_ordinary row rest R hk,
        if_neg (by simp [not_emptyAssistantRow_of_nonDegenerate hheadNd hk]),
        project_cons, project_cons,
        filterCallsBy_cons_ordinary row.row (project rest) R hk, ih hrestCoh hrestOnly hrestNd]

/-- **Conditional refinement.** On the calls-only, non-degenerate fragment the
enriched production model and the row-only `sanitize` agree exactly.

Off that fragment they differ by design, in two ways, both of them cases the
row-only model cannot express: an assistant message carrying text alongside an
unresolved call (see the module docstring), and an empty message, which
production drops and `sanitize` keeps. -/
theorem project_sanitizeForProvider_eq_sanitize {rows : List ProviderRow}
    (hcoh : AllCoherent rows) (honly : AllCallsOnly rows)
    (hnd : AllNonDegenerate rows) :
    project (sanitizeForProvider rows) = sanitize (project rows) := by
  unfold sanitizeForProvider dropUnpairedCallsP dropOrphanedResultsP resolvedInP sanitize
    dropUnpairedCalls dropOrphanedResults
  rw [project_normalizeOrder,
    project_filterCallsByP_of_callsOnly (dropOrphanedFromP ∅ rows) _
      (allCoherent_dropOrphanedFromP hcoh ∅) (allCallsOnly_dropOrphanedFromP honly ∅)
      (allNonDegenerate_dropOrphanedFromP hnd ∅),
    project_dropOrphanedFromP rows hnd ∅]

end PromptAssembly.Provider
