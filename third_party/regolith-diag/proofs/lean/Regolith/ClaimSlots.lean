/-!
# ClaimSlots: every slot has one place, and a stale free-stack word never matches

THE STORY. A transaction that has something to run when it aborts is listed
with the engine, so that `close` can find it. The list is a table of slots
(`ClaimSlots`, `src/engine/open_transactions/claim_slots.rs`). The table is
made of chunks: chunk 0 holds `FIRST` = 64 slots, chunk 1 holds 128, chunk
`k` holds `FIRST << k`, and there are `CHUNKS` = 26 of them. Slots are
numbered from 0 across the chunks, and `place(slot)` says which chunk a slot
is in and where in it. Tiny example: slot 63 is the last of chunk 0, slot 64
is the first of chunk 1 (offset 0), slot 191 is the last of chunk 1 (offset
127), and slot 192 is the first of chunk 2.

The free slots sit on a stack. One 64-bit word, `free`, holds the top slot
plus one in its low 32 bits (0 when the stack is empty) and, in its high 32
bits, a count of every change ever made to the stack: the "tag". Taking or
giving back a slot is a compare-and-swap (CAS) of that word to
`next_word(head, top)`, which puts the new top in the low half and adds one
to the tag. A thread that read the word, paused, and then tries its CAS must
fail if the stack changed meanwhile, even when the same top came back (the
"ABA" problem: pop A, pop B, push A leaves A on top again, but with B gone).

What must never happen:
- two slots in one place, a slot outside every chunk, or a chunk cell no
  slot reaches (then two transactions would share a cell, or close would miss
  one);
- a slot number whose "plus one" does not fit the low half of the word;
- a CAS that read the word before a few changes succeeding after them.

WHAT IS PROVED, for every chunk size and every number of chunks where it says
so:

1. `place_bounds`, `place_chunk_lt`, `place_cell`, `slot_of_place`,
   `place_injective`, `cell_lt_capacity` and `place_bijective`: for any
   `FIRST > 0` and any number of chunks, `place` sends every slot below the
   capacity `FIRST * (2^CHUNKS - 1)` to a chunk below `CHUNKS` and an offset
   below that chunk's length `FIRST * 2^chunk`; two slots never share a
   place; and every cell of every chunk is the place of exactly one slot
   below the capacity, the slot `FIRST * (2^chunk - 1) + offset`.
2. `chunkTotal_eq`: the chunks' lengths add up to the capacity.
3. `real_capacity`, `real_slot_fits` and `real_chunks_most`: with the real
   constants (64 and 26) the capacity is `2^32 - 64`, every slot below it
   plus one is between 1 and `u32::MAX`, so it fits the low half of the word
   and a `u32`, and a 27th chunk would not fit.
4. `topOf_eq`, `highOf_eq`, `pack_unpack`, `unpack_pack` and `nextWord_eq`:
   the word's bit operations, as the Rust writes them, are the arithmetic of
   a pair (tag, top): `head & TOP` is the top, `head & !TOP` is the tag
   shifted up, and `next_word` keeps the new top and adds one to the tag,
   modulo `2^32`.
5. `nextWord_ne`: a word after one change is never the word before it.
6. `changes_tag` and `no_aba`: after any `k` changes, `0 < k < 2^32`,
   whatever tops they leave, the word differs from the word read before
   them, so a CAS expecting the old word fails. This is the bound
   `proofs/tla/ClaimSlots.tla` names in its "left out" note: its runs make a
   few changes, and here any number below `2^32` is covered.
7. `untagged_aba`: the RED case. Without the tag (the `NoTag` calibration of
   the loom models), pop A, pop B, push A brings back the very word read at
   the start, so the stale CAS would succeed.

HOW THE BIT OPERATIONS ARE MODELLED. A `u64` is a natural number below
`2^64`. Lean's `&&&` and `|||` on natural numbers are bitwise and and or,
which agree with Rust's `&` and `|` on numbers below `2^64`. Rust's `!TOP` on
a `u64` is every bit `TOP` lacks: `2^64 - 1 - TOP`. `wrapping_add` is an add
taken modulo `2^64`. The proofs then show these equal plain arithmetic on
the pair (tag, top).

This module backs `proofs/tla/ClaimSlots.tla`, whose chunks hold `2^k`
slots (`FIRST = 1`) and whose tag never wraps; the loom models in
`src/engine/open_transactions/loom_model.rs` check the memory orderings.
-/

-- Everything below lives under this name, so it cannot clash with other files.
namespace Regolith.ClaimSlots

/-! ## Part 1: `place`, the chunk and offset of a slot -/

