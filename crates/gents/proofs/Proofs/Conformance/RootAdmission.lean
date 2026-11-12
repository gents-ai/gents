import Proofs.Tools.PeerRegistryDiscovery.RootAdmission
import Proofs.Conformance.Family

namespace Conformance.RootAdmissionContracts

open PeerRegistryDiscovery.RootAdmission
open Lean

structure Case where
  name : String
  operation : String
  authored : String
  blank : Bool
  stored : Bool
  observation : String
  configured : Bool
  ceiling : Option CanonicalPath
  enabled : List CanonicalPath
  published : List CanonicalPath
  candidate : Option CanonicalPath
  expected : Bool
  effective : Option CanonicalPath
  deriving DecidableEq, Repr

private def path (components : List String) : CanonicalPath := ⟨"sandbox", components⟩
private def workspace := path ["workspace"]
private def root := path ["workspace", "root"]
private def nested := path ["workspace", "root", "project"]

def cases : List Case :=
  [ ⟨"exact_create", "create", "configured", false, false, "existing", true, some workspace,
      [root], [root], some root, true, none⟩
  , ⟨"descendant_create", "create", "configured", false, false, "existing", true, some workspace,
      [root], [root], some nested, true, none⟩
  , ⟨"whitespace_create_explicit", "create", "   ", true, false, "blank", true, some workspace,
      [root], [root], none, false, none⟩
  , ⟨"whitespace_create_unconfigured", "create", "   ", true, false, "blank", false, some root,
      [], [root], none, true, none⟩
  , ⟨"no_documents_uses_ceiling", "create", "configured", false, false, "existing", false,
      some root, [], [root], some root, true, none⟩
  , ⟨"no_policy_no_ceiling_authored_root", "create", "configured", false, false, "existing",
      false, none, [], [], some root, true, none⟩
  , ⟨"all_disabled_does_not_fallback", "create", "configured", false, false, "existing", true,
      some root, [], [], some root, false, none⟩
  , ⟨"invalid_ceiling_does_not_fallback", "create", "configured", false, false,
      "invalid_ceiling", true, none, [], [], some root, false, none⟩
  , ⟨"prefix_sibling", "create", "configured", false, false, "existing", true, some workspace,
      [root], [root], some (path ["workspace", "root-other"]), false, none⟩
  , ⟨"traversal_normalizes_inside", "create", "configured", false, false, "traversal", true,
      some workspace, [root], [root], some nested, true, none⟩
  , ⟨"traversal_normalizes_outside", "create", "configured", false, false, "traversal", true,
      some workspace, [root], [root], some (path ["workspace", "outside"]), false, none⟩
  , ⟨"symlink_target_inside", "create", "configured", false, false, "symlink", true,
      some workspace, [root], [root], some nested, true, none⟩
  , ⟨"symlink_target_outside", "create", "configured", false, false, "symlink", true,
      some workspace, [root], [root], some (path ["outside", "project"]), false, none⟩
  , ⟨"nonexistent_suffix", "create", "configured", false, false, "nonexistent", true,
      some workspace, [root], [root], some (path ["workspace", "root", "future", "project"]),
      true, none⟩
  , ⟨"resolution_failure", "create", "configured", false, false, "unresolved", true,
      some workspace, [root], [root], none, false, none⟩
  , ⟨"explicit_restriction_does_not_widen", "create", "configured", false, false,
      "explicit_restriction", true, some root, [nested], [nested],
      some (path ["workspace", "root", "other"]), false, none⟩
  , ⟨"one_of_multiple_published_roots", "create", "configured", false, false, "existing", true,
      some (path []), [root, path ["srv", "source"]], [root, path ["srv", "source"]],
      some (path ["srv", "source", "repo"]), true, none⟩
  , ⟨"different_anchor", "create", "configured", false, false, "existing", true, none,
      [root], [root], some ⟨"other-volume", ["workspace", "root"]⟩, false, none⟩
  , ⟨"containing_stored_grant_overlaps_policy", "create", "configured", false, true,
      "existing", true, some workspace, [nested], [nested], some root, true, none⟩
  , ⟨"containing_stored_grant_under_default_policy", "create", "configured", false, true,
      "existing", false, some nested, [], [nested], some root, true, none⟩
  , ⟨"containing_new_authored_grant_refused", "create", "configured", false, false,
      "existing", false, some nested, [], [nested], some root, false, none⟩
  , ⟨"meet_selection_inside_ceiling_keeps_selection", "meet", "configured", false, false,
      "existing", false, some root, [], [root], some nested, true, some nested⟩
  , ⟨"meet_selection_contains_ceiling_clamps", "meet", "configured", false, false,
      "existing", false, some nested, [], [nested], some root, true, some nested⟩
  , ⟨"meet_disjoint_refused", "meet", "configured", false, false, "existing",
      false, some root, [], [root], some (path ["outside", "project"]), false, none⟩
  , ⟨"meet_blank_unconfigured_inherits_ceiling", "meet", "   ", true, false, "blank",
      false, some root, [], [root], none, true, some root⟩
  , ⟨"meet_blank_configured_refused", "meet", "   ", true, false, "blank",
      true, some root, [], [], none, false, none⟩
  ]

