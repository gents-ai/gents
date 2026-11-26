# Engineer composite apps

End-to-end multi-subsystem apps combining schemas, ACP-scoped agents, filtered and
per-key automations, pack packaging, and linted desktop UI pages across varied
`@gents/ux-sdk` patterns (`Stat`, `Table`, `LineChart`, `BarChart`, `DonutChart`,
`Grid`, `Card`, `Tabs`, `Select`, `Input`, `Badge`, `Dialog`, `useQuery`, `useWrite`).

Each case builds a working composite app and exercises it in a later stage (seeded
trigger fires, ACP enforcement checks, or live data mutations).

- `train` (8 cases): `fleet-service-desk`, `greenhouse-climate-board`, `studio-gear-checkout`,
  `lab-sample-pipeline`, `invoice-approval-hub`, `library-desk-pack`, `market-vendor-board`,
  `clinic-intake-triage`
- `validation` (3 cases): `dock-cargo-control`, `patent-review-desk`, `bakery-batch-pack`
- `held_out` (3 cases): `solar-grid-dispatch`, `museum-loan-vault`, `orchard-harvest-pack`
