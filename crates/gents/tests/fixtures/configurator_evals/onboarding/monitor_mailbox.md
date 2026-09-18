Configure a full synthetic document-triggered monitoring automation, with behavior named Mailbox Monitor using the existing onboarding-medium profile and root {{ROOT}}. Keep Setup/default and all inference configuration unchanged. No real machine inspection, shell commands, schedules or repairs are authorized.

First preview only. Propose the smallest working configuration for this scenario: a fresh monitor request receives two synthetic findings, disk=81% and docker=unavailable. It must notify that request's requester through the built-in mailbox, accurately covering both findings. A combined acknowledgment summary is welcome; do not drop a finding or authorize repairs. Repeating the same findings must not add duplicates. Do not invent a custom mailbox collection. Discover the canonical datastore declaration and bind it through the ordinary config owners. The monitor's initial literal system prompt must include this version marker and safety instruction (their layout is up to you):
MONITOR_CHECKS_V1
No repairs without user approval.

Explain unsupported recipient/repair routing honestly. Wait for approval before writes.

Requirements for the proposal and later approved implementation: an input schema `EvalMailboxInput` with string fields `correlation` and `message`, plus the Task, EventSource and enabled parallel Trigger watching creation of EvalMailboxInput documents. The task must render the input message into the monitor's request using MiniJinja; do not hardcode test readings in the task. An external client will write inputs only; it will not create or repair your automation. Each message supplies two synthetic conditions to file as canonical acknowledgment flags. Configure the canonical notification policy to maintain an open monitoring summary across inputs. No custom output collection is needed: MailboxItem is the output.

This turn is read/preview only. Do not install the schema, create or edit configuration, or precreate input/mailbox documents. If a dependent preview requires a not-yet-created document, explain that dependency; do not create it to make preview pass. Stop with the proposal and wait for the separate approval message.
