/-!
# MemFileExtents: where every byte of an in-memory file lives in its chain

THE STORY. A file that regolith keeps in memory (`MemFile`, behind `MemEnv`
and the OPFS mirror) holds its bytes in an "extent": a chain of buffers
called segments (`src/env/mem_file/extent.rs`). A file starts as one
segment. When an append runs out of room it links one more segment at the
end of the chain (`Extent::reserve`), as large as all the room before it, so
a growing file never copies the bytes it already has. Every read, write and
zeroing goes through one walk (`Extent::pieces`): it cuts the byte range it
is given into one piece per segment the range crosses, and hands each piece
to a copy.

Tiny example. The chain has segments of 64, 64 and 128 bytes, so the room
is 256 bytes: bytes 0..63 live in segment 0, bytes 64..127 in segment 1,
bytes 128..255 in segment 2. A read of 100 bytes from byte 50 is cut into
three pieces: 14 bytes from position 50 of segment 0, then 64 bytes from
position 0 of segment 1, then 22 bytes from position 0 of segment 2. The
three pieces fill the 100 bytes of the reader's buffer at offsets 0, 14
and 78, one after the other.

What must never happen: a piece that runs past the end of its segment (a
copy into memory that belongs to nobody), two pieces that cover the same
byte of the range or a byte of the range no piece covers (a read that
returns garbage or a write that lands twice), a link that moves bytes a
reader is copying, and a chain that grows by tiny steps into thousands of
segments.

WHAT IS PROVED, for every chain of segments (zero-length ones included, as
`Extent::empty` starts with one) and every range:

1. `start_eq_room_take`, `start_succ`, `start_length`: segment k starts at
   the sum of the lengths before it, each segment starts where the one
   before it ends, and the last one ends at the room (`Extent::capacity`).
2. `locate_iff`, `locate_total`, `place_unique`: a byte below the room lies
   in exactly one segment, at exactly one position.
3. `walk_spec`, `pieces_exact`: the walk, written step for step as the Rust
   loop, cuts a range that fits the room into pieces that each lie inside
   their segment, cover the range from its first byte to its last with no
   gap and no overlap, in order, and land each byte where the chain holds
   it; it never takes the early `return`.
4. `pieces_short`: a range that runs past the room makes the walk take the
   early `return`, after pieces that cover exactly the bytes the chain
   holds, each still inside its segment: it touches nothing past the chain.
5. `reserve_links`, `reserve_reaches`, `reserve_moves_nothing`: reserving
   room for `end` bytes leaves the chain as it is or links one segment at
   least as large as the room before it and at least `MIN_CAPACITY`; the
   room then reaches `end`; and the pieces of any range below the old room
   are the same after the link, so a reader below a length it loaded is
   never disturbed by a writer that grows the file.
6. `grow_doubling`, `grow_few`: a chain grown by reserves from one segment
   has room at least `64 * 2^(j - 1)` once it has linked `j >= 1` segments,
   so a chain whose room is below `64 * 2^m` has at most `m + 1` segments:
   a file of n bytes is about log2(n) segments.

Backs `proofs/tla/MemFileExtents.tla`, which checks the same chain for a
few small sizes with every interleaving of writers and readers; here the
arithmetic is proved for every size, with no thread in sight.

The names `at_` and `end_` are the Rust variables `at` and `end`: both are
words Lean keeps for itself, so each carries an underscore here.
-/

-- Everything below lives under this name, so it cannot clash with other files.
namespace Regolith.MemFileExtents

/-- The room of a chain: the lengths of its segments added up. A chain is
the list of its segments' lengths, first segment first (`Segment::bytes`,
walked by `Extent::segments`); this is `Extent::capacity`. -/
def room : List Nat → Nat
  -- No segment: no room.
  | [] => 0
  -- The first segment's length, then the room of the rest.
  | l :: ls => l + room ls

/-- Where segment `k` of a chain starts: the number of bytes held by the
segments before it. -/
def start : List Nat → Nat → Nat
  -- Past the end of the chain nothing is in front, so 0.
  | [], _ => 0
  -- The first segment starts at byte 0.
  | _ :: _, 0 => 0
  -- Segment `k + 1` starts after the first segment, at the start of
  -- segment `k` of the rest.
  | l :: ls, k + 1 => l + start ls k

/-- The length of segment `k` of a chain, 0 past its end. -/
def lenAt : List Nat → Nat → Nat
  -- Past the end of the chain: no segment, no bytes.
  | [], _ => 0
  -- Segment 0 is the first one.
  | l :: _, 0 => l
  -- Segment `k + 1` is segment `k` of the rest.
  | _ :: ls, k + 1 => lenAt ls k

