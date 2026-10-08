import Proofs.Session.Properties
import Proofs.Request.Executable

/-! Folding queued user messages into the turn that claims them.

A user may keep writing while a session is busy. Each message is admitted and
signed as its own pending request. When the queue head is a user message, its
claim selects the run of user messages directly behind it, and the claimed
turn answers all of them with one inference.

Semantics:
* The claim transaction is the cutoff. A message admitted after it waits in
  the queue and heads (or folds into) the next turn.
* Only interactive user messages under the head's own requester authority and
  execution settings, whose signed admission the claim verified, fold. Agent
  steering, goal and background continuations, and scheduled or
  trigger-sourced requests keep their own turns.
* Selection is not consumption. The turn publishes each selected message as
  its own authored input before its first inference, and that publication
  supersedes the message's request. A turn that ends before publishing a
  selected message (an early failure, an interrupt) did not answer it: the
  message returns to the head of the queue.
* A redrive of the same physical request claims again: messages it already
  published stay superseded into it and are reused, and the claim selects
  again among those still queued.
* Provider retries inside the turn reuse its fixed input. -/
namespace SessionQueue

theorem mem_foldRun {head entry : QueueEntry} {admitted : List RequestId}
    {entries : List QueueEntry} (h_mem : entry ∈ foldRun head admitted entries) :
    head.foldsInto entry ∧ entry.requestId ∈ admitted ∧ entry ∈ entries := by
  induction entries with
  | nil => simp [foldRun] at h_mem
  | cons first rest ih =>
      by_cases h : head.foldsInto first ∧ first.requestId ∈ admitted
      · simp only [foldRun, h, and_self, ↓reduceIte, List.mem_cons] at h_mem
        rcases h_mem with h_eq | h_rest
        · subst h_eq
          exact ⟨h.1, h.2, List.mem_cons_self _ _⟩
        · rcases ih h_rest with ⟨h_fold, h_admitted, h_in⟩
          exact ⟨h_fold, h_admitted, List.mem_cons_of_mem _ h_in⟩
      · simp [foldRun, h] at h_mem

/-- Folding never merges principals: every folded message carries the head's
own signed requester and execution settings. -/
theorem folded_share_head_authority {head entry : QueueEntry}
    {admitted : List RequestId} {entries : List QueueEntry}
    (h_mem : entry ∈ foldRun head admitted entries) :
    entry.requester = head.requester ∧ entry.turnContext = head.turnContext ∧
      entry.queuedUserMessage := by
  have h := (mem_foldRun h_mem).1
  exact ⟨h.2.2.2.1, h.2.2.2.2, h.2.2.1⟩

theorem foreign_requester_never_folds {head entry : QueueEntry}
    {admitted : List RequestId} {entries : List QueueEntry}
    (h_foreign : entry.requester ≠ head.requester) :
    entry ∉ foldRun head admitted entries := fun h_mem =>
  h_foreign (folded_share_head_authority h_mem).1

/-- Only an admitted message can be folded: the claim transaction verifies
each folded request's signed admission before answering it. -/
theorem folded_admitted {head entry : QueueEntry} {admitted : List RequestId}
    {entries : List QueueEntry} (h_mem : entry ∈ foldRun head admitted entries) :
    entry.requestId ∈ admitted := (mem_foldRun h_mem).2.1

theorem non_user_head_folds_nothing {head : QueueEntry} (admitted : List RequestId)
    (entries : List QueueEntry)
    (h_head : ¬ (head.source = .user ∧ head.origin = .interactive)) :
    foldRun head admitted entries = [] := by
  cases entries with
  | nil => rfl
  | cons entry rest =>
      have h : ¬ (head.foldsInto entry ∧ entry.requestId ∈ admitted) := fun h =>
        h_head ⟨h.1.1, h.1.2.1⟩
      simp [foldRun, h]

theorem foldRun_nil_admitted (head : QueueEntry) (entries : List QueueEntry) :
    foldRun head [] entries = [] := by
  cases entries <;> simp [foldRun]

/-- A claim that verified no foldable admission is the ordinary claim. -/
theorem claimFolding_nil_admitted (s : SessionQueueState) (entry : QueueEntry)
    (rest : List QueueEntry) (h_idle : s.folding = []) :
    s.claimFolding entry rest [] = s.claimHead entry rest := by
  simp [SessionQueueState.claimFolding, SessionQueueState.claimHead, foldRun_nil_admitted,
    h_idle]

theorem claimFolding_claims_head (s : SessionQueueState) (entry : QueueEntry)
    (rest : List QueueEntry) (admitted : List RequestId) :
    (s.claimFolding entry rest admitted).active = some entry.requestId := rfl

/-- The claim selects the run; nothing is terminal yet. -/
theorem claimFolding_selects (s : SessionQueueState) (entry : QueueEntry)
    (rest : List QueueEntry) (admitted : List RequestId) :
    (s.claimFolding entry rest admitted).folding = foldRun entry admitted rest ∧
      (s.claimFolding entry rest admitted).terminal = s.terminal := ⟨rfl, rfl⟩

/-- The selected run leaves the pending queue as a prefix behind the head;
every other entry keeps its relative order. -/
theorem claimFolding_pending_suffix (s : SessionQueueState) (entry : QueueEntry)
    (rest : List QueueEntry) (admitted : List RequestId) :
    (s.claimFolding entry rest admitted).folding ++
      (s.claimFolding entry rest admitted).pending = rest :=
  foldRun_append_drop entry admitted rest

