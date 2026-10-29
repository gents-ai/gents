import Proofs.ClientShell.ObservationOrdering
import Lean

open Lean EnrollmentActivationObservation

private def failureName : Failure → String
  | .authentication => "authentication"
  | .route => "route"

private def authorityJson (a : Authority) : Json :=
  Json.mkObj [("request", toJson a.request), ("digest", toJson a.digest),
    ("generation", toJson a.generation), ("expiry", toJson a.expiresAt)]

def main : IO Unit := do
  let requested : Authority := ⟨1, 2, 3, 4⟩
  let authorities := [none, some requested, some ⟨1, 2, 4, 4⟩,
    some ⟨1, 2, 3, 5⟩, some ⟨5, 2, 3, 4⟩, some ⟨1, 5, 3, 4⟩]
  let cases := authorities.flatMap fun initial =>
    (if initial.isSome then [none, some Failure.authentication, some .route] else [none]).flatMap fun prior =>
      authorities.flatMap fun selected =>
        [none, some Failure.authentication, some .route].flatMap fun result =>
          [false, true].flatMap fun approved =>
            [false, true].map fun routeObserved =>
              let state : State := ⟨initial, prior⟩
              let chosen := select state selected
              let finished := finish chosen requested result
              Json.mkObj [
                ("requested", authorityJson requested),
                ("initial", toJson (initial.map authorityJson)),
                ("selected", toJson (selected.map authorityJson)),
                ("prior", toJson (prior.map failureName)),
                ("result", toJson (result.map failureName)),
                ("approved", toJson approved),
                ("routeObserved", toJson routeObserved),
                ("stored", toJson (finished.failure.map failureName)),
                ("projected", toJson ((project finished requested approved routeObserved).map failureName))]
  IO.println (toJson cases).compress
