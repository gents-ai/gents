# engineer_datastore

## Configuration

Application datastore eval definition: the Engineer counts, pages, links, updates and deletes application records, and sets up what other agents may read and change. The subject is the Engineer `gents init --engineer` makes at the commit under test, every core pack installed as init installs them (`scripts/evals/suites.py`).

No stage asks the Engineer to set up its own access. Its query tool reads every application collection from the start. It holds no write grant, so the first stage of a case that changes records has it grant itself the collection; the grant applies from its next turn, so that stage settles and may send one "OK, go ahead." turn before it is graded.

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| shipment-count-page | a count and the second page of queued Oslo shipments | `query` count, filter, order, paging | the total and the five references in the reply; records unchanged |
| shipment-lifecycle | add, update and remove one shipment | self-granted `write`: create, update, delete | the stored record after each stage; the other shipment untouched |
| protected-and-wrong-scope | a new display name, a new field, then a value in it | `config`, `schema`, `write`, each for its own kind of change | the Engineer's display name, the collection's fields, the stored note |
| board-open-receiving-tickets | which tickets are open, from a change log | `query` across two collections | the count and the two ticket IDs in the reply; records unchanged |
| board-notice-linkage | link tickets to notices by ID, then look the links up | `query`, `write` create | the link records; the unlinked ticket and both lookups in the replies |
| yard-approval-holds | clear holds that meet three conditions | `query`, `write` update | each hold's stored state; the cleared holds in the reply |
| sample-bin-placement | place samples in the latest eligible bin | `query`, `write` create | the placement records; the unplaced samples in the reply |
| rental-location-cleanup | correct one location, then delete retired ones no open order uses | `write` update and guarded delete | the locations left; the removed ones in the reply |
| dispatch-active-route-assignment | create assignments only for active routes | `query`, `write` create | the assignment records; the unassigned orders in the reply |
| library-hold-edition-links | fill in missing hold editions from an import | `query`, `write` create | the link records; the open hold in the reply |
| orders-analyst-read-only | an agent that can read orders but not change them | agent creation, datastore read grant, delegation | the agent answers from the table; the order it is asked to change is unchanged |
| returns-clerk-scoped-write | an agent that writes one collection and only reads another | agent creation, write grant on one collection | its new return ticket is stored; the account it is asked to change is unchanged |
| private-notes-per-agent | two agents sharing one notes table, each seeing only its own notes | agent creation, a document policy on a new collection, per-record access | each agent reads its own note and never the other's; both notes in the reply |
| pricing-clerk-write-revoked | take an agent's write access away again | write grant, then its removal | its price change lands before the revocation and not after |
| contractor-invoice-access | an agent that may read two of three invoices, then one fewer | per-record read access granted and revoked | the agent reads INV-7 and INV-8 and never INV-9's memo; after INV-7 is updated and revoked, it never reads the new memo; INV-8 in the replies |
| ledger-auditor-read-only | an agent that can read the ledger but not change it | agent creation, datastore read grant, delegation | the agent answers from the table; the entry it is asked to change is unchanged |
| courier-north-parcels | an agent that may read only one route's parcels | per-record read access | the agent reads P-31 and P-32 and never P-33's address; both codes in the reply |

The access cases grade the access by using it: the stage after the grant has the Engineer ask the new agent to read, and to attempt a write the user ruled out. A completed request by an agent other than the Engineer shows the agent ran. The per-record cases start with no collection, so the Engineer creates it and may bind a document access policy; each grades what reached the restricted agent: its output (tool results and replies) holds a detail of each record it may read, and never a detail only a hidden record holds.

## Usage

Run with `scripts/evals/run-ladder.sh engineer-datastore`. Held-out cases require `GENTS_EVAL_SPLITS=held_out`.
