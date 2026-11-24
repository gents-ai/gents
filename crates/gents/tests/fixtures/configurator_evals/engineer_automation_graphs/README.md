# engineer_automation_graphs

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). A case's
first stage asks for an automation in the user's words, saying what each run
should reply or where it should post; later seed stages write the records it
watches, one at a time, and the case grades what came out: a line of the reply
of the run each record started that is the verdict asked for, the requests each
record started (where they landed and what they carry), and that nothing fired
twice. No check reads
the configuration the Engineer wrote or which tools it called, so a filter, a
template and a prompt that decides are all fair routes to the same outcome.
Requests are matched by the record's own identifier, so a test record the
Engineer adds while building does not count against it. A trial has one
inference slot, so a run cannot start while the Engineer's own turn holds it;
prompts that start runs say not to wait for them.

A seed stage waits for its own record to start a request, so every seeded
record has something to do under a correct setup. Existing conversations are
fixture sessions. Automation a case changes is built by its own earlier stage,
because the Engineer neither sees nor owns evaluator-written Tasks.

## Cases

| Case | Split | User need | Features exercised | Graded by |
| --- | --- | --- | --- | --- |
| inspection-band | train | a verdict on each new reading against a numeric band | event source, filter or prompt logic on strict bounds, task template | each run's reply (INSPECT or CLEAR and the id) at 4, 8 and 7 units; one run per reading |
| dock-alert-triage | train | triage each alert, including ones with no severity | a missing field in filters or templates | each run's reply for info, missing, critical and warn; one run per alert, so a filter that fires twice fails |
| scale-card-repair | train | each card posted to two conversations, then one branch changed in place | fan-out, session templates into existing sessions, editing an automation | posts in scale-receipts and scale-audit before and after the change: the light card gets no receipt, the heavy card's receipt has its lane, audits continue, nothing fires again |
| proof-slip-run | train | a task run by hand for each open slip, then a change previewed | manual Task with arguments, task runs, config preview | one completed run per open slip carrying its ref and units, none for the closed slip, file unchanged; after the preview the task and runs are unchanged and the reply shows what a run would reply |
| declared-quorum | train | a batch note once a batch has the number of parts it declares | grouped event source counting from a field, correlation | the parts' replies; exactly one completed note for ASH, none for ELM |
| paired-crate-scans | validation | a lot note once both scans of a lot are in | grouped fan-in on a fixed count, group template fields | the scans' replies; exactly one completed note for KITE, none for LARK |
| redline-follow-up | validation | a follow-up once a confirmation has finished | emit_outcome, an event source on FireOutcome with a filter | the confirmation's reply; exactly one completed FOLLOW redline-8 run, started by a trigger on finished-run records |
| desk-ticket-routing | validation | tickets posted into existing desk conversations, urgent ones to duty | session routing by field, with an exception | each ticket's post and session; no desk session added |
| cold-packing-chain | held_out | the chain need of redline-follow-up on packing slips | as redline-follow-up | as redline-follow-up |
| bay-thread-per-key | held_out | one conversation per bay, started by its first slip | session template with create_session | each slip's post and session; bay-west exists once |

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-automation-graphs`;
held-out cases run only with `GENTS_EVAL_SPLITS=held_out`.