/-- Segment `k` starts at the sum of the lengths of the `k` segments before
it. -/
theorem start_eq_room_take :
    -- The statement: for every chain and every k, the start of segment k is the room of the first
    -- k segments.
    ∀ (segs : List Nat) (k : Nat), start segs k = room (segs.take k) := by
  -- Take the chain.
  intro segs
  -- Induction on the chain: first the empty one, then one segment more.
  induction segs with
  -- Case: the empty chain.
  | nil =>
    -- Take k; whether it is 0 or not, both sides are 0 by definition.
    intro k; cases k <;> rfl
  -- Case: a first segment l in front of the chain ls, with the claim for ls (ih) known.
  | cons l ls ih =>
    -- Take k.
    intro k
    -- Either k is 0 or it is one more than some k.
    cases k with
    -- k = 0: both sides are 0 by definition.
    | zero => rfl
    -- k + 1: unfold both sides one segment and use the claim for the rest.
    | succ k => simp [start, room, List.take, ih k]

/-- The first segment starts at byte 0, whatever the chain. -/
theorem start_zero :
    -- The statement: the start of segment 0 is 0.
    ∀ segs : List Nat, start segs 0 = 0 := by
  -- Take the chain; empty or not, the start of segment 0 is 0 by definition.
  intro segs; cases segs <;> rfl

/-- Each segment starts exactly where the one before it ends: no byte
between two segments belongs to neither, and none to both. -/
theorem start_succ :
    -- The statement: for every segment k of the chain, segment k + 1 starts at the start of
    -- segment k plus its length.
    ∀ (segs : List Nat) (k : Nat), k < segs.length →
      start segs (k + 1) = start segs k + lenAt segs k := by
  -- Take the chain.
  intro segs
  -- Induction on the chain.
  induction segs with
  -- Case: the empty chain has no segment k, so the assumption is false.
  | nil =>
    -- Take k and the assumption k < 0, which simplification turns into False.
    intro k h; simp at h
  -- Case: a first segment l in front of ls, with the claim for ls (ih) known.
  | cons l ls ih =>
    -- Take k and the assumption that the chain has a segment k.
    intro k h
    -- Either k is 0 or one more than some k.
    cases k with
    -- k = 0: segment 1 starts after the first segment, which starts at 0.
    | zero => simp [start, lenAt, start_zero]
    -- k + 1: the claim for the rest, shifted by the first segment.
    | succ k =>
      -- The rest has a segment k, so the claim holds for it.
      have := ih k (by simp at h; omega)
      -- Unfold one segment on every side, here and in the fact just got.
      simp only [start, lenAt] at *
      -- Arithmetic: add l to both sides of the fact.
      omega

/-- The segment after the last one starts at the room: the chain's segments
hold exactly its room, end to end. -/
theorem start_length :
    -- The statement: the start one past the last segment is the room.
    ∀ segs : List Nat, start segs segs.length = room segs := by
  -- Take the chain.
  intro segs
  -- Induction on the chain.
  induction segs with
  -- Case: the empty chain, where both sides are 0.
  | nil => rfl
  -- Case: one segment more; unfold one segment and use the claim for the rest.
  | cons l ls ih => simp [start, room, ih]

/-- Which segment holds byte `x`, and where in it: the walk the Rust makes,
subtracting each segment's length from `x` until `x` falls inside one.
`none` when `x` is at or past the room. -/
def locate : List Nat → Nat → Option (Nat × Nat)
  -- Past the last segment: no segment holds the byte.
  | [], _ => none
  -- Inside the first segment: segment 0, at position x. Otherwise look in the rest for x minus
  -- the first segment's length, and count one segment more.
  | l :: ls, x => if x < l then some (0, x) else (locate ls (x - l)).map fun kp => (kp.1 + 1, kp.2)

/-- `locate` finds segment `k`, position `p` exactly when `p` is inside
segment `k` and segment `k` starts `p` bytes before `x`. -/
theorem locate_iff :
    -- The statement: for every chain, byte, segment and position, locate answers (k, p) if and only
    -- if k is a segment of the chain, p is below its length, and its start plus p is x.
    ∀ (segs : List Nat) (x k p : Nat),
      locate segs x = some (k, p) ↔ (k < segs.length ∧ p < lenAt segs k ∧ start segs k + p = x) := by
  -- Take the chain.
  intro segs
  -- Induction on the chain.
  induction segs with
  -- Case: the empty chain, where locate says none and no segment k exists: both sides false.
  | nil => intro x k p; simp [locate]
  -- Case: a first segment l in front of ls, with the claim for ls (ih) known.
  | cons l ls ih =>
    -- Take the byte, the segment and the position.
    intro x k p
    -- Either the byte is inside the first segment or it is past it.
    by_cases hx : x < l
    -- Inside the first segment:
    ·
      -- either k is 0 or one more than some k;
      cases k with
      -- k = 0: unfold, and both sides say p = x; arithmetic finishes.
      | zero => simp [locate, hx, lenAt, start]; omega
      -- k + 1: unfold; locate said segment 0, and segment k + 1 starts past x: both false.
      | succ k => simp [locate, hx, lenAt, start]; omega
    -- Past the first segment:
    ·
      -- either k is 0 or one more than some k;
      cases k with
      -- k = 0: unfold; locate never says segment 0, and x is not inside it: both false.
      | zero => simp [locate, hx, lenAt, start]; omega
      -- k + 1: unfold one segment and use the claim for the rest at x - l.
      | succ k =>
        -- Unfold one segment on both sides and rewrite the rest by the claim for ls.
        simp [locate, hx, lenAt, start, ih]
        -- Arithmetic: x - l plus l is x, since x is at least l.
        omega