private def policy (c : Case) : PublicationPolicy :=
  ⟨c.configured, c.enabled.toFinset, c.ceiling⟩

def createSelectionOk (c : Case) : Prop :=
  rootSelectionOk c.configured (publishedRoots (policy c)) (c.authored.trim == "") c.candidate ∨
    (c.stored = true ∧ overlapping (publishedRoots (policy c)) c.candidate)

instance (c : Case) : Decidable (createSelectionOk c) := by
  unfold createSelectionOk
  infer_instance

def evaluates (c : Case) : Bool :=
  decide (publishedRoots (policy c) = c.published.toFinset) &&
    decide (c.blank = (c.authored.trim == "")) &&
    match c.operation with
    | "create" =>
        decide (createSelectionOk c)
    | "meet" =>
        decide (effectiveSelectionOk c.configured c.ceiling c.candidate c.effective)
    | _ => false

theorem cases_replay : ∀ c ∈ cases, evaluates c = c.expected := by native_decide

example : (cases.map (·.name)).Nodup ∧ cases.all (fun c =>
    (c.operation == "create" || c.operation == "meet") && !c.observation.trim.isEmpty &&
      c.blank == (c.authored.trim == "")) = true := by native_decide

deriving instance ToJson for CanonicalPath

structure AdmissionInput where
  name : String
  operation : String
  authored : String
  stored : Bool
  observation : String
  configured : Bool
  ceiling : Option CanonicalPath
  enabled : List CanonicalPath
  candidate : Option CanonicalPath
  deriving ToJson

structure AdmissionOutput where
  blank : Bool
  published : List CanonicalPath
  expected : Bool
  effective : Option CanonicalPath
  deriving ToJson

private def publishedList (configured : Bool) (ceiling : Option CanonicalPath)
    (enabled : List CanonicalPath) : List CanonicalPath :=
  if configured then enabled.filter (fun root => decide (withinCeiling ceiling root))
  else ceiling.toList

theorem publishedList_publishedRoots (configured : Bool) (ceiling : Option CanonicalPath)
    (enabled : List CanonicalPath) :
    (publishedList configured ceiling enabled).toFinset =
      publishedRoots ⟨configured, enabled.toFinset, ceiling⟩ := by
  cases configured
  · cases ceiling <;> simp [publishedList, publishedRoots, defaultRoots]
  · simp [publishedList, publishedRoots, List.toFinset_filter]

/-- Publication is a set; the family carries it in one canonical order. -/
private def canonicalOrder (roots : List CanonicalPath) : List CanonicalPath :=
  roots.mergeSort fun a b => decide (a.anchor < b.anchor ∨
    (a.anchor = b.anchor ∧ ¬ b.components < a.components))

private def asCase (i : AdmissionInput) : Case :=
  ⟨i.name, i.operation, i.authored, i.authored.trim == "", i.stored, i.observation,
    i.configured, i.ceiling, i.enabled, [], i.candidate, false, none⟩

@[conformance_family]
def admission : Family AdmissionInput AdmissionOutput where
  inputs := cases.map fun c =>
    { name := c.name, operation := c.operation, authored := c.authored, stored := c.stored
      observation := c.observation, configured := c.configured, ceiling := c.ceiling
      enabled := c.enabled, candidate := c.candidate }
  model := fun i =>
    let meet := effectiveSelection i.configured i.ceiling i.candidate
    { blank := i.authored.trim == ""
      published := canonicalOrder (publishedList i.configured i.ceiling i.enabled)
      expected := match i.operation with
        | "create" => decide (createSelectionOk (asCase i))
        | "meet" => meet.isSome
        | _ => false
      effective := if i.operation == "meet" then meet.join else none }

end Conformance.RootAdmissionContracts
