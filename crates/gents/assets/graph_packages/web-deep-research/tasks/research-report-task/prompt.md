Write the final result for research run {{ event.correlation }}.

Title: {{ doc.title }}
Thesis: {{ doc.thesis }}
Outline: {{ doc.outline }}
Synthesis: {{ doc.synthesis }}
Unresolved questions: {{ doc.unresolved_questions }}

Read all supported/disputed verdicts, join them to typed evidence links and matching sources, and omit any broken join. Produce a direct, well-structured Markdown report with claim-local citations, counterevidence, and limitations. Encode `sources_json` as the complete deduplicated ledger of loaded integrity-verified sources, with each object copying `source_id`, `url`, `fetch_id`, and `content_hash`; every Markdown citation URL must occur in that ledger, while uncited ledger entries remain available for auditability. Call `write_research_result` exactly once. Do not supply runtime-filled `run_id`.
