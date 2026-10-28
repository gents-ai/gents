import Proofs.ClientShell.ObservationOrdering
import Lean

open Lean EnrollmentActivationObservation

private def failureName : Failure → String
  | .authentication => "authentication"
  | .route => "route"

def main : IO Unit := do
  let requested : Authority := ⟨1, 2, 3⟩
  let cases := [none, some requested, some ⟨1, 2, 4⟩].flatMap fun current =>
    [none, some Failure.authentication, some .route].flatMap fun prior =>
      [none, some Failure.authentication, some .route].flatMap fun result =>
        [false, true].map fun approved =>
          let state : State := ⟨current, prior⟩
          let finished := finish state requested result
          Json.mkObj [
            ("current", toJson (current.map fun a => a.generation)),
            ("prior", toJson (prior.map failureName)),
            ("result", toJson (result.map failureName)),
            ("approved", toJson approved),
            ("stored", toJson (finished.failure.map failureName)),
            ("projected", toJson ((project finished requested approved).map failureName))]
  IO.println (toJson cases).compress
