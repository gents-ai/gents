namespace TextNormalization

/-- Native objective normalization uses Rust's Unicode White_Space property,
as specified by `char::is_whitespace`, rather than Lean's ASCII whitespace.
https://doc.rust-lang.org/std/primitive.char.html#method.is_whitespace -/
def objectiveWhitespaceRanges : List (Nat × Nat) :=
  [(0x9, 0xD), (0x20, 0x20), (0x85, 0x85), (0xA0, 0xA0), (0x1680, 0x1680),
   (0x2000, 0x200A), (0x2028, 0x2029), (0x202F, 0x202F), (0x205F, 0x205F), (0x3000, 0x3000)]

def objectiveWhitespace (c : Char) : Bool :=
  objectiveWhitespaceRanges.any fun (first, last) => first ≤ c.toNat && c.toNat ≤ last

def objectiveWhitespaceCodepoints : List Nat :=
  objectiveWhitespaceRanges.flatMap fun (first, last) =>
    (List.range (last - first + 1)).map (first + ·)

def canonicalObjective (objective : String) : String :=
  (objective.dropWhile objectiveWhitespace).dropRightWhile objectiveWhitespace

end TextNormalization
