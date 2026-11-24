namespace ClientShell.ViewRest

/-- What the transcript's scroll owner has observed of the reader's motion:
whether a finger or a scrollbar drag holds the view, how long since the last
wheel, touch or reader scroll, and how many drawn frames in a row have found
the view where the frame before left it since the last wheel that could move
it, touch or reader scroll. The owner's own position writes are not motion. -/
structure Motion where
  touching : Bool
  dragging : Bool
  sinceMotionMs : Nat
  stillFrames : Nat
  deriving DecidableEq, Repr

def restMs : Nat := 150

def stillFramesNeeded : Nat := 2

/-- The view rests, and the owner may write its position, only once nothing
holds it, no motion has come for `restMs`, and it has stood still across
`stillFramesNeeded` drawn frames.

External premise: WebKit moves an animated wheel scroll one step per drawn
frame and drops the rest of it when the position is written; a loaded page
draws frames further apart than `restMs`. Time alone would call the view
rested between two steps of one wheel, and the write would cut the reader's
travel short. -/
def rests (m : Motion) : Bool :=
  !m.touching && !m.dragging && decide (restMs ≤ m.sinceMotionMs) &&
    decide (stillFramesNeeded ≤ m.stillFrames)

/-- However long since the last motion, a view still moving between frames
does not rest. -/
theorem moving_view_never_rests (m : Motion) (h : m.stillFrames < stillFramesNeeded) :
    rests m = false := by
  have : ¬ stillFramesNeeded ≤ m.stillFrames := Nat.not_le.mpr h
  simp [rests, this]

theorem recent_motion_never_rests (m : Motion) (h : m.sinceMotionMs < restMs) :
    rests m = false := by
  have : ¬ restMs ≤ m.sinceMotionMs := Nat.not_le.mpr h
  simp [rests, this]

theorem held_view_never_rests (m : Motion) (h : m.touching = true ∨ m.dragging = true) :
    rests m = false := by
  rcases h with h | h <;> simp [rests, h]

/-- A view nothing holds, quiet for `restMs` and still across the frames,
rests: the owner's correction is never deferred forever. -/
theorem quiet_still_view_rests (m : Motion) (ht : m.touching = false)
    (hd : m.dragging = false) (hq : restMs ≤ m.sinceMotionMs)
    (hs : stillFramesNeeded ≤ m.stillFrames) : rests m = true := by
  simp [rests, ht, hd, hq, hs]

/-- Every combination of holds around both thresholds. -/
def cases : List Motion :=
  [false, true].flatMap fun touching => [false, true].flatMap fun dragging =>
    [0, restMs - 1, restMs, restMs + 1, 10 * restMs].flatMap fun sinceMotionMs =>
      [0, stillFramesNeeded - 1, stillFramesNeeded, stillFramesNeeded + 1].map
        fun stillFrames => { touching, dragging, sinceMotionMs, stillFrames }

private def bool (b : Bool) : String := if b then "true" else "false"

def caseJson (m : Motion) : String :=
  "{\"touching\":" ++ bool m.touching ++ ",\"dragging\":" ++ bool m.dragging ++
    ",\"sinceMotionMs\":" ++ toString m.sinceMotionMs ++
    ",\"stillFrames\":" ++ toString m.stillFrames ++
    ",\"rests\":" ++ bool (rests m) ++ "}"

def casesJson : String := "[" ++ String.intercalate "," (cases.map caseJson) ++ "]"

end ClientShell.ViewRest
