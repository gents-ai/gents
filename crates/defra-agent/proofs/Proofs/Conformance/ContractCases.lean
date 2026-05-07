import Proofs.Conformance.ContractCases.Runtime
import Proofs.Conformance.ContractCases.SlotAccounting
import Proofs.Conformance.ContractCases.SessionRecovery
import Proofs.Conformance.ContractCases.BoundaryRuntime

/-!
# Finite Conformance Witness Cases

Representative executable witnesses emitted by `Proofs.Conformance.Contracts`.
The cases stay finite and deterministic so Rust can consume them as a contract
without re-implementing Lean evaluation.
-/
