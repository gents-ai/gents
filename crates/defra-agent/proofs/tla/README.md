# TLA+ Specs

Distributed-system verification for cross-node properties. Sibling to `../Proofs/` (per-node Lean specs) under issue #155's cross-boundary verification strategy.

See `../README.md` for how this fits with the broader formal-verification model.

## Specs

- `ReversePairing` — control-plane convergence of subscription/replicator reverse-pairing between two peers. Spec design: `../../../../docs/superpowers/specs/2026-05-08-reverse-pairing-tla-design.md`.
- `Sanity` — toolchain smoke test; not a real model.

## One-time setup

```bash
./scripts/install-tools.sh
```

Downloads `tla2tools.jar` into `.tools/` (gitignored). Requires Java 11+ on `PATH`.

## Running a model-check

```bash
./scripts/run-tlc.sh MCReversePairing
```

The script runs TLC with parallel workers and writes state-graph artifacts to `states/` (gitignored).

## Bounded parameters

Current parameters in `MCReversePairing.cfg`:

- 2 nodes
- 2 collections
- <= 2 crashes per node

Edit the `CONSTANTS` block to change. Larger parameters increase state space exponentially; benchmark before raising them.

## Expected runtimes (2024 laptop)

- Safety check: < 5 minutes
- Liveness check (with `-deadlock` and fairness): < 15 minutes
