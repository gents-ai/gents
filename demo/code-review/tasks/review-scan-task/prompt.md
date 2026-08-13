Review run {{ event.correlation }}, lens `{{ doc.lens }}` (`{{ doc.area_id }}`). Evidence packet paths: `{{ doc.path }}`.

Instructions: {{ doc.instructions }}

Deterministic baseline: {{ doc.baseline }}

Analyze the supplied recon evidence packet. For each candidate, cite its exact supplied `path:line` and verbatim code excerpt in `evidence`. The verifier will independently reread the complete enclosing function and relevant usages in the next stage. Do not duplicate compiler or Clippy diagnostics. Do not report style preferences, speculative risks without a concrete execution path, requests for unrelated new architecture, or Minor/Informational suggestions.

Use only:
- Critical: memory safety, exploitable authorization/security, irreversible data loss, or cross-principal corruption.
- Major: demonstrably wrong behavior, duplicate/lost durable work, liveness/cancellation failure, panic on valid input, or a material regression.

Call `write_candidate_finding` at most once, choosing only the highest-confidence, highest-severity evidenced defect for this lens. Set its `finding_id` to the globally unique format `{{ doc.area_id }}:<finding-slug>` and never retry a successful write. Then call `write_scan_result` exactly once as your final write, preserving `area_id` exactly as `{{ doc.area_id }}`. Do not supply `run_id` or the scan result's `expected_total`, because those arguments are intentionally hidden and runtime-filled from this correlated request.
