Review run {{ event.correlation }}, lens `{{ doc.lens }}` (`{{ doc.area_id }}`). Evidence packet paths: `{{ doc.path }}`.

Instructions: {{ doc.instructions }}

Deterministic baseline: {{ doc.baseline }}

Start from the supplied recon evidence packet, then read every assigned changed file in full and inspect the relevant consumer/usage context. For every patch-introduced type, variant, message, event, command, frame, or queue value that crosses a boundary, locate the consuming dispatch point and prove it is forwarded or handled. Use `lsp` for symbol-aware definitions, references, implementations, hover types, and symbol outlines whenever available; read the locations it returns. Use targeted Cargo tests or read-only `gh` context when they can settle a claim, backgrounding long commands through `spawn_process`. Do not rerun the full workspace check/Clippy baseline.

Every successful tool result remains authoritative for this request. Never repeat an identical tool call or reread the same line range; use the prior result. If you notice repeated exploration, stop inspecting and write the supported candidates followed by `write_scan_result`.

For each candidate, cite an exact changed `path:line` and verbatim code excerpt in `evidence`; the line must overlap the reviewed diff. Do not duplicate compiler or Clippy diagnostics. Do not report style preferences, speculative risks without a concrete execution path, requests for unrelated new architecture, or Minor/Informational suggestions.

Use only:
- Critical: memory safety, exploitable authorization/security, irreversible data loss, or cross-principal corruption.
- Major: demonstrably wrong behavior, duplicate/lost durable work, liveness/cancellation failure, panic on valid input, or a material regression.
- Cleanup: a concrete redundant execution path or reimplementation of an existing Gents/DefraDB feature, or comments/docs added by the diff that duplicate code, implementation history, or another canonical source. Cleanup needs exact evidence and a specific deletion or reuse action; it is not a style preference.

Apply all gates before writing: the issue has provable impact or maintenance cost, is actionable, is unintended, is introduced by the patch, makes no unstated assumptions, and asks for rigor proportionate to this repository. Call `write_candidate_finding` at most three times for distinct defects with confidence at least 80/100. Prefer Critical/Major over Cleanup. Set `confidence` to the integer string, set each `finding_id` to the globally unique format `{{ doc.area_id }}:<finding-slug>`, and never retry a successful write. Then call `write_scan_result` exactly once as your final write, preserving `area_id` exactly as `{{ doc.area_id }}`. Do not supply `run_id` or the scan result's `expected_total`, because those arguments are intentionally hidden and runtime-filled from this correlated request.
