# engineer_schema_experience

## Configuration

Engineer schema experience eval definition: create and additively evolve collections, then write and read their records. The subject is the Engineer `gents init --engineer` makes at the commit under test, every core pack installed as init installs them (`scripts/evals/suites.py`).

Every case that creates or changes a collection has a later stage that stores records in it and reads them back through the feature it set up. The Engineer holds no write grant at the start, so the first writing stage has it grant itself the collection; the grant applies from its next request, so that stage settles and may send one "OK, go ahead." turn before it is graded.

## Cases

| Case | User need | Tool features | Graded by |
| --- | --- | --- | --- |
| help-center-search | a help center searchable by word | unique index, English full-text indexes, ranked search | the schema; the three loaded articles; the best match's slug in the reply |
| projects-and-tasks | projects with tasks | two-way relationship, linked writes | the schema; each task linked to its project; the open task in the reply |
| stock-records | parts keyed by SKU | unique index, create and lookup | the schema; the two parts; the looked-up part in the reply |
| customer-timeline | one customer's events newest first | compound ordered index | the schema; the four events; the two latest in the reply |
| catalog-neighbors | nearest items to a vector | flat Euclidean vector index, similarity read | the schema; the four items; the nearest one in the reply |
| extend-customer-records | two new optional fields on live records | additive schema change, update | the schema; old values kept; the new values stored |
| editorial-relationships | writers, stories and headline search | relationship plus full-text index | the schema; each story linked to its writer; the matching story's writer in the reply |
| spanish-knowledge-base | a Spanish knowledge base searchable by word | Spanish full-text indexes, stemming | the schema; the loaded pages; the best match's code in the reply |
| guest-bookings | guests, bookings and arrivals in order | relationship, ordered index | the schema; the stored guests and bookings; arrivals in order in the reply |
| admiral-report-search | keyword search over paragraphs | English full-text index, ranked search | the schema; the five reports as written; a ranked search result naming the officer |
| board-category-migration | a new field filled from a map, reviewed values kept | additive change, backfill, a second run | the schema; each ticket's category; nothing changed by the second run |
| cooler-target-temperature | a new field filled by precedence, zero kept | additive change, backfill | the schema; each rack's target |
| crate-audit-discrepancy | a new computed field from the latest approved audit | additive change, backfill | the schema; each ticket's discrepancy |
| workshop-shift-priority | a new flag from a manual value or agreeing queue lines | additive change, backfill | the schema; each shift's priority |
| archive-return-seal | one new optional field, then a value in it | additive change, update | the schema; the seal on one checkout, the rest untouched |

## Usage

Run with `scripts/evals/run-ladder.sh schema-experience 3 16`. Held-out cases require `GENTS_EVAL_SPLITS=held_out`.
