---
name: use-database
description: Load before designing an app's data. What DefraDB does for you, and how to shape collections.
---

# Use the database

DefraDB is what you build apps from. Reach for it before writing code. Most apps are collections, datastore tools and a page, with a plugin where rules must be exact.

## What it does for you

| You need | Use | `schema` help topic |
| --- | --- | --- |
| Find by key, no duplicates | Indexes, unique indexes | `indexes` |
| Linked records | Relationships | `relationships` |
| Search text | `@fulltext`, then `query search` | `indexes fulltext` |
| Find similar things | Embeddings and a vector index | `embeddings`, `indexes vector` |
| Computed results | Views | `views` |
| Rollups of time series | Downsampling | `collections downsample` |
| Defaults and validation | Field defaults and constraints | `fields defaults`, `fields constraints` |
| Values many writers change | Merge rules | `fields crdt` |
| Values that never change | `@immutable` | `fields immutable` |
| A record of every change | `@branchable` | `collections branchable` |
| Who can see what | `@policy` | `collections policy` |

A page's `useQuery` refreshes when documents change, so live pages need no extra work. A new document can start work through an event source (`automate`). Selected collections can sync to another node.

## Shape each collection

While you build you can empty or remove a collection (`schema` help `collections`), so try a shape with real documents and change it if it reads badly. Settle it before others depend on it.

- **A copy of something external** (a ticket): one document per thing, keyed by its external ID.
- **Something that happened** (a move, a payment): append-only. Derive current state from the log.
- **A setting one writer owns**: one document, updated in place.

If two writers can change one value and it isn't a merge-rule field, use a log. Concurrent updates to one field merge, and one is lost.

Use a stable key with a unique index, so a repeated write is harmless.

## Design for readers

List every reader first: each page section, trigger and agent.

- Index what you filter or sort on.
- A trigger's filter sees only the document that fired it. If one consumer should react, put the field that names it on the document.
- An agent's prompt is rendered from the firing document, so put the state it needs there.
- Queries return bounded pages. Plan for real row counts.

Derive what can be computed on read: an age, a count, an overdue flag. Store times as UTC `DateTime`. `@branchable` can't be changed later.

## Check it

Write two or three realistic documents, read them back with your page's query, and confirm filters and sort order.

Next: `connect-data` if the data comes from outside, otherwise `design-page`.
