Set up a reusable document-triggered workflow using the existing Builder behavior.

An external client will create documents in a new `EvalAutomationInput` collection
with string fields `correlation` and `message`. For each new input, Builder should
uppercase the message and publish exactly one document in `EvalAutomationOutput`
with string fields `correlation` and `result`, preserving the input correlation.

Create the schemas, a narrowly scoped output datastore tool, and the reusable
task/event-source/trigger configuration yourself. Use templating and runtime-filled
correlation so each invocation uses its own input. Attach the necessary datastore
surface to Builder while preserving its coding tools, root, and other settings.
Do not grant unrestricted database access, change inference profiles, or modify
Setup. Do not precreate output documents: the workflow must produce them after
the external client submits input. Inspect and verify the configuration, and
explain what will happen when an input arrives.
