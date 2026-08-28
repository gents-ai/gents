Plan research run {{ event.correlation }} for this question:

{{ doc.question }}

Scope: {{ doc.scope }}
Freshness: {{ doc.freshness }}
Audience: {{ doc.audience }}
Output requirements: {{ doc.output_requirements }}
Requested investigator count: {{ doc.investigator_count }}

Before creating assignments, call `web_collect_evidence` exactly once with reconnaissance assignment ID `{{ event.correlation }}:planning-reconnaissance`, 3–6 broad queries derived from the question, and `max_sources` 4. Use that real retrieval to discover terminology, likely primary-source classes, live disagreements, and missing facets. If retrieval fails, do not fabricate it and do not close the plan.

Then create exactly the requested number of assignments (the CLI has already constrained it to 2–8). Call `write_research_assignment` once per assignment with stable IDs such as `{{ event.correlation }}:primary-evidence`; every write must use that same exact requested count as `expected_total`. Then call `write_research_plan` exactly once. Do not supply runtime-filled `run_id`.
