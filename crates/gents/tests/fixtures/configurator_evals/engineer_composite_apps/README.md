# Engineer composite apps

End-to-end multi-subsystem apps combining schemas, workspace imports, fulltext BM25 search,
datastore-scoped least-privilege agents, exact-boundary filtered triggers, grouped quorum
and two-agent handoff chains, in-place app evolution, packaged app packs, and linted desktop
UI pages across varied `@gents/ux-sdk` patterns (`Stat`, `Card`, `Table`, `LineChart`,
`BarChart`, `DonutChart`, `Grid`, `Tabs`, `Select`, `Switch`, `Input`, `Textarea`, `Badge`,
`Dialog`, `EmptyState`, `toast`, themed CSS `var(--...)`, `useQuery`, `useWrite`).

Each case builds a working composite app and exercises it in later stages (seeded trigger
fires and boundary skips, two-agent chains, BM25 queries, datastore-scope enforcement,
live mutations, or in-place schema and UI evolution).

- `train` (8 cases): `fleet-service-desk`, `greenhouse-climate-board`, `studio-gear-checkout`,
  `lab-sample-pipeline`, `kb-triage-desk`, `library-desk-pack`, `market-vendor-board`,
  `clinic-intake-triage`
- `validation` (3 cases): `dock-cargo-control`, `patent-review-desk`, `bakery-batch-pack`
- `held_out` (3 cases): `solar-grid-dispatch`, `museum-loan-vault`, `orchard-harvest-pack`
