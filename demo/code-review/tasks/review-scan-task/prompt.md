Review run {{ event.correlation }}, area `{{ doc.area_id }}` at `{{ doc.path }}`.

Instructions: {{ doc.instructions }}

Inspect the repository with read-only tools. Write zero or more findings, then exactly one scan result. Supply `area_id` and content fields; the runtime supplies `run_id` and the immutable `expected_total` snapshot.
