# engineer_mailbox

## Configuration

Use the bundled pack_config.json. The subject is the Engineer `gents init --engineer` makes at the commit under
test, every core pack installed as init installs them (`scripts/evals/suites.py`). Mailbox
items are read as the operator (the evaluator), their only reader, so every
case grades what the recipient sees: how many items there are, their titles,
notes, choices, kind, action and status. The Engineer cannot read them back,
so a repeated turn is graded on the items the operator sees. The surface files
one notice per request, and a question is its own item. No check names a tool;
a case that says not to file grades that no item appeared. A run an automation
started files for the Node's owner, which in a trial is the evaluator, so a
record the evaluator seeds reaches the operator's inbox only through the
automation the Engineer built.

## Cases

| Case | Split | User need | Features exercised | Graded by |
| --- | --- | --- | --- | --- |
| door-interlock-reminder | train | a reminder, then a second one later | flag items with exact title and note across turns | one item, then two, with their titles and notes, open, ack |
| clinic-seating-shortage | train | find short wings from records and flag them | queries over two collections, one flag | one item titled Clinic seating with "Amber (+7), Blue (+1)"; the reply names both wings |
| dock-slot-question | train | ask me to pick between the two lowest bids | ask items with options from records | one ask item with exactly Ostend Lines and Harlow Freight as choices |
| invoice-chat-only | train | answer in chat, not in the inbox | not filing when told not to | no item; the reply lists ["INV-301", "INV-304"] |
| freezer-breach-sustained | train | flag a breach, and don't re-flag it when it persists | one flag, nothing new on a later turn | one item "F2 (-12)" after both turns |
| blocked-order-escalation | train | do the work that needs no sign-off, escalate the rest | files plus one flag | invoices for the four approved orders with their totals, none for A-111 and A-114, orders unchanged; one item "A-111, A-114" |
| kiln-flag-and-question | train | a flag and an optional question in one turn | flag and ask kinds, optional questions | two items: the K3 flag with its note, and the glaze question with both choices and optional set |
| big-order-inbox-note | train | a note in my inbox for each new order over $500, none for smaller ones | an automation whose run files for the Node's owner | the $120 order the user adds gets no item; the seeded $870 order ORD-1044 gets exactly one item for the operator naming it |
| cooler-boundary-silent | validation | flag only if a reading left an inclusive range | staying silent at a boundary | no item |
| boiler-new-breach | validation | flag a breach, then only the newly breaching boiler | a second flag on a later turn | "B2 (13 bar)" then a second item "B3 (14 bar)" |
| tank-two-low-readings | validation | flag only after two low readings in a row, once | state kept across turns however the Engineer keeps it | no item after 41 and 27; one item after 26; still one after 24 |
| binder-next-shift | held_out | put the unresolved handoff from a notes file in my inbox | a flag from workspace notes | one item titled Pier binder M7 with its handoff note |
| ferry-crew-swap-question | held_out | ask me about the swap waiting on me | an ask item from workspace notes | one ask item titled with the heading and the two candidate covers; file unchanged |

## Usage

Run it through scripts/evals with `run-ladder.sh engineer-mailbox`; held-out
cases run only with `GENTS_EVAL_SPLITS=held_out`.
