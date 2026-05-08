import Proofs.Conformance.ContractCases.Runtime
import Proofs.Conformance.ContractCases.SlotAccounting
import Proofs.Conformance.ContractCases.SessionRecovery
import Proofs.Conformance.ContractCases.BoundaryRuntime
import Proofs.Conformance.ContractCases.LifecycleTransitions
import Proofs.Conformance.ContractCases.LiveOverlay

/-!
# Finite Conformance Witness Cases

Representative executable witnesses emitted by `Proofs.Conformance.Contracts`.
The cases stay finite and deterministic so Rust can consume them as a contract
without re-implementing Lean evaluation. Request and Process transition cases
cover every source/target pair and classify reserved product vocabulary
separately from ordinary denied transitions.
-/
