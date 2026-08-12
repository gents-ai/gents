Run {{ group.correlation_value }} has {{ group.count }} scan results (complete={{ group.complete }}):

{{ group.docs | tojson }}

Query `Finding` with `run_id == "{{ group.correlation_value }}"`, triage the results, and write one report.
