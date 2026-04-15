# defra-agent desktop local bootstrap plan

## Goal

Make the standard laptop demo path explicit and low-flag:

```bash
defra-agent init
defra-agent server
defra-agent-desktop init
defra-agent-desktop
```

The server path is IROH P2P. The desktop init path discovers the standard
local runtime, verifies it is usable, saves it as a desktop deployment, and
sets up the app to complete local pairing on launch.

## Principles

- IROH is the only supported transport for the desktop/local-agent path.
- The happy path should not require users to paste P2P flags.
- Init commands should be explicit about what they changed and what to run next.
- Runtime-owned state stays in `~/.defra-agent`; desktop-owned state stays in
  the desktop data directory.
- The first implementation uses the P2P HTTP endpoints directly for the
  desktop-to-runtime pairing calls. The app depends on `defradb.rs` after
  `sourcenetwork/defradb.rs#864`, so the local code uses the shared
  `P2POperations` surface for embedded-node calls.

## Task Breakdown

1. Standardize local server P2P defaults.
   - Make `defra-agent server` start with IROH enabled by default.
   - Default to localhost bind, ephemeral P2P port, relay disabled, and
     discovery disabled for local laptop safety.
   - Keep readiness JSON explicit: GraphQL URL, agent DID, P2P peer ID, and
     listen addresses.
   - Update CLI tests so the no-flag server path asserts IROH readiness.

2. Add `defra-agent-desktop init`.
   - Add a headless subcommand to the desktop binary.
   - Read the standard agent home, `init.json`, and `runtime.json`.
   - Verify the runtime GraphQL endpoint is reachable.
   - Require `p2p_transport = "iroh"` and at least one listen address.
   - Save a local deployment record into the desktop peer directory.
   - Print a human-readable summary of what was discovered and written.

3. Extend desktop peer metadata.
   - Add optional `source` and `graphql` fields to saved peer records.
   - Mark standard local discovery records as `source = "local-standard"`.
   - Preserve backward compatibility for existing `peers.json` files.

4. Complete local pairing on desktop app launch.
   - When a saved peer has a local runtime GraphQL endpoint, connect the runtime
     back to the current desktop IROH listen address.
   - Configure runtime-side collection subscriptions and runtime-to-desktop
     replicators for the protocol collections.
   - Keep this logic isolated behind a small local-runtime bootstrap helper.

5. Make inference and behavior editing demoable from the UI.
   - Ensure the standard initialized backend and behavior appear in Operator.
   - Support changing endpoint, model, profile, and behavior prompt from the UI.
   - Ensure new chat submissions use the edited runtime documents.

6. Seed optional behavior presets.
   - Add a small set of standard behaviors for demos, such as `General`,
     `Repo Reader`, and `Planner`.
   - Keep the default principal behavior stable.
   - Decide whether presets belong in `defra-agent init` or a later
     `config preset apply` command before broadening the surface.

7. Add end-to-end bootstrap coverage.
   - Exercise `defra-agent init -> defra-agent server -> defra-agent-desktop init`.
   - Use an explicit `--home`/`--agent-home` pair so the demo is easy to run in
     an isolated temp directory.
   - Default the live demo endpoint to `http://workstation-1:8000/v1` and the
     model to the endpoint-reported `MiniMax-M2.7-NVFP4`.
   - Launch a desktop client against the saved local peer.
   - Submit a request from the desktop path and assert the response is visible.

## First PR Slice

This PR should cover tasks 1-4 and the focused tests needed to prove the
standard local bootstrap path. It also adds the first demo e2e for
`defra-agent init --home -> defra-agent server --home -> defra-agent-desktop init --agent-home`
against the live workstation inference endpoint. Behavior presets and richer UI
editing coverage remain follow-up work.
