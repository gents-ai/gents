import Proofs.CommandPolicy.Types
import Proofs.CommandPolicy.Sandbox
import Proofs.CommandPolicy.Validation
import Proofs.Workspace.Types

namespace CommandPolicy

namespace ExecutionMode

-- This is a coarse effect classification, not a proof of OS enforcement.
def sourceWrites : ExecutionMode → Bool
  | .workspaceWrite | .unrestricted => true
  | _ => false

def artifactWrites : ExecutionMode → Bool
  | .artifactWrite | .unrestricted => true
  | _ => false

def meet (a b : ExecutionMode) : ExecutionMode :=
  if a = b then a
  else match a, b with
  | .unrestricted, x | x, .unrestricted => x
  | _, _ => .readOnly

def Below (a b : ExecutionMode) : Prop :=
  (a.sourceWrites = true → b.sourceWrites = true) ∧
  (a.artifactWrites = true → b.artifactWrites = true)

instance (a b : ExecutionMode) : Decidable (Below a b) := by
  unfold Below
  infer_instance

theorem meet_comm (a b : ExecutionMode) : meet a b = meet b a := by
  cases a <;> cases b <;> decide

theorem meet_assoc (a b c : ExecutionMode) : meet (meet a b) c = meet a (meet b c) := by
  cases a <;> cases b <;> cases c <;> decide

theorem meet_idem (a : ExecutionMode) : meet a a = a := by
  cases a <;> decide

theorem meet_below_left (a b : ExecutionMode) : Below (meet a b) a := by
  cases a <;> cases b <;> decide

theorem meet_below_right (a b : ExecutionMode) : Below (meet a b) b := by
  cases a <;> cases b <;> decide

theorem incomparable : meet .artifactWrite .workspaceWrite = .readOnly ∧
    ¬ Below .artifactWrite .workspaceWrite ∧ ¬ Below .workspaceWrite .artifactWrite := by
  decide

end ExecutionMode

/-- Verified observations from the existing workspace/execution/host owners.
Construction requires an active workspace binding and a current live execution
that has not been canceled. incarnationMatches includes that live-owner check,
not merely equality of an old generation value. These facts must be revalidated
at launch where revocation can race preparation.
No predicate here proves path canonicalization, kernel policy, or host isolation. -/
structure ArtifactBinding where
  authority : BindingAuthority
  state : WorkspaceState
  sealMatches : Bool
  ownerMatches : Bool
  incarnationMatches : Bool
  privateRootEligible : Bool
  sandboxEnforced : Bool
  deriving DecidableEq, Repr

def ArtifactBinding.eligible (c : ArtifactBinding) : Bool :=
  c.authority == .readOnly && c.state == .sealed && c.sealMatches &&
  c.ownerMatches && c.incarnationMatches && c.privateRootEligible && c.sandboxEnforced

/-- Deliberately separate from ordinary attenuation: an explicit artifact selection
can obtain this contextual grant; no WorkspaceWrite-to-artifact conversion exists.
`requested` is the result AFTER behavior/selection/operator-ceiling meet, never a
raw model or tool-selection request that bypasses a restrictive ceiling. -/
def admitArtifact (requested : ExecutionMode) (binding : Option ArtifactBinding) : Bool :=
  requested == .artifactWrite && binding.any ArtifactBinding.eligible

theorem admitted_explicit {m : ExecutionMode} {c : Option ArtifactBinding}
    (h : admitArtifact m c = true) : m = .artifactWrite := by
  simp only [admitArtifact, Bool.and_eq_true, beq_iff_eq] at h
  exact h.1

theorem missing_binding_denied (m : ExecutionMode) : admitArtifact m none = false := by
  simp [admitArtifact]

theorem admitted_checks {m : ExecutionMode} {c : ArtifactBinding}
    (h : admitArtifact m (some c) = true) :
    c.authority = .readOnly ∧ c.state = .sealed ∧ c.sealMatches = true ∧
    c.ownerMatches = true ∧ c.incarnationMatches = true ∧
    c.privateRootEligible = true ∧ c.sandboxEnforced = true := by
  simp only [admitArtifact, Option.any, ArtifactBinding.eligible,
    Bool.and_eq_true, beq_iff_eq, and_assoc] at h
  exact h.2

