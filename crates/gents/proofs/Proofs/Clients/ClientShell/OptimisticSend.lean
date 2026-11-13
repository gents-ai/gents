import Lean

/-!
Desktop chat submission: a standard optimistic send.

The client picks the request identity when the person sends, shows the
message as pending at once and keeps its payload. It sends one signed create.
A reply confirms it; a refusal, or no reply by the client's deadline (the
reply was lost, the runtime restarted, the app closed), shows it as failed
with Retry, unless the runtime denied this client (`view`). Retry resends the same payload under the same identity. The
runtime treats a repeat of that identity as the same request, so a retry after
a lost reply never makes a second turn. The payload is released only once the
message is confirmed or the person discards it.

Code owners of each action:
* `create` is `gents::request::lifecycle::send_client_request`. It requires
  the create's `retry_key` to be `client_request_key` of its signer and
  request identity (`AgentRequest.retry_key` is `@index(unique: true)`, so two
  concurrent creates of one identity commit once), returns the request the
  runtime already holds under that `request_id` before it validates anything
  else (`held_client_request`, which also finds requests committed before the
  key existed), and `stage_client_request_in_txn` refuses any client create
  whose `request_id` is already held. `AgentRequest.request_id` itself is only
  `@index`; these three checks are what make the identity idempotent.
* `refuse` is a runtime refusal (a closed question, an unavailable Agent). It
  never applies to an identity the runtime holds: the held lookup runs before
  the mailbox cause or any other admission check
  (`gents_desktop_core` `submit_request`), so a resend of a held identity
  confirms (`held_resend_never_refused`).
* `drop` is the transport failing before the create reaches the runtime.
* `timeout` is the client deadline `CHAT_SEND_DEADLINE_MS`
  (`apps/gents-desktop/src/hooks/chatDelivery.ts`). Each send persists its
  deadline with the kept payload (`ChatDraft.pendingUntil`), so every window
  derives the same status from the shared draft and a closed or crashed
  window's send also fails at its deadline. A window's in-memory state never
  decides the status.
* `receive true` marks the message confirmed before the view applies the
  reply; a failure to update the view is reported as such and never shows a
  confirmed message as failed.
* `observe` is the client reading the selected session
  (`observeLandedChatSends` in `chatDelivery.ts`): a failed message whose
  identity the session holds is confirmed. A pending one waits for its reply
  or deadline, so the reply still updates the view that sent it.

Decisions (#3538, user):
* No receipt journal, no reconcile of an unknown outcome and no
  "unretained" state: every state the person sees is pending (progressing),
  confirmed, failed with Retry and Discard, or discarded.
* Duplicates are impossible by construction (`turns_nodup`), not by
  reconciling evidence about delivery: the identity is fixed at compose time
  and the owner's create is idempotent on it.
* A late reply is still applied: a confirmed reply after a client timeout
  confirms the message, because the runtime holds it.
* Chat sends never block the composer. A mailbox answer does while it is
  pending, failed or confirmed: a second answer to one question is a new
  authored answer, not a retry, and a confirmed answer stays shown until the
  question is observed closed (the runtime claims it after the reply).
* `compose` moves the authored text out of the composer into the kept
  message, which shows it with Retry and Discard. The composer therefore never
  holds text that a second send would author again under a fresh identity
  (TLA RED `RetryFreshId`), whether the first send is pending, failed after a
  slow commit, or confirmed late.

TLA `ClientSubmissionContention` checks the same flow with the runtime's
conflict rebuild, and its REDs show that a retry under a fresh identity
duplicates the turn and that a failed message without its payload is stuck.
-/

namespace ClientOptimisticSend
open Lean

inductive Status where
  | pending
  | confirmed
  | failed
  | discarded
  deriving DecidableEq, Repr

/-- One authored message on the client. `id` is the request identity the
client picked at compose time; `payload` is whether the client still holds the
authored input it would resend. -/
structure Message where
  id : Nat
  status : Status
  payload : Bool
  deriving DecidableEq, Repr

/-- `wire` holds creates on their way to the runtime or still committing
there; `replies` holds the runtime's answers on their way back (`true` is a
committed create); `turns` holds the runtime's `AgentRequest` rows by request
identity. -/
structure State where
  messages : List Message
  wire : List Nat
  replies : List (Nat × Bool)
  turns : List Nat
  deriving DecidableEq, Repr

