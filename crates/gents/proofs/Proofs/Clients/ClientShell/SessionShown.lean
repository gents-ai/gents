/-! Whether the selected session is on screen, and so whether the desktop
keeps its seen mark (`hooks/sessionShown.ts`, `keepSeen` in
`hooks/desktopApp.ts`).

A wide window shows the selected session beside whatever page is open. A
narrow window shows one page at a time: going back from a session to the
sessions list leaves the session selected but out of sight. The seen mark
follows the selected session only while it is on screen and the window is
visible, so a reply that lands while a narrow window shows the list reads as
new. -/

namespace SessionShown

/-- The page a window shows: the session's own page, or any other. -/
inductive Page where
  | session
  | other
  deriving DecidableEq, Repr

def shown (wide : Bool) (page : Page) : Bool :=
  wide || page == .session

/-- Whether the seen mark is set to the selected session's latest work now:
the window is visible, the session is on screen, and the list holds it. -/
def keepsSeen (visible wide : Bool) (page : Page) (listed : Bool) : Bool :=
  visible && shown wide page && listed

/-- A narrow window showing another page never marks the selected session
seen, so a reply landing then reads as new. -/
theorem a_narrow_window_off_the_session_never_marks_it_seen
    (visible listed : Bool) : keepsSeen visible false .other listed = false := by
  cases visible <;> rfl

/-- A visible window showing the selected session, wide or narrow, keeps it
seen. -/
theorem the_shown_session_is_seen (wide : Bool) :
    keepsSeen true wide .session true = true := by
  cases wide <;> rfl

/-- A wide window keeps the selected session seen beside any page. -/
theorem a_wide_window_keeps_it_seen_beside_any_page (page : Page) :
    keepsSeen true true page true = true := by
  cases page <;> rfl

end SessionShown
