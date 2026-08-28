Plan research run {{ event.correlation }} for this question:

{{ doc.question }}

Scope: {{ doc.scope }}
Freshness: {{ doc.freshness }}
Audience: {{ doc.audience }}
Output requirements: {{ doc.output_requirements }}
Requested investigator count: {{ doc.investigator_count }}

Create exactly the requested number of assignments (the CLI has already constrained it to 2–8). Call `write_research_assignment` once per assignment with stable IDs such as `{{ event.correlation }}:primary-evidence`; every write must use that same exact requested count as `expected_total`. Then call `write_research_plan` exactly once. Do not supply runtime-filled `run_id`.