def init : State := ⟨[], [], [], []⟩

/-- The runtime's create, idempotent on the request identity. -/
def create (turns : List Nat) (id : Nat) : List Nat :=
  if id ∈ turns then turns else turns ++ [id]

def update (messages : List Message) (id : Nat) (f : Message → Message) : List Message :=
  messages.map fun message => if message.id = id then f message else message

def statusOf (messages : List Message) (id : Nat) : Option Status :=
  (messages.find? fun message => message.id = id).map (·.status)

inductive Action where
  /-- The person sends: the client picks `id`, shows it pending and sends. -/
  | compose (id : Nat)
  /-- The runtime commits a create and replies. -/
  | commit (id : Nat)
  /-- The runtime refuses a create of an identity it does not hold. -/
  | refuse (id : Nat)
  /-- The transport fails before the create reaches the runtime. -/
  | drop (id : Nat)
  /-- A reply is lost on its way back. -/
  | loseReply (id : Nat) (ok : Bool)
  /-- The client receives a reply. -/
  | receive (id : Nat) (ok : Bool)
  /-- The client's deadline passes with no reply. -/
  | timeout (id : Nat)
  /-- The person retries a failed message: the same payload, the same id. -/
  | retry (id : Nat)
  /-- The person discards a failed message. -/
  | discard (id : Nat)
  /-- The client observes the session holding a failed message's identity. -/
  | observe (id : Nat)
  deriving DecidableEq, Repr

def onReply (ok : Bool) (message : Message) : Message :=
  if ok then
    if message.status = .discarded then message
    else { message with status := .confirmed, payload := false }
  else if message.status = .pending then { message with status := .failed }
  else message

def onTimeout (message : Message) : Message :=
  if message.status = .pending then { message with status := .failed } else message

def onRetry (message : Message) : Message := { message with status := .pending }

def onDiscard (message : Message) : Message :=
  { message with status := .discarded, payload := false }

def onObserve (message : Message) : Message :=
  if message.status = .failed then { message with status := .confirmed, payload := false }
  else message

def step (s : State) : Action → Option State
  | .compose id =>
    if s.messages.any (fun message => message.id = id) then none
    else some { s with messages := s.messages ++ [⟨id, .pending, true⟩], wire := s.wire ++ [id] }
  | .commit id =>
    if id ∈ s.wire then
      some { s with wire := s.wire.erase id, turns := create s.turns id,
                    replies := s.replies ++ [(id, true)] }
    else none
  | .refuse id =>
    if id ∈ s.wire ∧ id ∉ s.turns then
      some { s with wire := s.wire.erase id, replies := s.replies ++ [(id, false)] }
    else none
  | .drop id =>
    if id ∈ s.wire then
      some { s with wire := s.wire.erase id, replies := s.replies ++ [(id, false)] }
    else none
  | .loseReply id ok =>
    if (id, ok) ∈ s.replies then some { s with replies := s.replies.erase (id, ok) } else none
  | .receive id ok =>
    if (id, ok) ∈ s.replies then
      some { s with replies := s.replies.erase (id, ok), messages := update s.messages id (onReply ok) }
    else none
  | .timeout id =>
    if statusOf s.messages id = some .pending then
      some { s with messages := update s.messages id onTimeout }
    else none
  | .retry id =>
    if s.messages.any (fun message => message.id = id ∧ message.status = .failed ∧ message.payload) then
      some { s with messages := update s.messages id onRetry, wire := s.wire ++ [id] }
    else none
  | .discard id =>
    if statusOf s.messages id = some .failed then
      some { s with messages := update s.messages id onDiscard }
    else none
  | .observe id =>
    if id ∈ s.turns ∧ statusOf s.messages id = some .failed then
      some { s with messages := update s.messages id onObserve }
    else none