/-- Every byte below the room lies in some segment, at some position. -/
theorem locate_total :
    -- The statement: for every chain and every byte below its room, some segment k and position p
    -- inside it hold the byte.
    ∀ (segs : List Nat) (x : Nat), x < room segs →
      ∃ k p, k < segs.length ∧ p < lenAt segs k ∧ start segs k + p = x := by
  -- Take the chain.
  intro segs
  -- Induction on the chain.
  induction segs with
  -- Case: the empty chain has room 0, so no byte is below it: the assumption is false.
  | nil => intro x h; simp [room] at h
  -- Case: a first segment l in front of ls, with the claim for ls (ih) known.
  | cons l ls ih =>
    -- Take the byte and the fact that it is below the room.
    intro x h
    -- Either it is inside the first segment or past it.
    by_cases hx : x < l
    -- Inside: segment 0, position x.
    · exact ⟨0, x, by simp, by simp [lenAt]; exact hx, by simp [start]⟩
    -- Past it: the rest holds byte x - l at some (k, p), so this chain holds x at (k + 1, p).
    · obtain ⟨k, p, hk, hp, hs⟩ := ih (x - l) (by simp [room] at h; omega)
      -- Segment k + 1 exists, p is inside it, and it starts l bytes later than segment k of ls.
      exact ⟨k + 1, p, by simp; omega, by simp [lenAt]; exact hp, by simp [start]; omega⟩

/-- No byte lies in two places: two segments and positions that both hold
byte `x` are the same segment and the same position. A zero-length segment
holds no byte at all, so it never competes. -/
theorem place_unique (segs : List Nat) (x k₁ p₁ k₂ p₂ : Nat)
    -- The first place holds x,
    (h₁ : k₁ < segs.length ∧ p₁ < lenAt segs k₁ ∧ start segs k₁ + p₁ = x)
    -- and so does the second.
    (h₂ : k₂ < segs.length ∧ p₂ < lenAt segs k₂ ∧ start segs k₂ + p₂ = x) :
    -- The statement: the two places are one.
    k₁ = k₂ ∧ p₁ = p₂ := by
  -- locate answers the first place,
  have e₁ := (locate_iff segs x k₁ p₁).mpr h₁
  -- and it answers the second place too.
  have e₂ := (locate_iff segs x k₂ p₂).mpr h₂
  -- Put the first answer in place of locate in the second: some (k₁, p₁) = some (k₂, p₂).
  rw [e₁] at e₂
  -- Two equal answers have equal parts.
  simp at e₂
  -- That is the goal.
  exact e₂

/-- One call the walk makes to its copy: `f(base + pos, off, take)` in
`Extent::pieces`. -/
structure Piece where
  /-- Which segment the piece is in, 0 for the first. -/
  seg : Nat
  /-- Where in that segment it starts (`at` when the call is made). -/
  pos : Nat
  /-- Where in the range it starts (`done` when the call is made). -/
  off : Nat
  /-- How many bytes it covers (`take`). -/
  take : Nat
  -- Two pieces can be compared, so the examples below can be checked.
  deriving DecidableEq

/-- `Extent::pieces` (extent.rs:189-215), step for step. `n` is the length
of the range; the chain is the segments from the current one on; `k` is the
current segment's number, `at_` the Rust `at`, `done` the Rust `done`. The
answer is the list of calls the loop makes, and whether it left by the
early `return` (the chain ended while `done < n`). The empty chain stands
for "past the last segment": a real extent always has a first one. -/
def walk (n : Nat) : List Nat → Nat → Nat → Nat → List Piece × Bool
  -- No segment left: the loop has stopped; it left by the `return` exactly when bytes were
  -- still owed (`done < n`), and normally otherwise.
  | [], _, _, done => ([], decide (done < n))
  -- The current segment has `len` bytes and `rest` follows it.
  | len :: rest, k, at_, done =>
    -- `while done < n`:
    if done < n then
      -- `if at < len`: the range starts inside this segment,
      if at_ < len then
        -- so after one call of `take = min(n - done, len - at)` bytes, `done += take`,
        -- `at = 0`, and the loop goes on in the next segment;
        let r := walk n rest (k + 1) 0 (done + min (n - done) (len - at_))
        -- the call is `f(base + at, done, take)`, then come the next segment's calls.
        (⟨k, at_, done, min (n - done) (len - at_)⟩ :: r.1, r.2)
      -- `else`: the range starts past this segment,
      else
        -- so `at -= len` and the loop goes on in the next segment, with no call.
        walk n rest (k + 1) (at_ - len) done
    -- The loop's test failed: every byte is done, and it leaves normally.
    else
      -- No more calls, and no `return`.
      ([], false)

