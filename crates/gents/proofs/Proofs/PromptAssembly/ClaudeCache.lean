import Proofs.Basic

/-!
# Claude Code prompt-cache placement

Claude Code 2.1.311 places at most one message breakpoint and normalizes user
content on the way to the wire (`dsn`, `cfr`, `bcr`, `wcr`, `Gfn`, `EXe` in
`chunk-ps7vnt4g.js`). Every breakpoint uses the TTL `Tbe` chooses. GrowthBook
flags are evaluated at their shipped defaults (`tengu_tidy_frost` and
`tengu_stellar_drift` off, `tengu_prompt_cache_1h_config` on its built-in
allowlist): the client cannot observe a server-side assignment. The
second-breakpoint features (`divergesAtMessageId`, cache keepalive, fork pins)
and mid-conversation system text are not reachable from a Gents request, which
hoists system rows into `system`. The one system row a request carries is the
empty per-turn effort row (`ClaudeBody.perTurnEffort`); `dsn` skips every
`api_system` row when it picks the breakpoint and marks one only when its
content is a non-blank string, so the empty row is never marked.
-/

namespace PromptAssembly.ClaudeCache

inductive Ttl where
  | fiveMinutes
  | oneHour
  deriving DecidableEq, Repr

/-- Claude Code `Tbe`: a subscriber's main-thread request (query sources
`repl_main_thread*`, `sdk`, `auto_mode`, `memdir_relevance`) caches for an hour
unless the account is drawing paid overage; every other request keeps the
five-minute default. A one-hour breakpoint also sends
`extended-cache-ttl-2025-04-11`. -/
def ttl (subscriber mainThread usingOverage : Bool) : Ttl :=
  if subscriber && mainThread && !usingOverage then .oneHour else .fiveMinutes

/-- ECMAScript `String.prototype.trim` whitespace: WhiteSpace and LineTerminator. -/
def jsWhitespace (c : Char) : Bool :=
  let n := c.toNat
  n == 0x09 || n == 0x0A || n == 0x0B || n == 0x0C || n == 0x0D || n == 0x20 ||
    n == 0xA0 || n == 0x1680 || (0x2000 ≤ n && n ≤ 0x200A) || n == 0x2028 ||
    n == 0x2029 || n == 0x202F || n == 0x205F || n == 0x3000 || n == 0xFEFF

def blank (text : String) : Bool := text.toList.all jsWhitespace

inductive Role where
  | user
  | assistant
  /-- An `api_system` row (`role: "system"`). -/
  | system
  deriving DecidableEq, Repr

inductive Block where
  | text (text : String)
  | thinking
  | redactedThinking
  | fallback
  /-- Any other block: `tool_use`, `tool_result`, `image`, `document`, server
  tool blocks. -/
  | other (kind : String)
  deriving DecidableEq, Repr

inductive Content where
  | str (text : String)
  | blocks (blocks : List Block)
  deriving DecidableEq, Repr

structure Message where
  role : Role
  content : Content
  deriving DecidableEq, Repr

def blankText : Block → Bool
  | .text text => blank text
  | _ => false

def noContent : String := "(no content)"

/-- `Gfn`: user block content loses its blank text blocks; content left empty by
that removal becomes the `(no content)` text. -/
def userBlocks (blocks : List Block) : List Block :=
  if blocks.any blankText then
    match blocks.filter (fun block => !blankText block) with
    | [] => [.text noContent]
    | kept => kept
  else blocks

/-- `EXe` with `tengu_stellar_drift` off. -/
def stampable : Block → Bool
  | .text text => !text.isEmpty
  | .thinking | .redactedThinking | .fallback => false
  | .other _ => true

/-- `lsn` and the `api_system` skip in `dsn`: an assistant message is skipped
when its last block cannot carry a breakpoint, and a system row is always
skipped. -/
def eligible (message : Message) : Bool :=
  match message.role, message.content with
  | .system, _ => false
  | .user, _ => true
  | .assistant, .str _ => true
  | .assistant, .blocks blocks => blocks.getLast?.any stampable

def lastEligibleBelow (messages : List Message) (bound : Nat) : Option Nat :=
  ((List.range (min bound messages.length)).filter
    (fun i => messages[i]?.any eligible)).getLast?

/-- `dsn`: the last eligible message, or the one before it when the request must
not write the cache. -/
def markerIndex (messages : List Message) (skipWrite : Bool) : Option Nat :=
  let last := lastEligibleBelow messages messages.length
  if skipWrite then last.bind (lastEligibleBelow messages) else last

structure Placed where
  role : Role
  /-- `none` keeps a string body as a string. -/
  blocks : Option (List Block)
  text : Option String
  /-- Index of the block that carries `cache_control`. -/
  marker : Option Nat
  deriving DecidableEq, Repr

def placeOne (marked : Bool) (message : Message) : Placed :=
  match message.role, message.content with
  | role, .str text =>
      if marked then { role, blocks := some [.text text], text := none, marker := some 0 }
      else { role, blocks := none, text := some text, marker := none }
  | .user, .blocks blocks =>
      let blocks := userBlocks blocks
      { role := .user, blocks := some blocks, text := none,
        marker := if marked && !blocks.isEmpty then some (blocks.length - 1) else none }
  | .assistant, .blocks blocks =>
      { role := .assistant, blocks := some blocks, text := none,
        marker := if marked && blocks.getLast?.any stampable then some (blocks.length - 1)
          else none }
  | .system, .blocks blocks => { role := .system, blocks := some blocks, text := none, marker := none }

