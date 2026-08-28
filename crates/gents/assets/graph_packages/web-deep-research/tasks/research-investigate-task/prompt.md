Investigate run {{ event.correlation }}, assignment `{{ doc.assignment_id }}`.

Question: {{ doc.question }}
Lens: {{ doc.lens }}
Instructions: {{ doc.instructions }}
Query plan: {{ doc.query_plan }}
Source requirements: {{ doc.source_requirements }}
Freshness: {{ doc.freshness }}

Call `web_collect_evidence` exactly once with assignment ID `{{ doc.assignment_id }}`, 3–6 distinct queries from the query plan above, and `max_sources` 6. It performs the complete capped network retrieval; this deployment exposes no raw network tools. Persist at least two useful bundled sources with their exact quote-verification fields, then write 6–8 atomic claims and at least one typed evidence link per claim. Evidence links reference the authoritative source by `source_id`; they do not duplicate its fetch ID or hash. Stable source, claim, and evidence IDs must begin with `{{ doc.assignment_id }}:`. Never retry a successful write. Call `write_research_investigation` exactly once as the final write, only after at least two sources, six claims, and six evidence links are durable, with honest counts and a `complete` or `partial` status. Do not supply runtime-filled `run_id`, `assignment_id`, or `expected_total`.