/-- The walk as `copy_out`, `copy_in` and `zero` start it: `n` bytes from
byte `at_` of the chain, from the first segment, with nothing done yet. -/
def pieces (n : Nat) (segs : List Nat) (at_ : Nat) : List Piece × Bool :=
  -- Segment 0, `done = 0`.
  walk n segs 0 at_ 0

-- The story's example: 100 bytes from byte 50 of the chain [64, 64, 128] are three pieces,
-- 14 + 64 + 22 bytes at range offsets 0, 14 and 78, and the walk leaves normally.
example : pieces 100 [64, 64, 128] 50 = ([⟨0, 50, 0, 14⟩, ⟨1, 0, 14, 64⟩, ⟨2, 0, 78, 22⟩], false) := by
  -- Run the walk and compare.
  rfl

-- The same with an empty first segment, as a file that started empty has: it is skipped.
example : pieces 100 [0, 64, 64, 128] 50 = ([⟨1, 50, 0, 14⟩, ⟨2, 0, 14, 64⟩, ⟨3, 0, 78, 22⟩], false) := by
  -- Run the walk and compare.
  rfl

-- A range past the room: the walk covers the 78 bytes the chain holds and takes the `return`.
example : pieces 100 [64, 64] 50 = ([⟨0, 50, 0, 14⟩, ⟨1, 0, 14, 64⟩], true) := by
  -- Run the walk and compare.
  rfl

/-- `Covers ps a b`: the pieces' range offsets run from `a` to `b` with no
gap and no overlap: the first starts at `a`, each next one starts where the
one before it ended, the last one ends at `b`, and each covers at least one
byte. -/
def Covers : List Piece → Nat → Nat → Prop
  -- No piece: the stretch is empty, so it starts where it ends.
  | [], a, b => a = b
  -- A first piece starts at a and covers at least one byte, and the rest cover from its end on.
  | p :: ps, a, b => p.off = a ∧ 0 < p.take ∧ Covers ps (a + p.take) b

/-- Once every byte is done, the walk makes no call and leaves normally,
whatever chain is left. -/
theorem walk_done (n : Nat) (segs : List Nat) (k at_ done : Nat) (h : n ≤ done) :
    -- The statement: no call, no `return`.
    walk n segs k at_ done = ([], false) := by
  -- Either no segment is left or one is.
  cases segs with
  -- No segment: unfold; `done < n` is false, which arithmetic shows.
  | nil => simp [walk]; omega
  -- A segment: unfold; the loop's test `done < n` fails, which arithmetic shows.
  | cons l ls => simp [walk]; intro h'; omega

/-- `Inside segs k at_ done p`: piece `p` is in segment `j` of the chain
(number `k + j` overall), runs no further than that segment's end, and the
byte where it starts in the chain is the byte where it starts in the range,
counted from where the walk was. -/
def Inside (segs : List Nat) (k at_ done : Nat) (p : Piece) : Prop :=
  -- Some segment j of this chain holds the piece, whole, at the right byte.
  ∃ j, p.seg = k + j ∧ j < segs.length ∧ p.pos + p.take ≤ lenAt segs j ∧
    start segs j + p.pos + done = at_ + p.off