/-- Publishing a selected message consumes the next one in queue order and
supersedes its request. -/
theorem consume_terminalizes_next {pre post : SessionQueueState}
    (h_step : step? pre .consumeFolded = some post) :
    ∃ entry rest, pre.folding = entry :: rest ∧ post.folding = rest ∧
      entry.requestId ∈ post.terminal ∧ post.active = pre.active := by
  simp only [step?] at h_step
  split at h_step
  · rename_i entry rest _ h_folding
    cases h_step
    exact ⟨entry, rest, h_folding, rfl, Finset.mem_insert_self _ _, rfl⟩
  · contradiction

/-- A turn that ends without publishing a selected message returns it to the
head of the queue, ahead of every message admitted later. -/
theorem finish_returns_unconsumed {pre post : SessionQueueState}
    (h_step : step? pre .finishActive = some post) :
    post.pending = pre.folding ++ pre.pending ∧ post.folding = [] ∧ post.active = none := by
  simp only [step?] at h_step
  split at h_step
  · cases h_step
    exact ⟨rfl, rfl, rfl⟩
  · contradiction

/-- An early failure strands nothing: claiming, then ending the turn before
any publication, leaves every selected message queued in its original order. -/
theorem early_finish_restores_queue (s : SessionQueueState) (entry : QueueEntry)
    (rest : List QueueEntry) (admitted : List RequestId) :
    ((s.claimFolding entry rest admitted).finishActive entry.requestId).pending = rest ∧
      ((s.claimFolding entry rest admitted).finishActive entry.requestId).terminal =
        insert entry.requestId s.terminal := by
  constructor
  · simp only [SessionQueueState.finishActive]
    exact claimFolding_pending_suffix s entry rest admitted
  · rfl

/-- The claim is the cutoff: a message admitted while the folded turn is
active is queued for a later turn and does not change the active request. -/
theorem append_after_claim_waits {pre post : SessionQueueState} {entry : QueueEntry}
    (h_step : step? pre (.appendPending entry) = some post) :
    post.active = pre.active ∧ post.pending = pre.pending ++ [entry] ∧
      post.terminal = pre.terminal ∧ post.folding = pre.folding := by
  simp only [step?] at h_step
  split at h_step
  · cases h_step
    exact ⟨rfl, rfl, rfl, rfl⟩
  · contradiction

/-- Each consumed request leaves `pending` through the existing supersession
transition; folding adds no lifecycle state. -/
theorem folded_request_superseded (request : RequestContext)
    (h_state : request.state = .pending) (h_admission : request.admission = .released) :
    (RequestContext.step? request .dedupLose).map RequestContext.state = some .superseded := by
  simp [RequestContext.step?, h_state, h_admission]

/-- One authored transcript entry per admitted message in the folded turn. -/
inductive AuthoredKey where
  | context
  | prompt
  | folded (requestId : RequestId)
  deriving DecidableEq, Repr

/-- The input a folded turn admits: the request context, then the head's
message, then each folded message in queue order, as distinct user messages.
The owned loop publishes exactly these authored entries before its first
inference, from the input itself and not from the provider projection, which
the existing compaction owner may reduce. -/
structure TurnInput (α : Type) where
  context : Option α
  head : α
  folded : List (RequestId × α)

namespace TurnInput

variable {α : Type}

def providerInput (input : TurnInput α) : List α :=
  input.context.toList ++ input.head :: input.folded.map Prod.snd

def authored (input : TurnInput α) : List (AuthoredKey × α) :=
  (input.context.toList.map fun context => (.context, context)) ++
    (.prompt, input.head) :: input.folded.map fun (id, message) => (.folded id, message)

/-- Before any provider-view reduction, the provider input is the transcript's
authored entries in order. -/
theorem providerInput_eq_authored (input : TurnInput α) :
    input.providerInput = input.authored.map Prod.snd := by
  cases h : input.context <;>
    simp [providerInput, authored, h, Function.comp_def]

theorem authored_keys_nodup (input : TurnInput α)
    (h_ids : (input.folded.map Prod.fst).Nodup) :
    (input.authored.map Prod.fst).Nodup := by
  have h_folded : (input.folded.map fun (pair : RequestId × α) =>
      (AuthoredKey.folded pair.1)).Nodup := by
    have h_inj : Function.Injective AuthoredKey.folded := fun _ _ h => by cases h; rfl
    simpa [List.map_map, Function.comp_def] using h_ids.map h_inj
  cases h : input.context <;>
    simp [authored, h, Function.comp_def, h_folded, List.mem_map]

end TurnInput

/-- The turn a folding claim runs, with each request's admitted content. -/
def foldedTurn {α : Type} (content : RequestId → α) (context : Option α)
    (entry : QueueEntry) (rest : List QueueEntry) (admitted : List RequestId) :
    TurnInput α :=
  { context
  , head := content entry.requestId
  , folded := (foldRun entry admitted rest).map fun folded =>
      (folded.requestId, content folded.requestId) }

/-- The folded turn admits exactly the messages the claim selected, in queue
order. -/
theorem foldedTurn_admits_selection {α : Type} (content : RequestId → α)
    (context : Option α) (s : SessionQueueState) (entry : QueueEntry)
    (rest : List QueueEntry) (admitted : List RequestId) :
    (foldedTurn content context entry rest admitted).folded.map Prod.fst =
      (s.claimFolding entry rest admitted).folding.map QueueEntry.requestId := by
  simp [foldedTurn, SessionQueueState.claimFolding, Function.comp_def]

end SessionQueue
