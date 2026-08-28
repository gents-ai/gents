Investigate run {{ event.correlation }}, assignment `{{ doc.assignment_id }}`.

Question: {{ doc.question }}
Lens: {{ doc.lens }}
Instructions: {{ doc.instructions }}
Query plan: {{ doc.query_plan }}
Source requirements: {{ doc.source_requirements }}
Freshness: {{ doc.freshness }}

Call `web_collect_evidence` exactly once with assignment ID `{{ doc.assignment_id }}`, 3–6 distinct queries from the query plan above, and `max_sources` 6. It performs the complete capped network retrieval; never call raw search, scrape, crawl, map, or browser tools and never repeat the bundle call. Persist every useful bundled source, then write 6–8 atomic supported, disputed, or negative claims and the sentinel. Stable IDs must begin with `{{ doc.assignment_id }}:`. Never retry a successful write. Call `write_research_investigation` exactly once as the final write. Do not supply runtime-filled `run_id`, `assignment_id`, or `expected_total`.
