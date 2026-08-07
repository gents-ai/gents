Job {{ doc.job_id }} (arm {{ doc.arm }}, suite {{ doc.suite }}) fired by
trigger {{ event.trigger_id }} on {{ event.source_collection }} doc
{{ event.source_doc_id }} at {{ ctx.now }}, handled by behavior
{{ node.behavior_id }} on {{ node.node_did }}.

{{ doc.prompt }}

After answering, use write_experiment_finding to record your finding for job_id={{ doc.job_id }}.
