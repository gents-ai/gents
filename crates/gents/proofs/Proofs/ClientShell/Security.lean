namespace ClientShell.Security

/-- Native URL parsing supplies exact scheme/host/port equality. The shell may
    navigate only within its configured application origin or the explorer
    origin explicitly selected by the native runtime owner.
    WebKit also reports the shell's sandboxed srcdoc frames here. The exact
    `about:srcdoc` document has no URL-supplied payload or independent origin;
    frame sandbox/CSP still own its authority. It is not an explorer origin. -/
def navigate (applicationWindow application explorer sandboxFrame : Bool) : Bool :=
  if applicationWindow then application || sandboxFrame else explorer

def pluginEnabled (development : Bool) (manifest choice : Option Bool) : Bool :=
  choice.getD (if development then false else manifest.getD true)

/-- Transient activation alone is not permission to send in the user's name.
    The person must confirm the exact proposed text, and its selected target
    must still match after that asynchronous confirmation. -/
def send (gesture confirmed sameTarget : Bool) : Bool :=
  gesture && confirmed && sameTarget

theorem developmentRequiresChoice (manifest : Option Bool) :
    pluginEnabled true manifest none = false := by rfl

theorem unconfirmedNeverSends (gesture sameTarget : Bool) :
    send gesture false sameTarget = false := by simp [send]

theorem changedTargetNeverSends (gesture confirmed : Bool) :
    send gesture confirmed false = false := by simp [send]

theorem unrelatedOriginRefused (applicationWindow : Bool) :
    navigate applicationWindow false false false = false := by cases applicationWindow <;> rfl

theorem frameCannotNavigateExplorer (application : Bool) :
    navigate false application false true = false := rfl

end ClientShell.Security
