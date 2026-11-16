namespace SchemaInstall

/-- A declared collection against the node's live collection of that name.
`extra`: every declared field is live with the declared definition and the
live collection has more. `pending`: no declared field conflicts and at least
one is not live yet. `conflicting`: a declared field is live with a different
definition. -/
inductive Live where
  | missing | exact | extra | pending | conflicting
  deriving DecidableEq, Repr

inductive Decision where
  | publish | satisfied | awaitsFields | fieldsPending | mismatch
  deriving DecidableEq, Repr

/-- The one schema install of a client verb (the CLI's `schema apply` and the
model's `collection create`). It is additive: it publishes a collection that
does not exist, accepts one that already holds the declared fields, and never
changes or removes a live field. A declared field that is not live is supplied
by the patches of the same request (`patches`); without them the install is
refused and names the collection update. Removing or emptying a collection
and activating a version are separate verbs with their own preview and digest.

Pack installation and authored-document creation verify a signed artifact
against the node rather than serve a client verb, and keep their exact
comparison. -/
def decide (live : Live) (patches : Bool) : Decision :=
  match live with
  | .missing => .publish
  | .exact | .extra => .satisfied
  | .pending => if patches then .awaitsFields else .fieldsPending
  | .conflicting => .mismatch

def refused (decision : Decision) : Bool :=
  decision == .fieldsPending || decision == .mismatch

/-- One request either publishes its collections or checks them against live
ones; a request that would do both is refused before anything is published. -/
def requestAdmitted (anyMissing anyLive : Bool) : Bool := !(anyMissing && anyLive)

theorem publishes_only_missing (live : Live) (patches : Bool)
    (publish : decide live patches = .publish) : live = .missing := by
  cases live <;> cases patches <;> simp_all [decide]

theorem conflict_is_always_refused (patches : Bool) :
    refused (decide .conflicting patches) = true := by
  cases patches <;> rfl

theorem live_superset_is_accepted (patches : Bool) :
    decide .extra patches = .satisfied := by
  cases patches <;> rfl

theorem pending_without_patches_is_refused : refused (decide .pending false) = true := rfl

end SchemaInstall