inductive Reachable : State → Prop where
  | init : Reachable init
  | step {s s' : State} (action : Action) : Reachable s → step s action = some s' → Reachable s'

/-! ### Invariants -/

/-- The runtime never holds a request the client did not author, and every
create on the wire belongs to an authored message. -/
def known (s : State) : Prop :=
  (∀ id ∈ s.wire, ∃ m ∈ s.messages, m.id = id) ∧
  (∀ id ∈ s.turns, ∃ m ∈ s.messages, m.id = id)

/-- A message that is not confirmed or discarded still has its payload. -/
def retained (s : State) : Prop :=
  ∀ m ∈ s.messages, (m.status = .pending ∨ m.status = .failed) → m.payload = true

structure Inv (s : State) : Prop where
  ids : (s.messages.map (·.id)).Nodup
  turns : s.turns.Nodup
  known : known s
  retained : retained s

theorem create_nodup (turns : List Nat) (id : Nat) (h : turns.Nodup) : (create turns id).Nodup := by
  unfold create
  split
  · exact h
  · rename_i absent
    exact List.pairwise_append.mpr ⟨h, List.pairwise_singleton _ _, by
      intro a ha b hb
      simp only [List.mem_singleton] at hb
      subst hb
      exact fun same => absent (same ▸ ha)⟩

theorem mem_create (turns : List Nat) (id x : Nat) :
    x ∈ create turns id ↔ x ∈ turns ∨ x = id := by
  unfold create
  split
  · rename_i held
    constructor
    · exact Or.inl
    · rintro (h | h)
      · exact h
      · subst h; exact held
  · simp

theorem nodup_map_inj {messages : List Message} (ids : (messages.map (·.id)).Nodup)
    {a b : Message} (ha : a ∈ messages) (hb : b ∈ messages) (same : a.id = b.id) : a = b := by
  induction messages with
  | nil => contradiction
  | cons head tail ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at ids
    simp only [List.mem_cons] at ha hb
    rcases ha with rfl | ha <;> rcases hb with rfl | hb
    · rfl
    · exact absurd ⟨b, hb, same.symm⟩ ids.1
    · exact absurd ⟨a, ha, same⟩ ids.1
    · exact ih ids.2 ha hb

theorem count_le_one_of_nodup {l : List Nat} (h : l.Nodup) (x : Nat) : l.count x ≤ 1 := by
  induction l with
  | nil => simp
  | cons head tail ih =>
    rw [List.nodup_cons] at h
    rw [List.count_cons]
    by_cases same : head = x
    · subst same
      rw [List.count_eq_zero.mpr h.1]
      simp
    · have : (head == x) = false := by simpa using same
      simp only [this]
      simpa using ih h.2

/-- A repeat of an identity the runtime already holds changes nothing. -/
theorem create_repeat_is_same_request (turns : List Nat) (id : Nat) (held : id ∈ turns) :
    create turns id = turns := by
  simp [create, held]

theorem update_ids (messages : List Message) (id : Nat) (f : Message → Message)
    (keeps : ∀ m, (f m).id = m.id) :
    (update messages id f).map (·.id) = messages.map (·.id) := by
  unfold update
  rw [List.map_map]
  congr 1
  funext m
  simp only [Function.comp]
  split <;> simp [keeps]

theorem mem_update {messages : List Message} {id : Nat} {f : Message → Message} {m : Message}
    (h : m ∈ update messages id f) :
    ∃ m₀ ∈ messages, m = (if m₀.id = id then f m₀ else m₀) := by
  unfold update at h
  rw [List.mem_map] at h
  obtain ⟨m₀, mem, eq⟩ := h
  exact ⟨m₀, mem, eq.symm⟩

theorem known_update (messages : List Message) (id : Nat) (f : Message → Message)
    (keeps : ∀ m, (f m).id = m.id) (x : Nat) (h : ∃ m ∈ messages, m.id = x) :
    ∃ m ∈ update messages id f, m.id = x := by
  obtain ⟨m, mem, eq⟩ := h
  refine ⟨if m.id = id then f m else m, ?_, ?_⟩
  · unfold update; exact List.mem_map.mpr ⟨m, mem, rfl⟩
  · split <;> simp_all [keeps]

theorem onReply_id (ok : Bool) (m : Message) : (onReply ok m).id = m.id := by
  unfold onReply; split <;> (try split) <;> rfl
theorem onTimeout_id (m : Message) : (onTimeout m).id = m.id := by
  unfold onTimeout; split <;> rfl
theorem onRetry_id (m : Message) : (onRetry m).id = m.id := rfl
theorem onDiscard_id (m : Message) : (onDiscard m).id = m.id := rfl
theorem onObserve_id (m : Message) : (onObserve m).id = m.id := by
  unfold onObserve; split <;> rfl

theorem init_inv : Inv init := by
  refine ⟨by simp [init], by simp [init], ⟨by simp [init], by simp [init]⟩, by simp [init, retained]⟩

/-- The retained-payload invariant across an update whose function never
leaves a pending or failed message without its payload. -/
theorem retained_update (s : State) (id : Nat) (f : Message → Message)
    (inv : retained s)
    (keeps : ∀ m ∈ s.messages, m.id = id →
      ((f m).status = .pending ∨ (f m).status = .failed) → (f m).payload = true) :
    ∀ m ∈ update s.messages id f, (m.status = .pending ∨ m.status = .failed) → m.payload = true := by
  intro m mem active
  obtain ⟨m₀, mem₀, eq⟩ := mem_update mem
  subst eq
  by_cases same : m₀.id = id
  · simp only [same, ↓reduceIte] at active ⊢
    exact keeps m₀ mem₀ same active
  · simp only [same, ↓reduceIte] at active ⊢
    exact inv m₀ mem₀ active

theorem step_inv (s s' : State) (action : Action) (inv : Inv s) (h : step s action = some s') :
    Inv s' := by
  obtain ⟨ids, turns, ⟨wire, held⟩, kept⟩ := inv
  cases action with
  | compose id =>
    simp only [step] at h
    split at h
    · contradiction
    · rename_i fresh
      cases h
      refine ⟨?_, turns, ⟨?_, ?_⟩, ?_⟩
      · simp only [List.map_append, List.map_cons, List.map_nil]
        refine List.pairwise_append.mpr ⟨ids, List.pairwise_singleton _ _, ?_⟩
        simp only [List.any_eq_true, decide_eq_true_eq, not_exists, not_and] at fresh
        intro a ha b hb
        simp only [List.mem_singleton] at hb
        subst hb
        simp only [List.mem_map] at ha
        obtain ⟨m, mem, eq⟩ := ha
        exact fun same => fresh m mem (eq.trans same)
      · intro x hx
        simp only [List.mem_append, List.mem_singleton] at hx
        rcases hx with old | new
        · obtain ⟨m, mem, eq⟩ := wire x old
          exact ⟨m, List.mem_append_left _ mem, eq⟩
        · exact ⟨⟨id, .pending, true⟩, by simp, new.symm⟩
      · intro x hx
        obtain ⟨m, mem, eq⟩ := held x hx
        exact ⟨m, List.mem_append_left _ mem, eq⟩
      · intro m mem active
        simp only [List.mem_append, List.mem_singleton] at mem
        rcases mem with old | new
        · exact kept m old active
        · subst new; rfl
  | commit id =>
    simp only [step] at h
    split at h
    · rename_i onWire
      cases h
      refine ⟨ids, create_nodup _ _ turns, ⟨?_, ?_⟩, kept⟩
      · intro x hx
        exact wire x (List.mem_of_mem_erase hx)
      · intro x hx
        rcases (mem_create _ _ _).mp hx with old | new
        · exact held x old
        · subst new; exact wire _ onWire
    · contradiction
  | refuse id =>
    simp only [step] at h
    split at h
    · cases h
      exact ⟨ids, turns, ⟨fun x hx => wire x (List.mem_of_mem_erase hx), held⟩, kept⟩
    · contradiction
  | drop id =>
    simp only [step] at h
    split at h
    · cases h
      exact ⟨ids, turns, ⟨fun x hx => wire x (List.mem_of_mem_erase hx), held⟩, kept⟩
    · contradiction
  | loseReply id ok =>
    simp only [step] at h
    split at h
    · cases h; exact ⟨ids, turns, ⟨wire, held⟩, kept⟩
    · contradiction
  | receive id ok =>
    simp only [step] at h
    split at h
    · cases h
      refine ⟨by simpa [update_ids _ _ _ (onReply_id ok)] using ids, turns,
        ⟨fun x hx => known_update _ _ _ (onReply_id ok) x (wire x hx),
         fun x hx => known_update _ _ _ (onReply_id ok) x (held x hx)⟩, ?_⟩
      apply retained_update s id _ kept
      intro m mem _ active
      unfold onReply at active ⊢
      cases ok
      · simp only [Bool.false_eq_true, ↓reduceIte] at active ⊢
        split
        · exact kept m mem (Or.inl (by assumption))
        · rename_i notPending
          simp only [notPending, ↓reduceIte] at active
          exact kept m mem (Or.inr (active.resolve_left (fun f => f)))
      · simp only [↓reduceIte] at active ⊢
        split
        · rename_i discarded
          simp only [discarded, ↓reduceIte] at active
          simp_all
        · rename_i notDiscarded
          simp only [notDiscarded, ↓reduceIte] at active
          simp at active
    · contradiction
  | timeout id =>
    simp only [step] at h
    split at h
    · cases h
      refine ⟨by simpa [update_ids _ _ _ onTimeout_id] using ids, turns,
        ⟨fun x hx => known_update _ _ _ onTimeout_id x (wire x hx),
         fun x hx => known_update _ _ _ onTimeout_id x (held x hx)⟩, ?_⟩
      apply retained_update s id _ kept
      intro m mem _ active
      unfold onTimeout at active ⊢
      split
      · exact kept m mem (Or.inl (by assumption))
      · rename_i notPending
        simp only [notPending, ↓reduceIte] at active
        exact kept m mem (Or.inr (active.resolve_left (fun f => f)))
    · contradiction
  | retry id =>
    simp only [step] at h
    split at h
    · rename_i failed
      cases h
      refine ⟨by simpa [update_ids _ _ _ onRetry_id] using ids, turns, ⟨?_, ?_⟩, ?_⟩
      · intro x hx
        simp only [List.mem_append, List.mem_singleton] at hx
        rcases hx with old | new
        · exact known_update _ _ _ onRetry_id x (wire x old)
        · subst new
          simp only [List.any_eq_true, decide_eq_true_eq] at failed
          obtain ⟨m, mem, same, _⟩ := failed
          exact known_update _ _ _ onRetry_id _ ⟨m, mem, same⟩
      · exact fun x hx => known_update _ _ _ onRetry_id x (held x hx)
      · apply retained_update s id _ kept
        intro m mem same _
        simp only [onRetry]
        simp only [List.any_eq_true, decide_eq_true_eq] at failed
        obtain ⟨m', mem', same', isFailed, payload⟩ := failed
        have : m = m' := nodup_map_inj ids mem mem' (same.trans same'.symm)
        subst this
        exact payload
    · contradiction
  | discard id =>
    simp only [step] at h
    split at h
    · cases h
      refine ⟨by simpa [update_ids _ _ _ onDiscard_id] using ids, turns,
        ⟨fun x hx => known_update _ _ _ onDiscard_id x (wire x hx),
         fun x hx => known_update _ _ _ onDiscard_id x (held x hx)⟩, ?_⟩
      apply retained_update s id _ kept
      intro m _ _ active
      simp [onDiscard] at active
    · contradiction
  | observe id =>
    simp only [step] at h
    split at h
    · cases h
      refine ⟨by simpa [update_ids _ _ _ onObserve_id] using ids, turns,
        ⟨fun x hx => known_update _ _ _ onObserve_id x (wire x hx),
         fun x hx => known_update _ _ _ onObserve_id x (held x hx)⟩, ?_⟩
      apply retained_update s id _ kept
      intro m mem _ active
      unfold onObserve at active ⊢
      split
      · rename_i failed
        simp only [failed, ↓reduceIte] at active
        simp at active
      · rename_i notFailed
        simp only [notFailed, ↓reduceIte] at active
        exact kept m mem (Or.inl (active.resolve_right (fun f => f)))
    · contradiction

theorem reachable_inv {s : State} (h : Reachable s) : Inv s := by
  induction h with
  | init => exact init_inv
  | step action _ stepped ih => exact step_inv _ _ action ih stepped

/-! ### What the model guarantees -/

/-- No duplicate turn under any interleaving of send, lost reply, timeout and
retry: the runtime holds each request identity at most once. -/
theorem turns_nodup {s : State} (h : Reachable s) : s.turns.Nodup := (reachable_inv h).turns

/-- Each authored message has at most one runtime turn. -/
theorem one_turn_per_message {s : State} (h : Reachable s) (m : Message) :
    s.turns.count m.id ≤ 1 :=
  count_le_one_of_nodup (turns_nodup h) m.id

/-- The runtime only holds requests the person authored. -/
theorem turns_are_authored {s : State} (h : Reachable s) (id : Nat) (held : id ∈ s.turns) :
    ∃ m ∈ s.messages, m.id = id := (reachable_inv h).known.2 id held

/-- The client keeps the payload of every message it has not confirmed or
discarded, so "unknown, with no retained bytes" cannot occur. -/
theorem payload_retained {s : State} (h : Reachable s) (m : Message) (mem : m ∈ s.messages)
    (active : m.status = .pending ∨ m.status = .failed) : m.payload = true :=
  (reachable_inv h).retained m mem active

theorem statusOf_of_mem {messages : List Message} {m : Message}
    (ids : (messages.map (·.id)).Nodup) (mem : m ∈ messages) :
    statusOf messages m.id = some m.status := by
  unfold statusOf
  have found : messages.find? (fun message => message.id = m.id) = some m := by
    induction messages with
    | nil => contradiction
    | cons head tail ih =>
      simp only [List.map_cons, List.nodup_cons, List.mem_map] at ids
      simp only [List.mem_cons] at mem
      rcases mem with here | there
      · subst here; simp
      · have differs : head.id ≠ m.id := fun same => ids.1 ⟨m, there, same.symm⟩
        simp [differs, ih ids.2 there]
  simp [found]

theorem statusOf_update_self {messages : List Message} {m : Message} (f : Message → Message)
    (keeps : ∀ m, (f m).id = m.id)
    (ids : (messages.map (·.id)).Nodup) (mem : m ∈ messages) :
    statusOf (update messages m.id f) m.id = some (f m).status := by
  have ids' : ((update messages m.id f).map (·.id)).Nodup := by
    rw [update_ids _ _ _ keeps]; exact ids
  have mem' : f m ∈ update messages m.id f := by
    unfold update; exact List.mem_map.mpr ⟨m, mem, by simp⟩
  have := statusOf_of_mem ids' mem'
  rw [keeps] at this
  exact this

/-- A pending message always resolves: its deadline is always enabled and
shows it failed, and from failed Retry is always offered (the payload is
kept). A reply that arrives confirms or fails it earlier. -/
theorem pending_resolves {s : State} (h : Reachable s) (m : Message) (mem : m ∈ s.messages)
    (pending : m.status = .pending) :
    ∃ s', step s (.timeout m.id) = some s' ∧
      statusOf s'.messages m.id = some .failed ∧ (step s' (.retry m.id)).isSome := by
  have inv := reachable_inv h
  have current := statusOf_of_mem inv.ids mem
  rw [pending] at current
  refine ⟨{ s with messages := update s.messages m.id onTimeout }, by simp [step, current], ?_, ?_⟩
  · rw [statusOf_update_self onTimeout onTimeout_id inv.ids mem]
    simp [onTimeout, pending]
  · have payload := inv.retained m mem (Or.inl pending)
    have mem' : onTimeout m ∈ update s.messages m.id onTimeout := by
      unfold update; exact List.mem_map.mpr ⟨m, mem, by simp⟩
    have offered : (update s.messages m.id onTimeout).any
        (fun message => message.id = m.id ∧ message.status = .failed ∧ message.payload) = true := by
      rw [List.any_eq_true]
      exact ⟨onTimeout m, mem', by simp [onTimeout, pending, payload]⟩
    simp only [step, offered, ↓reduceIte, Option.isSome_some]

/-- Nothing is stuck: every failed message offers Retry, which sends the same
identity again; a retry of a request the runtime already holds is the same
request. -/
theorem failed_offers_retry {s : State} (h : Reachable s) (m : Message) (mem : m ∈ s.messages)
    (failed : m.status = .failed) : (step s (.retry m.id)).isSome := by
  have payload := (reachable_inv h).retained m mem (Or.inr failed)
  have offered : s.messages.any
      (fun message => message.id = m.id ∧ message.status = .failed ∧ message.payload) = true := by
    rw [List.any_eq_true]
    exact ⟨m, mem, by simp [failed, payload]⟩
  simp only [step, offered, ↓reduceIte, Option.isSome_some]

/-- A failed message the runtime holds is confirmed once the client observes
it in the session, so a landed send never stays shown as failed. -/
theorem observed_failure_confirms {s : State} (h : Reachable s) (m : Message)
    (mem : m ∈ s.messages) (held : m.id ∈ s.turns) (failed : m.status = .failed) :
    ∃ s', step s (.observe m.id) = some s' ∧ statusOf s'.messages m.id = some .confirmed := by
  have inv := reachable_inv h
  have current := statusOf_of_mem inv.ids mem
  rw [failed] at current
  refine ⟨{ s with messages := update s.messages m.id onObserve }, ?_, ?_⟩
  · simp only [step]
    rw [if_pos ⟨held, current⟩]
  · rw [statusOf_update_self onObserve onObserve_id inv.ids mem]
    simp [onObserve, failed]

/-- The runtime never refuses an identity it holds: a resend of a held
identity (a retry after a lost reply, a retry after the question it answers
was claimed) can only be answered by its commit, which confirms it. -/
theorem held_resend_never_refused (s : State) (id : Nat) (held : id ∈ s.turns) :
    step s (.refuse id) = none := by
  simp [step, held]

/-- A held identity's resend commits as the same request and replies that it
is held. -/
theorem held_resend_confirms (s : State) (id : Nat) (held : id ∈ s.turns) (onWire : id ∈ s.wire) :
    ∃ s', step s (.commit id) = some s' ∧ s'.turns = s.turns ∧ (id, true) ∈ s'.replies := by
  refine ⟨{ s with wire := s.wire.erase id, turns := create s.turns id,
                   replies := s.replies ++ [(id, true)] }, by simp [step, onWire], ?_, by simp⟩
  exact create_repeat_is_same_request _ _ held

theorem retry_commit_is_idempotent (s : State) (id : Nat) (held : id ∈ s.turns) (onWire : id ∈ s.wire) :
    (step s (.commit id)).map (·.turns) = some s.turns := by
  simp [step, onWire, create_repeat_is_same_request _ _ held]

/-! ### What the person is shown -/

inductive ViewToken where
  | sending
  | delivered
  | failed
  | hidden
  deriving DecidableEq, Repr

structure View where
  shown : ViewToken
  retry : Bool
  discard : Bool
  blocksComposer : Bool
  deriving DecidableEq, Repr

/-- `mailbox` marks an answer to a mailbox question. `refused` marks a failure
that a resend under the same identity meets again: the runtime denied this
client the operation (the bridge's `permissionDenied`). Its view withholds
Retry and keeps Discard; access is the node owner's to grant. Every other
failure (a lost reply, the deadline, a full disk, an unclassified refusal)
keeps Retry, which succeeds once its cause passes. The bridge's `retryable`
flag does not decide this: it marks errors that retrying the same call may
clear unattended, and a full disk is not one, yet Retry is how its recovery
resends the message. -/
def view (status : Status) (mailbox refused : Bool) : View :=
  match status with
  | .pending => ⟨.sending, false, false, mailbox⟩
  | .confirmed => ⟨.delivered, false, false, mailbox⟩
  | .failed => ⟨.failed, !refused, true, mailbox⟩
  | .discarded => ⟨.hidden, false, false, false⟩

/-- Every view is progressing (sending), settled (delivered or hidden), or
names its next action: Discard, and Retry unless the runtime refused this
client. -/
theorem view_progresses_or_acts (status : Status) (mailbox refused : Bool) :
    (view status mailbox refused).shown = .sending ∨
      (view status mailbox refused).shown = .delivered ∨
      (view status mailbox refused).shown = .hidden ∨
      ((view status mailbox refused).discard = true ∧
        ((view status mailbox refused).retry = true ∨ refused = true)) := by
  cases status <;> cases refused <;> simp [view]

/-- Retry is offered exactly for a failed message the runtime did not refuse. -/
theorem retry_offered_iff (status : Status) (mailbox refused : Bool) :
    (view status mailbox refused).retry = true ↔ status = .failed ∧ refused = false := by
  cases status <;> cases refused <;> simp [view]

/-- A blocked composer is a mailbox answer that is sending, delivered, or
failed with Discard offered; a chat send never blocks the composer. -/
theorem composer_blocks_only_mailbox (status : Status) (mailbox refused : Bool)
    (blocks : (view status mailbox refused).blocksComposer = true) :
    mailbox = true ∧ ((view status mailbox refused).shown = .sending ∨
      (view status mailbox refused).shown = .delivered ∨
      (view status mailbox refused).discard = true) := by
  cases status <;> cases mailbox <;> simp_all [view]

/-! ### Conformance cases -/

def statusToken : Status → String
  | .pending => "pending"
  | .confirmed => "confirmed"
  | .failed => "failed"
  | .discarded => "discarded"

def viewTokenString : ViewToken → String
  | .sending => "sending"
  | .delivered => "delivered"
  | .failed => "failed"
  | .hidden => "hidden"

def allStatuses : List Status := [.pending, .confirmed, .failed, .discarded]

def viewCasesJson : Json :=
  toJson (allStatuses.flatMap fun status => [false, true].flatMap fun mailbox =>
    [false, true].map fun refused =>
    let shown := view status mailbox refused
    Json.mkObj [("status", toJson (statusToken status)), ("mailbox", toJson mailbox),
      ("refused", toJson refused),
      ("view", toJson (viewTokenString shown.shown)), ("retry", toJson shown.retry),
      ("discard", toJson shown.discard), ("blocksComposer", toJson shown.blocksComposer)])

def actionJson : Action → Json
  | .compose id => Json.mkObj [("kind", toJson "compose"), ("id", toJson id)]
  | .commit id => Json.mkObj [("kind", toJson "commit"), ("id", toJson id)]
  | .refuse id => Json.mkObj [("kind", toJson "refuse"), ("id", toJson id)]
  | .drop id => Json.mkObj [("kind", toJson "drop"), ("id", toJson id)]
  | .loseReply id ok => Json.mkObj [("kind", toJson "loseReply"), ("id", toJson id), ("ok", toJson ok)]
  | .receive id ok => Json.mkObj [("kind", toJson "receive"), ("id", toJson id), ("ok", toJson ok)]
  | .timeout id => Json.mkObj [("kind", toJson "timeout"), ("id", toJson id)]
  | .retry id => Json.mkObj [("kind", toJson "retry"), ("id", toJson id)]
  | .discard id => Json.mkObj [("kind", toJson "discard"), ("id", toJson id)]
  | .observe id => Json.mkObj [("kind", toJson "observe"), ("id", toJson id)]

/-- The actions on one message `1` the generated traces explore. -/
def traceActions : List Action :=
  [.compose 1, .commit 1, .refuse 1, .drop 1, .loseReply 1 true, .loseReply 1 false,
   .receive 1 true, .receive 1 false, .timeout 1, .retry 1, .discard 1, .observe 1]

/-- Every enabled trace of `depth` actions from `s`, with its end state. -/
def traces : Nat → State → List (List Action × State)
  | 0, s => [([], s)]
  | depth + 1, s =>
    ([], s) :: traceActions.flatMap fun action =>
      match step s action with
      | none => []
      | some s' => (traces depth s').map fun (rest, final) => (action :: rest, final)

/-- Generated traces: every enabled sequence of up to `traceDepth` actions on
one message, with the message's status, its payload and the runtime's turn
count after each action. -/
def traceDepth : Nat := 7

def snapshotJson (s : State) : Json :=
  Json.mkObj [
    ("status", match statusOf s.messages 1 with
      | some status => toJson (statusToken status)
      | none => Json.null),
    ("payload", toJson ((s.messages.find? (·.id = 1)).map (·.payload) |>.getD false)),
    ("turns", toJson (s.turns.count 1)),
    ("wire", toJson (s.wire.count 1)),
    ("retryOffered", toJson (step s (.retry 1)).isSome)]

def replay : State → List Action → List Json
  | _, [] => []
  | s, action :: rest =>
    match step s action with
    | none => []
    | some s' => snapshotJson s' :: replay s' rest

def maximalTraces : List (List Action) :=
  ((traces traceDepth init).filter fun (actions, _) =>
    actions.length = traceDepth ∨
      traceActions.all fun action => (step (actions.foldl (fun s a => (step s a).getD s) init) action).isNone).map (·.1)

def traceCasesJson : Json :=
  toJson (maximalTraces.map fun actions =>
    Json.mkObj [("actions", toJson (actions.map actionJson)),
      ("after", toJson (replay init actions))])

def casesJson : String :=
  (Json.mkObj [("views", viewCasesJson), ("traces", traceCasesJson)]).compress

end ClientOptimisticSend