/-- What the walk does, for every chain and every range: its pieces cover
the range from `done` on without gap or overlap, for as many bytes as the
chain holds from `at_` on, up to `n`; it takes the `return` exactly when the
chain holds fewer than the bytes still owed; and every piece lies inside a
segment, at the byte where the chain holds it. -/
theorem walk_spec (n : Nat) :
    -- The statement: for every chain, segment number, `at` and `done` (no more than n),
    ∀ (segs : List Nat) (k at_ done : Nat), done ≤ n →
      -- the pieces cover from done for min(n - done, room - at) bytes,
      Covers (walk n segs k at_ done).1 done (done + min (n - done) (room segs - at_)) ∧
      -- the `return` is taken exactly when the chain holds fewer than n - done bytes from at on,
      ((walk n segs k at_ done).2 = true ↔ room segs - at_ < n - done) ∧
      -- and every piece is inside a segment of the chain.
      ∀ p ∈ (walk n segs k at_ done).1, Inside segs k at_ done p := by
  -- Take the chain.
  intro segs
  -- Induction on the chain: the walk leaves one segment behind at each step.
  induction segs with
  -- Case: no segment left.
  | nil =>
    -- Take the rest of the inputs.
    intro k at_ done hd
    -- Unfold: no piece, and the stretch is empty since room is 0; what is left is the `return`.
    simp [walk, Covers, room]
    -- Arithmetic: done < n exactly when 0 < n - done.
    omega
  -- Case: a current segment l in front of the rest ls, with the claim for ls (ih) known.
  | cons l ls ih =>
    -- Take the rest of the inputs.
    intro k at_ done hd
    -- Either bytes are still owed, or every byte is done.
    by_cases hn : done < n
    -- Bytes are owed:
    ·
      -- either the range starts inside this segment, or past it.
      by_cases ha : at_ < l
      -- Inside: one call, then the rest of the chain from its first byte.
      ·
        -- The claim for the rest, at `at = 0` and `done + take`, which is still at most n.
        have ⟨hc, hf, hp⟩ := ih (k + 1) 0 (done + min (n - done) (l - at_)) (by omega)
        -- Unfold the walk one step: both tests pass.
        simp only [walk, hn, ha, ite_true]
        -- Three parts to show, the first in three pieces of its own.
        refine ⟨⟨rfl, ?_, ?_⟩, ?_, ?_⟩
        -- The call covers at least one byte: take is the smaller of two numbers above 0.
        · show 0 < min (n - done) (l - at_)
          -- Arithmetic.
          omega
        -- The rest cover from the call's end to the right place:
        ·
          -- that place, counted as the rest count it, is the place this chain must reach.
          have e : done + min (n - done) (l - at_) +
              min (n - (done + min (n - done) (l - at_))) (room ls - 0) =
              done + min (n - done) (room (l :: ls) - at_) := by
            -- Unfold the room one segment, then arithmetic in both cases of each min.
            simp only [room]; omega
          -- Rewrite the goal's end to the rest's end, which the claim for ls covers.
          rw [← e]; exact hc
        -- The `return`: the rest's answer, and the same test said of this chain.
        · rw [hf]; simp only [room]; omega
        -- Every piece is inside a segment:
        · intro p hp'
          -- it is the call just made or one of the rest's.
          simp only [List.mem_cons] at hp'
          -- Look at each case.
          rcases hp' with rfl | hp'
          -- The call just made: segment 0 of this chain, from at for take bytes, no further than l.
          · exact ⟨0, by simp, by simp, by simp [lenAt]; omega, by simp [start]⟩
          -- One of the rest's pieces:
          ·
            -- either the call reached the end of this segment, or it finished the range.
            by_cases ht : min (n - done) (l - at_) = l - at_
            -- It reached the end: the rest's piece is in segment j of ls, so j + 1 of this chain.
            · obtain ⟨j, hj, hjl, hjp, hjs⟩ := hp p hp'
              -- Segment j + 1 exists, holds the piece, and starts l bytes later than j of ls.
              exact ⟨j + 1, by omega, by simp; omega, by simpa [lenAt] using hjp, by simp [start]; omega⟩
            -- It finished the range: every byte is done, so the rest made no call at all.
            · have hz : n ≤ done + min (n - done) (l - at_) := by omega
              -- The rest's walk is the empty answer,
              rw [walk_done n ls (k + 1) 0 _ hz] at hp'
              -- and nothing is in an empty list.
              simp at hp'
      -- Past this segment: no call, `at -= l`, and the walk goes on in the rest.
      ·
        -- The claim for the rest at `at - l`.
        have ⟨hc, hf, hp⟩ := ih (k + 1) (at_ - l) done hd
        -- Unfold the walk one step: the first test passes, the second fails.
        simp only [walk, hn, ha, ite_true, ite_false]
        -- Three parts to show.
        refine ⟨?_, ?_, ?_⟩
        -- The cover: the rest's room from at - l is this chain's room from at.
        · have e : room ls - (at_ - l) = room (l :: ls) - at_ := by simp only [room]; omega
          -- Rewrite to the rest's form, which the claim for ls gives.
          rw [← e]; exact hc
        -- The `return`: the rest's answer, and the same test said of this chain.
        · rw [hf]; simp only [room]; omega
        -- Every piece is one of the rest's, in segment j of ls, so j + 1 of this chain.
        · intro p hp'
          -- The rest's piece and where it lies.
          obtain ⟨j, hj, hjl, hjp, hjs⟩ := hp p hp'
          -- Segment j + 1 exists, holds the piece, and starts l bytes later than j of ls.
          exact ⟨j + 1, by omega, by simp; omega, by simpa [lenAt] using hjp, by simp [start]; omega⟩
    -- Every byte is done:
    ·
      -- so done is n,
      have hz : n ≤ done := by omega
      -- and the walk makes no call and takes no `return`.
      rw [walk_done n (l :: ls) k at_ done hz]
      -- Unfold: the empty stretch from n to n, no `return`, no piece; arithmetic for the rest.
      simp [Covers]
      -- Arithmetic: done = n makes the min 0.
      omega

