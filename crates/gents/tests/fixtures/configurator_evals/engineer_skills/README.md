# engineer_skills

## Configuration

Use the bundled pack_config.json. The subject is `../engineer_full` with every
core pack installed, so the pack_builder skills are in the Engineer's catalog;
`subject_patch.json` adds four domain Skill documents to that catalog. No
check requires a skill load: a case is graded on the outcome, and where only
a skill states the rules (the four added ones), the right answer depends on
reading it. A stage that needs a write grant the Engineer gives itself (which
applies from its next request) settles and may send one "OK, go ahead." turn
before it is graded.

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| plan-before-build | a plan and done-checks for a loaner laptop app, nothing built yet | `load_skill` (plan-work), `write_file` | ACCEPTANCE.md lists checks including a student must-not; no page, trigger, schedule or surface created |
| courier-window-tickets | which queue each courier arrival goes to | added skill rules in order, file reads | each ticket paired with its queue in the reply; tickets unchanged |
| archive-retention-records | how long the records office keeps each record | added skill, a legal hold | each record's months, or the hold, in the reply; records unchanged |
| trigger-never-fires | why an expected automation never ran, then fix it and use it | diagnose-node order, read-only `config`, automation setup, a self-granted write | first stage changes nothing and says no automation exists; the slip the user then adds fires the automation, its request completes, and an inbox note names the slip |
| courier-ticket-collection | a collection for tickets that arrive twice, then load a feed | use-database, `schema`, self-granting write access, writes | the collection with a unique ticket_number and a DateTime arrival; the feed lands as two tickets, CT-1001 once |
| plugin-tool-missing | what to change for an agent to get a plugin tool, and when it appears | product knowledge of Tools and requests | the agent's Tools and the next request in the reply |
| ferry-reimbursement-claims | what is payable on each claim | added skill, caps and the late-filing rule | each claim's amount; claims unchanged |
| urgent-count-once | a one-off count, and whether an app is needed | `query` | three urgent slips; no page, trigger or schedule built |
| lab-holding-periods | how long the lab holds each item | the lab skill, not its records-office sibling | each item's months; items unchanged |
| late-ferry-claim | the payout on one late ferry claim | added skill: rebate, cap, half for late filing, rounding down | 25 in the reply |

`courier-window-tickets`, `archive-retention-records`,
`ferry-reimbursement-claims`, `lab-holding-periods` and `late-ferry-claim`
exist only because of the added skills in `subject_patch.json`.

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-skills`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
