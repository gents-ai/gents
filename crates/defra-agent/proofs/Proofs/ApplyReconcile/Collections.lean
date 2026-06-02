import Proofs.Basic
import Proofs.RuntimeReconcile
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Image
import Mathlib.Data.Finset.SDiff

/-!
# Apply/Reconcile Collections

Collection ordering and document references for the apply model.
-/

namespace ApplyReconcile

/-- Operator-controlled document collections. Mirrors the Rust
    `enum Collection` in `defra-agent-cli`. -/
inductive Collection where
  | agentPrincipal
  | agentBehavior
  | skill
  | toolSelection
  | inferenceBackend
  | inferenceProfile
  | toolServiceRegistry
  | task
  | schedule
  | eventTrigger
  deriving DecidableEq, Repr

/-- Apply ordering rank. Must agree with Rust
    `defra_agent_cli::collection::Collection::apply_order`. -/
def Collection.applyOrder : Collection → Nat
  | .inferenceBackend      => 0
  | .toolSelection         => 0
  | .inferenceProfile      => 0
  | .toolServiceRegistry   => 0
  | .skill                 => 0
  | .agentBehavior         => 1
  | .task                  => 2
  | .schedule              => 2
  | .agentPrincipal        => 3
  | .eventTrigger          => 3

/-- Comparison on Collection: by `applyOrder` rank. -/
instance : LT Collection where
  lt a b := Collection.applyOrder a < Collection.applyOrder b

instance : LE Collection where
  le a b := Collection.applyOrder a ≤ Collection.applyOrder b

instance (a b : Collection) : Decidable (a < b) :=
  Nat.decLt (Collection.applyOrder a) (Collection.applyOrder b)

instance (a b : Collection) : Decidable (a ≤ b) :=
  Nat.decLe (Collection.applyOrder a) (Collection.applyOrder b)

/-- A document identifier — collection plus opaque id. -/
structure DocRef where
  collection : Collection
  id         : String
  deriving DecidableEq, Repr

/-- Comparison on DocRef: (collection.applyOrder, id) lexicographic.
    Defined as a `Bool`-valued helper so it can drive `List.mergeSort`. -/
def DocRef.le (a b : DocRef) : Bool :=
  if a.collection.applyOrder < b.collection.applyOrder then true
  else if a.collection.applyOrder > b.collection.applyOrder then false
  else a.id ≤ b.id

instance : LE DocRef where
  le a b := DocRef.le a b = true

instance (a b : DocRef) : Decidable (a ≤ b) := by
  unfold LE.le instLEDocRef
  infer_instance


/-- Exhaustive Collection pattern-match acting as a parity contract
    with the Rust `defra_agent::Collection` enum. When the Rust enum
    gains a variant, the Rust-side test
    `collection::tests::canonical_variants_and_ranks` breaks first;
    this example's pattern-match also becomes non-exhaustive and fails
    the Lean build. Both must be updated together. -/
example (c : Collection) : Nat :=
  match c with
  | .agentPrincipal       => 3
  | .agentBehavior        => 1
  | .skill                => 0
  | .toolSelection        => 0
  | .inferenceBackend     => 0
  | .inferenceProfile     => 0
  | .toolServiceRegistry  => 0
  | .task                 => 2
  | .schedule             => 2
  | .eventTrigger         => 3

/-- Sanity: the exhaustive example's rank map equals applyOrder. -/
theorem applyOrder_matches_parity_contract : ∀ c : Collection,
    Collection.applyOrder c =
      (match c with
       | .agentPrincipal       => 3
       | .agentBehavior        => 1
       | .skill                => 0
       | .toolSelection        => 0
       | .inferenceBackend     => 0
       | .inferenceProfile     => 0
       | .toolServiceRegistry  => 0
       | .task                 => 2
       | .schedule             => 2
       | .eventTrigger         => 3) := by
  intro c
  cases c <;> rfl


end ApplyReconcile