/-- A range that fits the room (what every caller of `pieces` promises: a
read below a length it loaded, a write into room it reserved) is cut into
pieces that cover it from its first byte to its last with no gap and no
overlap, each inside its segment and at the byte the chain holds; the early
`return` is never taken. -/
theorem pieces_exact (n : Nat) (segs : List Nat) (at_ : Nat) (h : at_ + n ≤ room segs) :
    -- The statement: no `return`, the pieces cover [0, n) of the range,
    (pieces n segs at_).2 = false ∧ Covers (pieces n segs at_).1 0 n ∧
    -- and each lies inside a segment of the chain, at the byte at_ + off.
    ∀ p ∈ (pieces n segs at_).1, p.seg < segs.length ∧ p.pos + p.take ≤ lenAt segs p.seg ∧
      start segs p.seg + p.pos = at_ + p.off := by
  -- `pieces` is the walk from segment 0 with nothing done.
  unfold pieces
  -- What the walk does, from there.
  have ⟨hc, hf, hp⟩ := walk_spec n segs 0 at_ 0 (Nat.zero_le n)
  -- Three parts to show.
  refine ⟨?_, ?_, ?_⟩
  -- No `return`: look at the answer.
  · cases hb : (walk n segs 0 at_ 0).2 with
    -- It says no `return`: that is the goal.
    | false => rfl
    -- It says `return`: then the chain holds fewer than n bytes from at_ on, against h.
    | true => have := hf.mp hb; omega
  -- The cover ends at n, because the chain holds at least n bytes from at_ on.
  · have e : 0 + min (n - 0) (room segs - at_) = n := by omega
    -- Rewrite the cover's end to n,
    rw [e] at hc
    -- and that is the goal.
    exact hc
  -- Each piece: the walk's segment j is segment j overall, since it started at segment 0.
  · intro p hm
    -- Where the piece lies.
    obtain ⟨j, hj, hjl, hjp, hjs⟩ := hp p hm
    -- Its segment number is 0 + j, which is j.
    rw [hj, Nat.zero_add]
    -- The three facts, the last with done = 0 dropped by arithmetic.
    exact ⟨hjl, hjp, by omega⟩

/-- A range that runs past what the chain holds (no caller makes one: the
`return` in the Rust is unreachable under its contract) makes the walk take
the early `return`, after pieces that cover exactly the bytes the chain
holds from `at_` on, each still inside its segment: it touches nothing past
the chain. A read of 0 bytes never counts as running past. -/
theorem pieces_short (n : Nat) (segs : List Nat) (at_ : Nat) (h : room segs - at_ < n) :
    -- The statement: the `return` is taken, the pieces cover [0, room - at_) of the range,
    (pieces n segs at_).2 = true ∧ Covers (pieces n segs at_).1 0 (room segs - at_) ∧
    -- and each lies inside a segment of the chain, at the byte at_ + off.
    ∀ p ∈ (pieces n segs at_).1, p.seg < segs.length ∧ p.pos + p.take ≤ lenAt segs p.seg ∧
      start segs p.seg + p.pos = at_ + p.off := by
  -- What the walk does, from segment 0 with nothing done.
  have ⟨hc, hf, hp⟩ := walk_spec n segs 0 at_ 0 (Nat.zero_le n)
  -- Three parts to show.
  refine ⟨?_, ?_, ?_⟩
  -- The `return`: the chain holds fewer than n bytes from at_ on.
  · exact hf.mpr (by omega)
  -- The cover ends at room - at_, the smaller of the two.
  · have e : 0 + min (n - 0) (room segs - at_) = room segs - at_ := by omega
    -- Rewrite the cover's end,
    rw [e] at hc
    -- and that is the goal.
    exact hc
  -- Each piece, as in `pieces_exact`.
  · intro p hm
    -- Where the piece lies.
    obtain ⟨j, hj, hjl, hjp, hjs⟩ := hp p hm
    -- Its segment number is 0 + j, which is j.
    rw [hj, Nat.zero_add]
    -- The three facts, the last with done = 0 dropped by arithmetic.
    exact ⟨hjl, hjp, by omega⟩

/-- Segments linked after the ones a range needs change nothing about its
walk: the walk is done before it reaches them. This is why a reader copying
below a length it loaded never notices a writer that links more room. -/
theorem walk_append (n : Nat) (more : List Nat) :
    -- The statement: for every chain and walk state whose remaining bytes fit the chain,
    ∀ (segs : List Nat) (k at_ done : Nat), done ≤ n → at_ + (n - done) ≤ room segs →
      -- the walk over the chain with more segments after it is the same walk.
      walk n (segs ++ more) k at_ done = walk n segs k at_ done := by
  -- Take the chain.
  intro segs
  -- Induction on the chain.
  induction segs with
  -- Case: no segment left, so the room is 0 and nothing is owed.
  | nil =>
    -- Take the inputs.
    intro k at_ done hd hr
    -- The room of no segment is 0.
    simp only [room] at hr
    -- Both walks have every byte done: each is the empty answer.
    rw [List.nil_append, walk_done n more k at_ done (by omega), walk_done n [] k at_ done (by omega)]
  -- Case: a current segment l in front of ls, with the claim for ls (ih) known.
  | cons l ls ih =>
    -- Take the inputs.
    intro k at_ done hd hr
    -- Unfold the room one segment.
    simp only [room] at hr
    -- Either bytes are owed or every byte is done.
    by_cases hn : done < n
    -- Bytes are owed:
    ·
      -- the range starts inside this segment or past it.
      by_cases ha : at_ < l
      -- Inside: both walks make the same call, then go on in the rest, the same by the claim.
      · simp only [List.cons_append, walk, hn, ha, ite_true]
        -- The rest's walks are equal, since what is owed still fits the rest.
        rw [ih (k + 1) 0 (done + min (n - done) (l - at_)) (by omega) (by omega)]
      -- Past it: both walks go on in the rest with `at - l`, the same by the claim.
      · simp only [List.cons_append, walk, hn, ha, ite_true, ite_false]
        -- The claim for the rest at at_ - l.
        exact ih (k + 1) (at_ - l) done hd (by omega)
    -- Every byte is done: both walks are the empty answer, by unfolding.
    · simp only [List.cons_append, walk, hn, ite_false]