def placeFrom (target : Option Nat) : Nat → List Message → List Placed
  | _, [] => []
  | i, message :: rest => placeOne (target == some i) message :: placeFrom target (i + 1) rest

/-- `cfr`: wire messages with the breakpoint placed when caching is on. -/
def place (caching skipWrite : Bool) (messages : List Message) : List Placed :=
  placeFrom (if caching then markerIndex messages skipWrite else none) 0 messages

theorem placeOne_unmarked (message : Message) : (placeOne false message).marker = none := by
  unfold placeOne
  split <;> simp

theorem placeFrom_unmarked (target : Option Nat) (i : Nat) (messages : List Message)
    (h : ∀ j, target = some j → j < i) :
    (placeFrom target i messages).filter (·.marker.isSome) = [] := by
  induction messages generalizing i with
  | nil => rfl
  | cons message rest ih =>
    have hne : (target == some i) = false := by
      cases target with
      | none => rfl
      | some j => have := h j rfl; simp; omega
    simp only [placeFrom, List.filter_cons, hne, placeOne_unmarked, Option.isSome_none,
      Bool.false_eq_true, if_false]
    exact ih (i + 1) (fun j hj => by have := h j hj; omega)

theorem placeFrom_marks_at_most_one (target : Option Nat) (i : Nat) (messages : List Message) :
    ((placeFrom target i messages).filter (·.marker.isSome)).length ≤ 1 := by
  induction messages generalizing i with
  | nil => simp [placeFrom]
  | cons message rest ih =>
    by_cases hi : target = some i
    · subst hi
      simp only [placeFrom, List.filter_cons]
      rw [placeFrom_unmarked (some i) (i + 1) rest (fun j hj => by cases hj; omega)]
      split <;> simp
    · have hne : (target == some i) = false := by simpa using hi
      simp only [placeFrom, List.filter_cons, hne, placeOne_unmarked, Option.isSome_none,
        Bool.false_eq_true, if_false]
      exact ih (i + 1)

theorem place_marks_at_most_one (caching skipWrite : Bool) (messages : List Message) :
    ((place caching skipWrite messages).filter (·.marker.isSome)).length ≤ 1 :=
  placeFrom_marks_at_most_one _ 0 messages

theorem lastEligibleBelow_eligible (messages : List Message) (bound i : Nat)
    (h : lastEligibleBelow messages bound = some i) : messages[i]?.any eligible = true := by
  unfold lastEligibleBelow at h
  have hmem := List.mem_of_getLast? h
  simpa using (List.mem_filter.mp hmem).2

theorem eligible_is_not_system (message : Message) (h : eligible message = true) :
    message.role ≠ .system := by
  obtain ⟨role, content⟩ := message
  cases role <;> simp_all [eligible]

/-- The breakpoint never lands on a system row, so the per-turn effort row is
sent exactly as built. -/
theorem markerIndex_skips_system (messages : List Message) (skipWrite : Bool) (i : Nat)
    (h : markerIndex messages skipWrite = some i) :
    ∃ m, messages[i]? = some m ∧ m.role ≠ .system := by
  have hi : messages[i]?.any eligible = true := by
    unfold markerIndex at h
    split at h
    · rename_i hs
      obtain ⟨j, _, hj⟩ := Option.bind_eq_some.mp h
      exact lastEligibleBelow_eligible _ _ _ hj
    · exact lastEligibleBelow_eligible _ _ _ h
  cases hm : messages[i]? with
  | none => simp [hm] at hi
  | some m => exact ⟨m, rfl, eligible_is_not_system m (by simpa [hm] using hi)⟩

theorem marked_assistant_block_stampable (marked : Bool) (blocks : List Block) (k : Nat)
    (h : (placeOne marked ⟨.assistant, .blocks blocks⟩).marker = some k) :
    ∃ block, blocks[k]? = some block ∧ stampable block = true := by
  simp only [placeOne] at h
  split at h
  · rename_i hm
    cases h
    simp only [Bool.and_eq_true] at hm
    obtain ⟨_, hlast⟩ := hm
    cases hb : blocks.getLast? with
    | none => simp [hb] at hlast
    | some last =>
      rw [hb] at hlast
      refine ⟨last, ?_, by simpa using hlast⟩
      rw [List.getLast?_eq_getElem?] at hb
      exact hb
  · cases h

theorem user_blocks_have_no_blank_text (blocks : List Block) (h : blocks.any blankText = true) :
    (userBlocks blocks).all (fun block => !blankText block) = true := by
  unfold userBlocks
  rw [if_pos h]
  cases hf : blocks.filter (fun block => !blankText block) with
  | nil => native_decide
  | cons first rest =>
    show (first :: rest).all (fun block => !blankText block) = true
    rw [← hf]
    exact List.all_eq_true.mpr (fun block hb => (List.mem_filter.mp hb).2)

end PromptAssembly.ClaudeCache
