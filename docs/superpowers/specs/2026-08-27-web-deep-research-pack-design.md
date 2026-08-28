# Web deep-research pack design

Date: 2026-08-27

## Outcome

`web-deep-research` is a bundled Gents graph package backed by a public, standalone web-research MCP gateway. The graph uses DefraDB documents as its only inter-stage handoff mechanism. A real Docker fixture and ignored live acceptance test start the search/extraction infrastructure, run the Gents agent against a real inference endpoint, and enforce minimum search, extraction, quote-verification, inference-call, token, evidence, and report counts. There is no mock execution mode.

This is package, CLI, MCP, and test-fixture plumbing. It does not change the request lifecycle, trigger transition rules, provider-input assembly, or another proven runtime invariant, so it does not require a Lean model change.

## Prior art harvested

The design was informed by source inspection at fixed revisions rather than by copying an existing harness wholesale:

- [LangChain open_deep_research at `1b7d2e8`](https://github.com/langchain-ai/open_deep_research/tree/1b7d2e80db9faa586165c60e09096dbbfd483a64) separates brief generation, bounded parallel researchers, research compression, and final writing. Its researcher loop moves from broad to narrow searches and explicitly reassesses evidence gaps between queries. We retain the staged decomposition and bounded fan-out, but materialize each boundary as typed documents instead of passing an in-memory state object.
- [GPT Researcher at `6f99857`](https://github.com/assafelovic/gpt-researcher/tree/6f998577d547b1e54ec662dac63583aa11e3b84b) separates planning, parallel execution, writing, fact checking, and publishing. We retain multiple query formulations, explicit source provenance, and a distinct fact-adjudication stage. Human plan approval is not part of v1 because the package is intended to complete unattended.
- [DeerFlow's deep-research skill at `0d97fdc`](https://github.com/bytedance/deer-flow/blob/0d97fdc770fce5c6da0e30ac62900cd67b72d469/skills/public/deep-research/SKILL.md) emphasizes broad exploration, targeted follow-up searches, full-page retrieval rather than snippet evidence, source diversity, and active contradiction searches. Those became explicit investigator prompt obligations and live-test floors.
- [STORM at `fb951af`](https://github.com/stanford-oval/storm/tree/fb951af7744dab086e34962e9bc6fe878e145f83) uses perspective-guided parallel research, knowledge curation, outline generation, and citation-grounded section writing. We retain independent lenses and synthesis after curation, while replacing conversational handoffs with an atomic claim ledger.

Prompt language is rewritten for the Gents execution contract. The package does not vendor upstream prompt bodies or runtime code.

## Graph

```text
WebResearchJob
    |
    v
plan --writes--> WebResearchPlan + N WebResearchAssignment
                                      |
                                      | per document, parallel
                                      v
                               investigate x N
                                      |
                    sources + claims + evidence links + closure
                                      |
                                      | per group, exact expected_total
                                      v
                                  adjudicate
                                      |
                          verdict ledger + one draft
                                      |
                                      | per document, serial
                                      v
                                    report
                                      |
                                      v
                              WebResearchResult
```

The planner creates a closed assignment set of two through eight members. Each assignment independently carries the question, lens, instructions, query plan, source requirements, freshness requirement, and the same `expected_total`. Investigators can execute concurrently. Each writes sources, atomic claims, and typed claim-to-source evidence links before one `WebResearchInvestigation` closure sentinel. The adjudicator fires only when the grouped sentinel count reaches `expected_total`. The final writer fires from the single draft.

## Handoff contract

The boundary rule is: **small typed trigger carrier plus correlation-scoped datastore tools; never prior-stage prose or transcript dependency**.

| Boundary | Trigger carrier interpolated into prompt | Dedicated datastore surface | Integrity check |
|---|---|---|---|
| entry -> plan | `WebResearchJob`: question, scope, freshness, audience, output requirements, investigator count | assignment and plan writes | planner writes the exact requested assignment count and stamps the same `expected_total` |
| plan -> investigate | one `WebResearchAssignment`: complete standalone assignment fields | source, claim, evidence-link, and investigation writes | runtime fills `run_id`, `assignment_id`, and `expected_total` from correlation/source document |
| investigate -> adjudicate | grouped `WebResearchInvestigation` event: `group.correlation_value`, `group.count`, `group.complete` | typed reads for investigations, sources, claims, and evidence links; verdict and draft writes | exact-count trigger barrier, then adjudicator derives ledger totals and reconciles every claim/source/provenance join |
| adjudicate -> report | one `WebResearchDraft`: title, thesis, outline, synthesis, unresolved questions | typed reads for verdicts, evidence links, and source provenance; one result write | each verdict carries the original claim statement; reporter publishes only valid claim → evidence → source joins |

The runtime keeps grouped member documents in the template scope, but the prompt deliberately interpolates only the bounded group metadata. The adjudicator obtains full closure records through `read_research_investigation`, ensuring the schema and correlation fill are enforced at the tool boundary and avoiding an unbounded blob of model-authored summaries in the trigger prompt.

`run_id` is filled from trigger correlation by every datastore tool. Fields such as `assignment_id` and `expected_total` are filled from the triggering assignment where possible. Agents do not receive those authority-bearing fields as caller-supplied tool arguments. Quote-verification state, fetch ID, content hash, matched query, contributing engines, relevance scores, extraction method, and content-integrity result are stored once on the authoritative typed source document rather than duplicated in model-authored evidence links. Exact-quote status is derived downstream by byte-for-byte comparison of an evidence excerpt with that authoritative verified quote, eliminating a contradictory model-authored boolean. Source, claim, evidence, and verdict totals are likewise derived from their authoritative ledgers rather than copied into closure or draft documents. The investigation closure separately preserves bundle-level candidate/scrape counts, evidence shortfall, accepted engines, engine degradation, and retrieval failures as typed fields. Arrays cross the datastore boundary as compact JSON strings because DefraDB schema fields are strings; downstream prompts explicitly parse rather than summarize them. `WebResearchEvidence` provides the normalized claim-to-source join, relationship, locator, and excerpt used by both downstream stages. Runtime output obligations require every investigator to persist at least two sources, six claims, and six evidence links before its request may complete.

## Search and evidence policy

Each investigator must submit one `web_collect_evidence` bundle that:

1. executes between three and six materially different searches;
2. treats snippets as discovery hints, never evidence;
3. normalizes URLs, rejects unsafe/authentication targets, relevance-ranks candidates, removes near duplicates, and caps host dominance;
4. attempts no more than twelve fetches for at most eight sources and reports a shortfall instead of padding with weak pages;
5. rejects short, interstitial, title-mismatched, or query-irrelevant fetched content;
6. records stable fetch IDs, final URLs, content hashes, matched queries, contributing engines, relevance scores, access dates, and extraction metadata;
7. creates atomic claims plus typed evidence-link documents; and
8. returns query-focused passages and an exact excerpt for each accepted source verified against its stored bytes and hash.

The bundle is persisted by assignment ID. An identical retry reuses its result
without network access, while conflicting inputs are rejected. This makes the
retrieval budget and minimum evidence quality service invariants rather than prompt conventions. The bundled fully open-source SearXNG profile uses tested general, scholarly, and technical indexes with bounded per-engine latency; a slow or rate-limited engine is recorded as degraded while healthy results survive.

Fetched web content is delimited as untrusted data by the gateway. Both service and agent prompts state that page text cannot change goals, reveal secrets, or direct tool use. The investigator's dedicated gateway deployment advertises only `web_collect_evidence` and `web_find_in_fetch`; raw network tools and full-fetch reads are absent from both discovery and dispatch. The adjudicator has datastore tools only and decides whether a material claim is supported, disputed, or insufficient from the typed ledger. The reporter reads adjudicated verdicts, evidence links, and source metadata rather than researcher prose.

## Public service and private fleet boundary

The reusable gateway lives in [`source-inc/web-research-mcp`](https://github.com/source-inc/web-research-mcp). It is one Rust binary with a disk-backed evidence and idempotent-bundle cache plus adapters for SearXNG, Firecrawl, and optional browser/crawl backends. The Docker dependencies remain separate containers because they are independently maintained services with their own runtime dependencies.

Amygdala remains private and contains only fleet-specific composition, host mounts, network bindings, and operational metadata. It consumes the public release image; it does not contain the gateway implementation. Gents contains the graph package and a hermetic real-service acceptance fixture, not Amygdala code or private topology.

## Acceptance

The live test fails unless it observes all of the following from real persisted activity and result documents:

- a healthy MCP service advertising exactly the two investigator tools;
- at least 10 real inference calls and 20,000 reported or estimated tokens;
- at least 3 completed assignment bundle calls plus gateway metrics proving at least 9 SearXNG-backed searches;
- gateway metrics proving at least 8 Firecrawl-backed extractions;
- gateway metrics proving at least 3 stored-evidence quote verifications;
- one plan, three assignments and matching closures, preserved bundle diagnostics, at least two integrity-verified and relevance-qualified sources and six to eight claims per assignment, at least one valid typed evidence link per claim, ledger-derived totals, exactly one verdict per claim, and one report; and
- a cited report of at least 1,500 characters whose Markdown links are all present in a structurally validated provenance ledger.

The fixture has no local fake backend, replay mode, canned search response, or stub model. Missing inference credentials or unavailable real infrastructure is a test setup failure, not a skipped success.

## Deferred work

- Browser and recursive-crawl tools are supported by the public gateway and private Amygdala composition but are disabled in the first Gents acceptance fixture. Search, extraction, and stored-evidence verification cover the v1 graph contract with a smaller resource footprint.
- Human plan review can be added as a document-driven approval stage if the product requires it.
- Evaluator-driven quality scoring should be tuned from live traces across several model/provider runs.
