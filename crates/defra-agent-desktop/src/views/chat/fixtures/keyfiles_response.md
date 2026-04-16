## Hex Rotor Rebuild

Synthetic response used to exercise the transcript renderer. This fixture exists to cover the prose-then-table-then-prose shape without pinning a real session transcript into the repo.

The rotor subsystem is split across a few modules. Key Files covers their roles, and Important Constraints lists the rules the scheduler has to respect when restarting a rotor.

### Key Files

| File | Role |
|------|------|
| `rotor/spin.rs` | Drives the core spin loop; owns the rate limiter |
| `rotor/plan.rs` | Builds the execution plan from the pending queue |
| `rotor/repair.rs` | Reconciles drift and re-emits missed ticks |
| `rotor/observe.rs` | Publishes rotor telemetry to subscribers |
| `rotor/tests/round_trip.rs` | End-to-end harness covering plan → spin → observe |

### Important Constraints

- Only one rotor may hold the lease for a given partition at a time → take the lease before any replay
- Replays must be idempotent; the observer will see each tick at most once, but possibly zero times under load
- A rotor that observes its own replay must short-circuit to avoid amplification
- `repair.rs` is the only component allowed to rewrite history; everything else treats the append log as immutable