/-- `MIN_CAPACITY` (extent.rs:35): the smallest segment a file grows by. -/
def minCapacity : Nat := 64

/-- `Extent::reserve(end)` (extent.rs:161-180): when the room is short of
`end`, link one segment of `max(end - room, room, MIN_CAPACITY)` bytes after
the last one; otherwise change nothing. A failed allocation returns an
error and links nothing, which leaves the chain as it was. -/
def reserve (segs : List Nat) (end_ : Nat) : List Nat :=
  -- `if end <= room { return }`, else the chain with `grow` linked at its end.
  if end_ ≤ room segs then segs else segs ++ [max (max (end_ - room segs) (room segs)) minCapacity]

/-- The room of two chains end to end is the room of each, added. -/
theorem room_append :
    -- The statement: for every two chains a and b.
    ∀ (a b : List Nat), room (a ++ b) = room a + room b := by
  -- Take both chains.
  intro a b
  -- Induction on the first one.
  induction a with
  -- Case: a is empty; unfolding shows both sides are the room of b.
  | nil => simp [room]
  -- Case: one segment more in front; unfold it, use the claim, and rearrange.
  | cons l ls ih => simp only [List.cons_append, room, ih]; omega

/-- A reserve either leaves the chain as it is, the room already reaching
`end`, or links exactly one segment `g` at its end, at least as large as the
room before it and at least `MIN_CAPACITY`, after which the room reaches
`end`. -/
theorem reserve_links (segs : List Nat) (end_ : Nat) :
    -- The statement: unchanged with room enough,
    reserve segs end_ = segs ∧ end_ ≤ room segs ∨
    -- or one segment linked, large enough on all three counts.
    ∃ g, reserve segs end_ = segs ++ [g] ∧ room segs ≤ g ∧ minCapacity ≤ g ∧ end_ ≤ room segs + g := by
  -- Unfold the reserve.
  unfold reserve
  -- Either the room already reaches end, or not.
  by_cases h : end_ ≤ room segs
  -- It does: the chain is unchanged.
  · left; simp [h]
  -- It does not: one segment is linked.
  · right
    -- That segment is `grow`; each of its three bounds is arithmetic on the max.
    refine ⟨max (max (end_ - room segs) (room segs)) minCapacity, by simp [h], ?_, ?_, ?_⟩ <;> omega

/-- After a reserve the room reaches `end`: the write that asked for it fits. -/
theorem reserve_reaches (segs : List Nat) (end_ : Nat) :
    -- The statement: end is at most the room after the reserve.
    end_ ≤ room (reserve segs end_) := by
  -- Look at what the reserve did.
  rcases reserve_links segs end_ with ⟨e, h⟩ | ⟨g, e, _, _, h⟩
  -- Nothing: the room already reached end.
  · rw [e]; exact h
  -- Linked g: the room is the old room plus g, which reaches end.
  · rw [e, room_append]; simp only [room]; omega

/-- A reserve moves no byte: the pieces of any range below the old room are
the same before and after it. -/
theorem reserve_moves_nothing (n : Nat) (segs : List Nat) (end_ at_ : Nat)
    -- The range fits the room before the reserve.
    (h : at_ + n ≤ room segs) :
    -- The statement: the same pieces, the same `return` flag.
    pieces n (reserve segs end_) at_ = pieces n segs at_ := by
  -- Look at what the reserve did.
  rcases reserve_links segs end_ with ⟨e, _⟩ | ⟨g, e, _⟩
  -- Nothing: the same chain.
  · rw [e]
  -- Linked g: the chain with one segment after it.
  · rw [e]
    -- A segment after the ones the range needs changes nothing.
    exact walk_append n [g] segs 0 at_ 0 (Nat.zero_le n) (by omega)

/-- The bound a grown chain keeps: one segment, or `j >= 1` linked ones
(length `j + 1`) and room at least `MIN_CAPACITY * 2^(j - 1)`. -/
def Doubling (segs : List Nat) : Prop :=
  -- Only the first segment, or room enough for the segments linked.
  segs.length = 1 ∨ (2 ≤ segs.length ∧ minCapacity * 2 ^ (segs.length - 2) ≤ room segs)