/-- The chunk a slot is in and its offset there, as `place` in claim_slots.rs
computes them: `run = slot / first + 1`, the chunk is the floor of log2 of
`run` (Rust's `run.ilog2()`), and the offset is the slot less every slot of
the chunks before its own, `first * ((1 << chunk) - 1)`. -/
def place (first slot : Nat) : Nat × Nat :=
  -- The chunk: floor of log2 of the run.
  (Nat.log2 (slot / first + 1),
    -- The offset: the slot less the slots of every chunk before its own.
    slot - first * (2 ^ Nat.log2 (slot / first + 1) - 1))

/-- The first slot of chunk `c`: the slots of every chunk before it,
`first * (2^c - 1)`. -/
def chunkStart (first c : Nat) : Nat :=
  -- Chunks 0 .. c-1 hold first * (1 + 2 + ... + 2^(c-1)) = first * (2^c - 1).
  first * (2 ^ c - 1)

/-- How many slots chunk `c` holds: `FIRST << c`, which is `first * 2^c`. -/
def chunkLen (first c : Nat) : Nat :=
  -- A shift left by c multiplies by 2^c.
  first * 2 ^ c

/-- Every slot the chunks hold: `CAPACITY = FIRST * ((1 << CHUNKS) - 1)`. -/
def capacity (first chunks : Nat) : Nat :=
  -- 1 << chunks is 2^chunks.
  first * (2 ^ chunks - 1)

/-- **`place_bounds`.** A slot lies at or after the first slot of the chunk
`place` names, and less than that chunk's length past it: the offset is
inside the chunk. Example ruled out: slot 64 placed in chunk 0 at offset 64,
one past chunk 0's last cell. -/
theorem place_bounds (first slot : Nat) (hf : 0 < first) :
    -- The statement: the chunk's first slot is at most the slot,
    chunkStart first (place first slot).1 ≤ slot ∧
      -- and the offset is below the chunk's length.
      slot - chunkStart first (place first slot).1 < chunkLen first (place first slot).1 := by
  -- The run, `slot / first + 1`, is not zero, so log2 of it is its floor.
  have hrun : slot / first + 1 ≠ 0 := Nat.succ_ne_zero _
  -- The run is at least 2^chunk: the floor of log2 is not above it.
  have hlo := Nat.log2_self_le hrun
  -- The run is below 2^(chunk + 1).
  have hhi : slot / first + 1 < 2 ^ (Nat.log2 (slot / first + 1) + 1) := Nat.lt_log2_self
  -- The slot is at least `first` times its quotient,
  have hdown := Nat.mul_div_le slot first
  -- and below `first` times its quotient plus one.
  have hup := Nat.lt_mul_div_succ slot hf
  -- Unfold the names into their arithmetic.
  simp only [place, chunkStart, chunkLen]
  -- Call the chunk `c`, so the rest is about plain numbers.
  generalize Nat.log2 (slot / first + 1) = c at hlo hhi ⊢
  -- Call the quotient `q`.
  generalize slot / first = q at hlo hhi hdown hup ⊢
  -- 2^(c + 1) is 2^c times 2.
  rw [Nat.pow_succ] at hhi
  -- 2^c is at least 1.
  have hp := Nat.two_pow_pos c
  -- Call 2^c `p`.
  generalize 2 ^ c = p at hlo hhi hp ⊢
  -- `first * (p - 1)` is at most `first * q`, because `p - 1 ≤ q`.
  have h1 : first * (p - 1) ≤ first * q := Nat.mul_le_mul_left first (by omega)
  -- `first * (q + 1)` is at most `first * (p - 1) + first * p`, because `q + 1 < 2p`.
  have h2 : first * (q + 1) ≤ first * (p - 1) + first * p := by
    -- Gather the right side into `first * ((p - 1) + p)`.
    rw [← Nat.mul_add]
    -- Multiplying by `first` keeps `q + 1 ≤ (p - 1) + p`.
    exact Nat.mul_le_mul_left first (by omega)
  -- Name `first * q`, the multiple of `first` at or below the slot, `a`.
  generalize first * q = a at hdown h1
  -- Name `first * (q + 1)`, the multiple of `first` above the slot, `b`.
  generalize first * (q + 1) = b at hup h2
  -- Name `first * (p - 1)`, the chunk's first slot, `s`.
  generalize first * (p - 1) = s at h1 h2 ⊢
  -- Name `first * p`, the chunk's length, `l`: what is left is adding and comparing.
  generalize first * p = l at h2 ⊢
  -- Both halves now follow by adding and comparing.
  omega

/-- **`place_chunk_lt`.** A slot below the capacity is placed in one of the
chunks there are. Example ruled out: the last slot placed in chunk
`CHUNKS`, one past the array of chunks. -/
theorem place_chunk_lt (first chunks slot : Nat) (hf : 0 < first)
    -- The slot is below the capacity.
    (hs : slot < capacity first chunks) :
    -- The statement: its chunk is below the number of chunks.
    (place first slot).1 < chunks := by
  -- The run is not zero.
  have hrun : slot / first + 1 ≠ 0 := Nat.succ_ne_zero _
  -- The quotient is below `2^chunks - 1`, because the slot is below `first` times that.
  have hq : slot / first < 2 ^ chunks - 1 :=
    -- Dividing by `first` keeps a strict bound by a multiple of `first`.
    (Nat.div_lt_iff_lt_mul hf).mpr (by rw [Nat.mul_comm]; exact hs)
  -- 2^chunks is at least 1.
  have hp := Nat.two_pow_pos chunks
  -- Unfold `place` to its first part.
  simp only [place]
  -- log2 of the run is below `chunks` because the run is below 2^chunks.
  refine (Nat.log2_lt hrun).mpr ?_
  -- Call the quotient `q`, a plain number.
  generalize slot / first = q at hq ⊢
  -- `q < 2^chunks - 1` gives `q + 1 < 2^chunks`.
  omega

/-- **`place_cell`.** The slot `chunkStart first c + o`, for any offset `o`
inside chunk `c`, is placed exactly at chunk `c`, offset `o`. Example ruled
out: slot 192 (chunk 2, offset 0) placed at chunk 1, offset 128. -/
theorem place_cell (first c o : Nat) (hf : 0 < first)
    -- The offset is inside chunk `c`.
    (ho : o < chunkLen first c) :
    -- The statement: `place` gives back that chunk and that offset.
    place first (chunkStart first c + o) = (c, o) := by
  -- Unfold the chunk's start and length.
  simp only [chunkStart, chunkLen] at ho ⊢
  -- The quotient of the slot by `first` is `2^c - 1` plus `o / first`.
  have hdiv : (first * (2 ^ c - 1) + o) / first = 2 ^ c - 1 + o / first :=
    -- Dividing a multiple of `first` plus a rest.
    Nat.mul_add_div hf _ _
  -- `o / first` is below 2^c, because `o` is below `first * 2^c`.
  have hob : o / first < 2 ^ c :=
    -- Dividing by `first` keeps a strict bound by a multiple of `first`.
    (Nat.div_lt_iff_lt_mul hf).mpr (by rw [Nat.mul_comm]; exact ho)
  -- 2^c is at least 1.
  have hp := Nat.two_pow_pos c
  -- So the run is at least 2^c,
  have hlo : 2 ^ c ≤ (first * (2 ^ c - 1) + o) / first + 1 := by
    -- by the quotient just computed.
    rw [hdiv]
    -- Call `o / first` a plain number `r`.
    generalize o / first = r at hob ⊢
    -- Arithmetic.
    omega
  -- and below 2^(c + 1).
  have hhi : (first * (2 ^ c - 1) + o) / first + 1 < 2 ^ (c + 1) := by
    -- By the quotient just computed, and 2^(c + 1) = 2^c * 2.
    rw [hdiv, Nat.pow_succ]
    -- Call `o / first` a plain number `r`.
    generalize o / first = r at hob ⊢
    -- Arithmetic.
    omega
  -- The run is not zero.
  have hrun : (first * (2 ^ c - 1) + o) / first + 1 ≠ 0 := Nat.succ_ne_zero _
  -- Hence its log2 is exactly `c`: at most `c` by the upper bound,
  have hlog : Nat.log2 ((first * (2 ^ c - 1) + o) / first + 1) = c :=
    -- at least `c` by the lower bound.
    Nat.le_antisymm (Nat.lt_succ_iff.mp ((Nat.log2_lt hrun).mpr hhi)) ((Nat.le_log2 hrun).mpr hlo)
  -- Unfold `place` and put in the chunk just found.
  simp only [place, hlog]
  -- The offset is the slot less the chunk's start, which is `o`.
  rw [Nat.add_sub_cancel_left]

/-- **`slot_of_place`.** The chunk's first slot plus the offset `place`
gives is the slot again: `place` loses nothing. -/
theorem slot_of_place (first slot : Nat) (hf : 0 < first) :
    -- The statement: start of the chunk plus the offset is the slot.
    chunkStart first (place first slot).1 + (place first slot).2 = slot :=
  -- The start is at most the slot, so adding back what was taken off restores it.
  Nat.add_sub_of_le (place_bounds first slot hf).1

/-- **`place_injective`.** Two slots with the same place are the same slot:
no cell holds two slots. Example ruled out: slots 63 and 64 both at chunk 0,
offset 63. -/
theorem place_injective (first a b : Nat) (hf : 0 < first)
    -- The two slots have the same place.
    (h : place first a = place first b) :
    -- The statement: they are the same slot.
    a = b := by
  -- Each slot is its chunk's start plus its offset.
  rw [← slot_of_place first a hf, ← slot_of_place first b hf, h]

/-- **`cell_lt_capacity`.** The slot of a cell inside one of the chunks is
below the capacity. Example ruled out: the last cell of the last chunk at a
slot past `CAPACITY`, which `take` never hands out. -/
theorem cell_lt_capacity (first chunks c o : Nat)
    -- The chunk is one there is, and the offset is inside it.
    (hc : c < chunks) (ho : o < chunkLen first c) :
    -- The statement: the cell's slot is below the capacity.
    chunkStart first c + o < capacity first chunks := by
  -- Unfold the names into their arithmetic.
  simp only [chunkStart, chunkLen, capacity] at ho ⊢
  -- 2^(c + 1) is at most 2^chunks, because c + 1 ≤ chunks.
  have hle : 2 ^ (c + 1) ≤ 2 ^ chunks := Nat.pow_le_pow_right (by decide) hc
  -- 2^(c + 1) is 2^c times 2.
  rw [Nat.pow_succ] at hle
  -- 2^c is at least 1.
  have hp := Nat.two_pow_pos c
  -- Call 2^c `p`.
  generalize 2 ^ c = p at hle hp ho ⊢
  -- Call 2^chunks `m`.
  generalize 2 ^ chunks = m at hle ⊢
  -- `first * ((p - 1) + p)` is at most `first * (m - 1)`, because `2p ≤ m`.
  have h : first * (p - 1 + p) ≤ first * (m - 1) := Nat.mul_le_mul_left first (by omega)
  -- Multiply out the left side.
  rw [Nat.mul_add] at h
  -- The cell's slot is below `first * (p - 1) + first * p`, which is at most the capacity.
  omega

/-- **`place_bijective`.** `place` is a one-to-one match between the slots
below the capacity and the cells of the chunks: each slot lands in a cell
that exists, no two slots share one, and every cell is some slot's place.
This is what lets `drain` look at slots `0 .. taken` and reach every cell a
claim can be in, and what keeps two transactions out of one cell. -/
theorem place_bijective (first chunks : Nat) (hf : 0 < first) :
    -- The statement, first part: every slot below the capacity lands in a chunk there is, inside
    -- its length;
    (∀ slot, slot < capacity first chunks →
      (place first slot).1 < chunks ∧ (place first slot).2 < chunkLen first (place first slot).1) ∧
    -- second part: no two slots share a place;
    (∀ a b, place first a = place first b → a = b) ∧
    -- third part: every cell of every chunk is the place of a slot below the capacity.
    (∀ c o, c < chunks → o < chunkLen first c →
      ∃ slot, slot < capacity first chunks ∧ place first slot = (c, o)) := by
  -- Three parts, one at a time.
  refine ⟨?_, ?_, ?_⟩
  -- Part one: the chunk is below `chunks` and the offset below the chunk's length.
  ·
    -- Take a slot below the capacity.
    intro slot hs
    -- The chunk bound, and the offset bound (the offset is the slot less the chunk's start).
    exact ⟨place_chunk_lt first chunks slot hf hs, (place_bounds first slot hf).2⟩
  -- Part two: equal places mean equal slots.
  ·
    -- Take two slots with one place.
    intro a b h
    -- They are equal.
    exact place_injective first a b hf h
  -- Part three: the slot `chunkStart first c + o` is below the capacity and placed at (c, o).
  ·
    -- Take a cell.
    intro c o hc ho
    -- Its slot works.
    exact ⟨chunkStart first c + o, cell_lt_capacity first chunks c o hc ho, place_cell first c o hf ho⟩

/-- The slots of the first `k` chunks, added chunk by chunk. -/
def chunkTotal (first : Nat) : Nat → Nat
  -- No chunks: no slots.
  | 0 => 0
  -- The first `k` chunks, then chunk `k`.
  | k + 1 => chunkTotal first k + chunkLen first k

/-- **`chunkTotal_eq`.** The chunks' lengths add up to the capacity: the
table holds `first * (2^k - 1)` slots in `k` chunks, no more, no fewer. -/
theorem chunkTotal_eq (first : Nat) :
    -- The statement: for every number of chunks, the lengths add up to the capacity.
    ∀ k, chunkTotal first k = capacity first k := by
  -- One chunk at a time.
  intro k
  -- Induction on k: first no chunk, then one more.
  induction k with
  -- Case: no chunk.
  | zero =>
    -- Both sides are 0: 2^0 - 1 is 0.
    simp [chunkTotal, capacity]
  -- Case: k + 1 chunks, with the claim for k (ih) known.
  | succ k ih =>
    -- Unfold one step of the total and the names.
    simp only [chunkTotal, ih, capacity, chunkLen]
    -- Gather the two products into one: `first * (2^k - 1) + first * 2^k = first * ((2^k - 1) + 2^k)`.
    rw [← Nat.mul_add]
    -- 2^(k + 1) is 2^k times 2.
    rw [Nat.pow_succ]
    -- 2^k is at least 1.
    have hp := Nat.two_pow_pos k
    -- Both sides multiply `first` by the same number: (2^k - 1) + 2^k = 2^k * 2 - 1.
    congr 1
    -- Arithmetic.
    omega

/-- `FIRST` in claim_slots.rs. -/
def FIRST : Nat := 64

/-- `CHUNKS` in claim_slots.rs. -/
def CHUNKS : Nat := 26

/-- `CAPACITY` in claim_slots.rs. -/
def CAPACITY : Nat := capacity FIRST CHUNKS

/-- **`real_capacity`.** With the real constants the table holds
`2^32 - 64` slots. -/
theorem real_capacity : CAPACITY = 2 ^ 32 - 64 := by
  -- A computation on numbers.
  decide

/-- **`real_slot_fits`.** Every slot the table hands out, plus one, is
between 1 and `u32::MAX`: it fits the low half of the free word and the
`u32` a free slot's `below` holds, and it is never 0, which means "empty".
The slot itself fits the `u32` `reserve` returns. -/
theorem real_slot_fits (slot : Nat) (hs : slot < CAPACITY) :
    -- The statement: slot + 1 is at least 1 and at most u32::MAX, and the slot is at most u32::MAX.
    1 ≤ slot + 1 ∧ slot + 1 ≤ 2 ^ 32 - 1 ∧ slot ≤ 2 ^ 32 - 1 := by
  -- The capacity is 2^32 - 64.
  rw [real_capacity] at hs
  -- Arithmetic.
  omega

/-- **`real_chunks_most`.** A 27th chunk would hold slots whose number plus
one no longer fits a `u32`: 26 is the most chunks the word allows, as the
comment on `CHUNKS` says. -/
theorem real_chunks_most : capacity FIRST (CHUNKS + 1) > 2 ^ 32 - 1 := by
  -- A computation on numbers.
  decide

/-- **`real_place_bijective`.** `place_bijective` for the real table. -/
theorem real_place_bijective :
    -- The statement: as `place_bijective`, with FIRST = 64 and CHUNKS = 26.
    (∀ slot, slot < CAPACITY →
      (place FIRST slot).1 < CHUNKS ∧ (place FIRST slot).2 < chunkLen FIRST (place FIRST slot).1) ∧
    -- No two slots share a place.
    (∀ a b, place FIRST a = place FIRST b → a = b) ∧
    -- Every cell is some slot's place.
    (∀ c o, c < CHUNKS → o < chunkLen FIRST c →
      ∃ slot, slot < CAPACITY ∧ place FIRST slot = (c, o)) :=
  -- The general theorem, with FIRST = 64 > 0.
  place_bijective FIRST CHUNKS (by decide)

/-! ## Part 2: the free stack's word -/

/-- `TOP = u32::MAX as u64`: the low half of the word, every bit set. -/
def TOP : Nat := 2 ^ 32 - 1

/-- `!TOP` on a `u64`: every one of the 64 bits that `TOP` does not have. -/
def NOT_TOP : Nat := 2 ^ 64 - 1 - TOP

/-- `head & TOP`: the top slot plus one, 0 when the stack is empty. -/
def topOf (w : Nat) : Nat := w &&& TOP

/-- `head & !TOP`: the high half of the word, left where it is. -/
def highOf (w : Nat) : Nat := w &&& NOT_TOP

/-- The tag: the high half read as a number, the count of changes. Rust never
names it; `next_word` changes it through `head & !TOP`. -/
def tagOf (w : Nat) : Nat := w / 2 ^ 32

/-- The word holding `tag` in its high half and `top` in its low half. -/
def pack (tag top : Nat) : Nat := 2 ^ 32 * tag + top

/-- `next_word(head, top)`: `(head & !TOP).wrapping_add(TOP + 1) | top`, the
`wrapping_add` being an add modulo `2^64`. -/
def nextWord (head top : Nat) : Nat :=
  -- The high half plus one change, wrapped to 64 bits, with the new top or-ed in.
  (highOf head + (TOP + 1)) % 2 ^ 64 ||| top

/-- **`topOf_eq`.** `head & TOP` is the word modulo `2^32`: its low half. -/
theorem topOf_eq (w : Nat) : topOf w = w % 2 ^ 32 :=
  -- And-ing with 2^32 - 1 keeps the low 32 bits, which is the remainder by 2^32.
  Nat.and_two_pow_sub_one_eq_mod w 32

/-- **`highOf_eq`.** For a 64-bit word, `head & !TOP` is its tag times
`2^32`: the low half cleared, the high half kept. -/
theorem highOf_eq (w : Nat) (hw : w < 2 ^ 64) : highOf w = 2 ^ 32 * tagOf w := by
  -- `!TOP` is 2^32 * (2^32 - 1) + 0: the high 32 bits set, the low 32 clear.
  have hnot : NOT_TOP = 2 ^ 32 * (2 ^ 32 - 1) + 0 := by decide
  -- The tag is below 2^32, since the word is below 2^64.
  have htag : w / 2 ^ 32 < 2 ^ 32 :=
    -- Dividing by 2^32 keeps a strict bound by a multiple of 2^32.
    (Nat.div_lt_iff_lt_mul (Nat.two_pow_pos 32)).mpr (by rw [← Nat.pow_add]; exact hw)
  -- Bit j of the word: from the low half below 32, from the tag at 32 and above.
  have hbits : ∀ j, w.testBit j =
      if j < 32 then (w % 2 ^ 32).testBit j else (w / 2 ^ 32).testBit (j - 32) := by
    -- Take any bit.
    intro j
    -- The bits of `2^32 * tag + low`, where the low half is below 2^32.
    have h := Nat.testBit_two_pow_mul_add (w / 2 ^ 32) (Nat.mod_lt w (Nat.two_pow_pos 32)) j
    -- `2^32 * tag + low` is the word.
    rw [Nat.div_add_mod] at h
    -- That is the claim.
    exact h
  -- Two numbers are equal when every bit is.
  apply Nat.eq_of_testBit_eq
  -- Take any bit j.
  intro j
  -- Unfold the names, and write `!TOP` and the right side as `2^32 * x + 0`.
  rw [highOf, hnot, tagOf, ← Nat.add_zero (2 ^ 32 * (w / 2 ^ 32))]
  -- Bit j of an and is the and of the bits; the bits of `2^32 * x + 0` come from x at 32 and above.
  rw [Nat.testBit_and, hbits j, Nat.testBit_two_pow_mul_add _ (Nat.two_pow_pos 32) j,
    Nat.testBit_two_pow_mul_add _ (Nat.two_pow_pos 32) j]
  -- Either the bit is in the low half or in the high half.
  by_cases hj : j < 32
  -- Low half: `!TOP` has no bit there and neither has the right side.
  ·
    -- Every side reads bit j of 0 there, which is clear.
    simp [hj]
  -- High half: both read the tag's bit j - 32, the left one only where `!TOP` has a bit.
  ·
    -- `!TOP` has bit j exactly when j - 32 < 32.
    simp only [hj, ↓reduceIte, Nat.testBit_two_pow_sub_one]
    -- Either j - 32 < 32, or the tag has no bit that high.
    by_cases hk : j - 32 < 32
    -- Inside the 64 bits: and-ing with true changes nothing.
    ·
      -- Bit j - 32 of the tag and true is that bit.
      simp [hk]
    -- Past the 64 bits: the tag, below 2^32, has no bit there.
    ·
      -- The tag is below 2^(j - 32), so that bit is clear.
      have hz : (w / 2 ^ 32).testBit (j - 32) = false :=
        -- 2^32 ≤ 2^(j - 32), and a number below a power of 2 has no bit at it.
        Nat.testBit_lt_two_pow (Nat.lt_of_lt_of_le htag (Nat.pow_le_pow_right (by decide) (by omega)))
      -- Both sides are false.
      simp [hz]

/-- **`pack_unpack`.** A word built from a tag and a top below `2^32` gives
them back: `head & TOP` is the top and the high half is the tag, and the word
fits 64 bits when the tag fits 32. -/
theorem pack_unpack (tag top : Nat) (ht : top < 2 ^ 32) :
    -- The statement: the low half is the top and the high half the tag;
    topOf (pack tag top) = top ∧ tagOf (pack tag top) = tag ∧
      -- and a tag below 2^32 makes a word below 2^64.
      (tag < 2 ^ 32 → pack tag top < 2 ^ 64) := by
  -- Unfold the names into their arithmetic.
  simp only [topOf_eq, tagOf, pack]
  -- Division and remainder by 2^32 split `2^32 * tag + top` back into its parts.
  omega

/-- **`unpack_pack`.** Every word is the pack of its tag and its top. -/
theorem unpack_pack (w : Nat) : pack (tagOf w) (topOf w) = w := by
  -- Unfold the names into their arithmetic.
  rw [pack, tagOf, topOf_eq]
  -- Quotient times divisor plus remainder is the number.
  exact Nat.div_add_mod w (2 ^ 32)

/-- **`nextWord_eq`.** For a 64-bit word and a top below `2^32`, `next_word`
is the word with that top and the tag plus one, modulo `2^32`. -/
theorem nextWord_eq (head top : Nat) (hh : head < 2 ^ 64) (ht : top < 2 ^ 32) :
    -- The statement: the new word packs tag + 1 (wrapped at 2^32) with the new top.
    nextWord head top = pack ((tagOf head + 1) % 2 ^ 32) top := by
  -- `TOP + 1` is 2^32.
  have hone : TOP + 1 = 2 ^ 32 := by decide
  -- 2^64 is 2^32 times 2^32.
  have h64 : (2 : Nat) ^ 64 = 2 ^ 32 * 2 ^ 32 := by decide
  -- Unfold `next_word` and write the high half as the tag times 2^32.
  rw [nextWord, highOf_eq head hh, hone, h64]
  -- 2^32 * tag + 2^32 is 2^32 * (tag + 1).
  rw [← Nat.mul_succ]
  -- (2^32 * (tag + 1)) mod (2^32 * 2^32) is 2^32 * ((tag + 1) mod 2^32).
  rw [Nat.mul_mod_mul_left]
  -- Or-ing a top below 2^32 into a multiple of 2^32 adds it.
  rw [pack, ← Nat.two_pow_add_eq_or_of_lt ht]

/-- **`nextWord_parts`.** After one change the word fits 64 bits, its top is
the new top and its tag is the old tag plus one, modulo `2^32`. -/
theorem nextWord_parts (head top : Nat) (hh : head < 2 ^ 64) (ht : top < 2 ^ 32) :
    -- The statement: 64 bits, the new top, the next tag.
    nextWord head top < 2 ^ 64 ∧ topOf (nextWord head top) = top ∧
      -- and its tag is the old tag plus one, wrapped at 2^32.
      tagOf (nextWord head top) = (tagOf head + 1) % 2 ^ 32 := by
  -- The word is a pack of the next tag and the top.
  rw [nextWord_eq head top hh ht]
  -- The next tag is below 2^32.
  have hlt : (tagOf head + 1) % 2 ^ 32 < 2 ^ 32 := Nat.mod_lt _ (Nat.two_pow_pos 32)
  -- Unpack it.
  have h := pack_unpack ((tagOf head + 1) % 2 ^ 32) top ht
  -- The three parts.
  exact ⟨h.2.2 hlt, h.1, h.2.1⟩

/-- **`nextWord_ne`.** A word after one change is never the word before it,
whatever top the change leaves: a CAS that read the word before the change
fails after it. Example ruled out: pop A then push A giving back the word the
popper read. -/
theorem nextWord_ne (head top : Nat) (hh : head < 2 ^ 64) (ht : top < 2 ^ 32) :
    -- The statement: the words differ.
    nextWord head top ≠ head := by
  -- Suppose they were equal.
  intro heq
  -- The tag of the old word is below 2^32.
  have htag : tagOf head < 2 ^ 32 :=
    -- Dividing by 2^32 keeps a strict bound by a multiple of 2^32.
    (Nat.div_lt_iff_lt_mul (Nat.two_pow_pos 32)).mpr (by rw [← Nat.pow_add]; exact hh)
  -- Then their tags would be equal,
  have h := (nextWord_parts head top hh ht).2.2
  -- so the tag would equal itself plus one, modulo 2^32,
  rw [heq] at h
  -- which no number below 2^32 does.
  omega

/-- The word after a list of changes, each leaving the next top in the list:
every successful CAS on the free stack, a pop or a push, stores `next_word`
of the word it read. -/
def changes : Nat → List Nat → Nat
  -- No change: the same word.
  | w, [] => w
  -- One change, then the rest.
  | w, t :: ts => changes (nextWord w t) ts

/-- **`changes_tag`.** After any list of changes with tops below `2^32`, the
word still fits 64 bits and its tag has gone up by the number of changes,
modulo `2^32`. -/
theorem changes_tag (tops : List Nat) :
    -- The statement: for every 64-bit word, if every top is below 2^32,
    ∀ w, w < 2 ^ 64 → (∀ t ∈ tops, t < 2 ^ 32) →
      -- the word after the changes fits 64 bits and its tag counts them.
      changes w tops < 2 ^ 64 ∧ tagOf (changes w tops) = (tagOf w + tops.length) % 2 ^ 32 := by
  -- One change at a time.
  induction tops with
  -- Case: no change.
  | nil =>
    -- Take the word.
    intro w hw _
    -- The word is unchanged, and its tag is below 2^32 so the remainder leaves it.
    refine ⟨hw, ?_⟩
    -- The tag is below 2^32, since the word is below 2^64.
    have htag : tagOf w < 2 ^ 32 :=
      -- Dividing by 2^32 keeps a strict bound by a multiple of 2^32.
      (Nat.div_lt_iff_lt_mul (Nat.two_pow_pos 32)).mpr (by rw [← Nat.pow_add]; exact hw)
    -- No changes: tag + 0 modulo 2^32 is the tag.
    simp only [changes, List.length_nil, Nat.add_zero]
    -- A number below 2^32 is its own remainder.
    exact (Nat.mod_eq_of_lt htag).symm
  -- Case: a change leaving top t, then the rest ts, with the claim for ts (ih) known.
  | cons t ts ih =>
    -- Take the word.
    intro w hw htops
    -- The first top is below 2^32.
    have ht : t < 2 ^ 32 := htops t List.mem_cons_self
    -- One change: 64 bits, the next tag.
    have h1 := nextWord_parts w t hw ht
    -- The rest, from the word after the first change.
    have h2 := ih (nextWord w t) h1.1 (fun x hx => htops x (List.mem_cons_of_mem t hx))
    -- Unfold one step of the changes and the length.
    simp only [changes, List.length_cons]
    -- The 64-bit bound, and the tag: (tag + 1) mod 2^32, then plus the rest, mod 2^32.
    refine ⟨h2.1, ?_⟩
    -- Put in the tag after the rest and after the first change.
    rw [h2.2, h1.2.2]
    -- Taking the remainder in the middle changes nothing.
    omega

/-- **`no_aba`.** After `k` changes, `0 < k < 2^32`, whatever tops they
leave, the word is not the word read before them: a thread that read the
word, paused while other threads popped and pushed, and then tries its CAS,
fails and reads the stack again. Example ruled out: the story's pop A, pop B,
push A, after which a stale pop of A would put B, which another thread
holds, back on the stack. -/
theorem no_aba (w : Nat) (tops : List Nat) (hw : w < 2 ^ 64)
    -- Every top fits the low half; between 1 and 2^32 - 1 changes were made.
    (htops : ∀ t ∈ tops, t < 2 ^ 32) (hk0 : 0 < tops.length) (hk : tops.length < 2 ^ 32) :
    -- The statement: the word now differs from the word read before the changes.
    changes w tops ≠ w := by
  -- Suppose it were the same.
  intro heq
  -- The tag counts the changes.
  have h := (changes_tag tops w hw htops).2
  -- So the old tag would equal itself plus k, modulo 2^32,
  rw [heq] at h
  -- The old tag is below 2^32.
  have htag : tagOf w < 2 ^ 32 :=
    -- Dividing by 2^32 keeps a strict bound by a multiple of 2^32.
    (Nat.div_lt_iff_lt_mul (Nat.two_pow_pos 32)).mpr (by rw [← Nat.pow_add]; exact hw)
  -- which no k between 1 and 2^32 - 1 allows.
  omega

/-! ## The RED case: the word without its tag -/

/-- `next_word` under the loom calibration `NoTag`: `(head & !TOP) | top`,
the high half left as it was. -/
def untaggedNext (head top : Nat) : Nat := highOf head ||| top

/-- A list of changes made with the untagged word. -/
def untaggedChanges : Nat → List Nat → Nat
  -- No change: the same word.
  | w, [] => w
  -- One change, then the rest.
  | w, t :: ts => untaggedChanges (untaggedNext w t) ts

/-- **`untagged_aba`.** RED `NoTag`: slots A (top 1) and B (top 2) are free,
A on top, and a thread has read the word `pack 0 1`. Other threads pop A
(top becomes B, 2), pop B (the stack is empty, 0) and push A (top 1). Without
the tag the word is `pack 0 1` again, so the paused thread's CAS from it
succeeds and puts B, which another thread holds, on the stack. With the tag
the same three changes leave tag 3, and the CAS fails. -/
theorem untagged_aba :
    -- The statement: untagged, the word comes back;
    untaggedChanges (pack 0 1) [2, 0, 1] = pack 0 1 ∧
      -- tagged, it does not: its tag is 3.
      changes (pack 0 1) [2, 0, 1] = pack 3 1 := by
  -- Both are computations on numbers.
  decide

-- End of the namespace opened at the top.
end Regolith.ClaimSlots
