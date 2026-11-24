import Mathlib.Data.List.Basic

/-! Whether a store client sends a request body gzipped.

A host that inflates gzip request bodies says so on every answer
(`Accept-Encoding: gzip`, which RFC 7694 lets a response carry). A
client remembers, per endpoint, what that endpoint's latest answer said, and
gzips a body only when it is large and the endpoint's latest answer accepted
gzip. A host that never said so, an older Gents host or a plain DefraDB
endpoint, which answers a gzipped body with 400, never receives one. -/

namespace RequestEncoding

/-- Bodies of at most this many bytes are sent as they are: compressing them
saves too little to pay for. -/
def threshold : Nat := 8 * 1024

/-- What a client knows of an endpoint after an answer: whether that answer
accepted gzip request bodies. -/
def remember (_known : Bool) (advertised : Bool) : Bool := advertised

/-- What a client knows of an endpoint after its answers so far, oldest first.
An endpoint not yet heard from has accepted nothing. -/
def known (answers : List Bool) : Bool := answers.foldl remember false

def gzipBody (accepts : Bool) (length : Nat) : Bool := accepts && decide (threshold < length)

private theorem fold_none (answers : List Bool) :
    ∀ start : Bool, start = false → true ∉ answers → answers.foldl remember start = false := by
  induction answers with
  | nil => intro start h _; simpa using h
  | cons answer rest ih =>
    intro _ _ never
    have : answer = false := by
      cases answer
      · rfl
      · exact absurd (List.mem_cons_self _ _) never
    exact ih answer this (fun h => never (List.mem_cons_of_mem _ h))

/-- A host none of whose answers accepted gzip never receives a gzipped body. -/
theorem a_host_that_never_accepted_receives_no_gzip (answers : List Bool) (length : Nat)
    (never : true ∉ answers) : gzipBody (known answers) length = false := by
  simp [gzipBody, known, fold_none answers false rfl never]

/-- A host whose latest answer stopped accepting gzip receives none. -/
theorem a_host_that_stops_accepting_receives_no_gzip (answers : List Bool) (length : Nat) :
    gzipBody (known (answers ++ [false])) length = false := by
  simp [known, remember, gzipBody]

theorem a_small_body_is_sent_plain (accepts : Bool) (length : Nat) (small : length ≤ threshold) :
    gzipBody accepts length = false := by
  simp [gzipBody, Nat.not_lt.mpr small]

end RequestEncoding