/-- Initial implementation may deny persistent LSP rather than introduce another
artifact lifetime owner. Both allowed dispatch and denial preserve the bound mode. -/
inductive SpawnKind where
  | foreground | background | persistentLsp
  deriving DecidableEq, Repr

def artifactSpawn (kind : SpawnKind) (m : ExecutionMode)
    (binding : Option ArtifactBinding) : Option ExecutionMode :=
  if kind == .persistentLsp then none
  else if admitArtifact m binding then some .artifactWrite else none

theorem spawned_never_escalates {k : SpawnKind} {m out : ExecutionMode}
    {c : Option ArtifactBinding} (h : artifactSpawn k m c = some out) :
    out = .artifactWrite := by
  simp only [artifactSpawn] at h
  split at h
  · contradiction
  · split at h
    · simpa using h.symm
    · contradiction

theorem artifact_requires_sandbox (support : RuntimeSupport) (sandbox : SandboxKind)
    (h : selectSandbox support .artifactWrite = .selected sandbox) :
    support.workspaceWriteSandboxEnforced = true ∧ sandbox = .macosSeatbelt := by
  cases support with
  | mk enforced =>
    cases enforced <;> simp [selectSandbox] at h ⊢
    exact h.symm

/-- This theorem proves only the argv-policy validation result. It does not
establish that sandbox selection or dispatch happened. A composed runtime launch
consumer must separately fence context, enforcing sandbox, and disabled network. -/
theorem artifact_disabled_network_validation (request : CommandRequest) :
    validateNetworkMode .artifactWrite .disabled request = .allow := rfl

namespace ArtifactCases

def modes : List ExecutionMode := [.readOnly, .workspaceWrite, .artifactWrite, .unrestricted]
def meetCases : List (ExecutionMode × ExecutionMode × ExecutionMode) :=
  modes.flatMap fun a => modes.map fun b => (a, b, a.meet b)

def valid : ArtifactBinding :=
  ⟨.readOnly, .sealed, true, true, true, true, true⟩

def admissionCases : List (String × ExecutionMode × Option ArtifactBinding × Bool) :=
  [ ("sealed_readonly_explicit", .artifactWrite, some valid, true)
  , ("missing_binding", .artifactWrite, none, false)
  , ("integrate_not_artifact", .artifactWrite, some {valid with authority := .integrate}, false)
  , ("readwrite_not_artifact", .artifactWrite, some {valid with authority := .readWrite}, false)
  , ("unsealed", .artifactWrite, some {valid with state := .ready}, false)
  , ("wrong_seal", .artifactWrite, some {valid with sealMatches := false}, false)
  , ("wrong_owner", .artifactWrite, some {valid with ownerMatches := false}, false)
  , ("stale_incarnation", .artifactWrite, some {valid with incarnationMatches := false}, false)
  , ("foreign_root", .artifactWrite, some {valid with privateRootEligible := false}, false)
  , ("unsupported_platform", .artifactWrite, some {valid with sandboxEnforced := false}, false)
  , ("workspace_write_not_converted", .workspaceWrite, some valid, false)
  , ("unrestricted_not_implicit_artifact", .unrestricted, some valid, false)
  , ("readonly_not_implicit_artifact", .readOnly, some valid, false) ]

theorem admission_cases_agree : admissionCases.all
    (fun (_, m, c, expected) => admitArtifact m c == expected) = true := by decide

theorem all_sixteen_mode_pairs : meetCases.length = 16 := by decide

structure SpawnCase where
  name : String
  kind : SpawnKind
  binding : Option ArtifactBinding
  expected : Option ExecutionMode
  deriving Repr

def spawnCases : List SpawnCase :=
  [ ⟨"foreground_valid", .foreground, some valid, some .artifactWrite⟩
  , ⟨"background_valid", .background, some valid, some .artifactWrite⟩
  , ⟨"persistent_lsp_denied_before_pool_lookup", .persistentLsp, some valid, none⟩
  , ⟨"background_missing_context", .background, none, none⟩
  , ⟨"background_stale_or_canceled_owner", .background,
      some {valid with incarnationMatches := false}, none⟩ ]

theorem spawn_cases_agree : spawnCases.all (fun c =>
    artifactSpawn c.kind .artifactWrite c.binding == c.expected) = true := by decide

end ArtifactCases
end CommandPolicy