/-- A reserve keeps the bound: the segment it links is at least the room
before it and at least `MIN_CAPACITY`, so the room at least doubles. -/
theorem reserve_doubling (segs : List Nat) (end_ : Nat) (h : Doubling segs) :
    -- The statement: the chain after the reserve keeps the bound too.
    Doubling (reserve segs end_) := by
  -- Look at what the reserve did.
  rcases reserve_links segs end_ with ⟨e, _⟩ | ⟨g, e, hg, hm, _⟩
  -- Nothing: the same chain, which kept the bound.
  · rw [e]; exact h
  -- Linked g:
  · rw [e]
    -- the chain now has at least two segments, so show the room bound.
    right
    -- Its length is one more, its room the old room plus g.
    rw [List.length_append, room_append]
    -- Unfold the length and room of the one-segment list [g].
    simp only [List.length_cons, List.length_nil, room]
    -- Before the link: one segment only, or the bound for some linked segments.
    rcases h with h1 | ⟨h2, hd⟩
    -- One segment only: the first link, and 64 * 2^0 = 64 is at most g.
    · rw [h1]
      -- Name 64 as the number it is.
      simp only [minCapacity] at *
      -- Arithmetic.
      omega
    -- Some segments were linked: write the length as m + 2.
    · obtain ⟨m, hl⟩ : ∃ m, segs.length = m + 2 := ⟨segs.length - 2, by omega⟩
      -- Put m + 2 in place of the length, before and after.
      rw [hl] at hd ⊢
      -- Before: 64 * 2^m is at most the room.
      simp only [Nat.add_sub_cancel] at hd
      -- After: the exponent is m + 1,
      have e2 : m + 2 + (0 + 1) - 2 = m + 1 := by omega
      -- and 2^(m + 1) is 2^m * 2.
      rw [e2, Nat.pow_succ]
      -- Name 64 as the number it is.
      simp only [minCapacity] at *
      -- Arithmetic: room + g is at least twice the room, which is at least 2 * 64 * 2^m.
      omega

/-- A chain grown from one segment of room `first` by the reserves for the
ends in `ends`, in order: what in-place appends do to a file. -/
def grow (first : Nat) (ends : List Nat) : List Nat :=
  -- One reserve per end, starting from the one segment.
  ends.foldl reserve [first]

/-- Reserves applied one after the other keep the bound. -/
theorem foldl_doubling :
    -- The statement: for every list of ends and every chain that keeps the bound,
    ∀ (ends : List Nat) (segs : List Nat), Doubling segs →
      -- the chain after all those reserves keeps it too.
      Doubling (ends.foldl reserve segs) := by
  -- Take the ends.
  intro ends
  -- Induction on the ends.
  induction ends with
  -- No end: no reserve, the same chain.
  | nil => intro segs h; exact h
  -- One end first: reserve for it (which keeps the bound), then the rest by the claim.
  | cons e es ih => intro segs h; exact ih (reserve segs e) (reserve_doubling segs e h)

/-- Every grown chain keeps the bound: it starts as one segment. -/
theorem grow_doubling (first : Nat) (ends : List Nat) :
    -- The statement: the grown chain keeps the bound.
    Doubling (grow first ends) :=
  -- One segment keeps it, and every reserve keeps it.
  foldl_doubling ends [first] (Or.inl rfl)

/-- A grown chain is short: room below `64 * 2^m` means at most `m + 1`
segments. A file of a megabyte (`2^20`, room below `64 * 2^16`) is at most
17 segments, what `a_growing_file_links_room_and_copies_nothing` checks
loosely. -/
theorem grow_few (first : Nat) (ends : List Nat) (m : Nat)
    -- The room is below 64 * 2^m.
    (h : room (grow first ends) < minCapacity * 2 ^ m) :
    -- The statement: at most m + 1 segments.
    (grow first ends).length ≤ m + 1 := by
  -- The chain keeps the bound: one segment, or room for the linked ones.
  rcases grow_doubling first ends with h1 | ⟨h2, hd⟩
  -- One segment: at most m + 1.
  · omega
  -- Linked segments: suppose there were more than m + 1.
  · apply Nat.le_of_not_lt
    -- Take that supposition.
    intro hlt
    -- Then the exponent of the bound is at least m, so 2^m is at most 2^(length - 2).
    have hp : 2 ^ m ≤ 2 ^ ((grow first ends).length - 2) :=
      -- A larger exponent of 2 gives a larger power.
      Nat.pow_le_pow_right (by decide) (by omega)
    -- Name 64 as the number it is.
    simp only [minCapacity] at *
    -- Arithmetic: 64 * 2^m would be at most the room, which is below it.
    omega

-- A file that starts empty and takes appends to 1000, 2000, 3000 and 70000 bytes: the chain
-- links 1000 (the append's own need), 1000 (the room before it), 2000 and 66000 bytes.
example : grow 0 [1000, 2000, 3000, 70000] = [0, 1000, 1000, 2000, 66000] := by
  -- Run the reserves and compare.
  rfl

-- The end of the names this file defines.
end Regolith.MemFileExtents
