# Gents hard-cutover runbook

This is the sole operator-facing record of the breaking rename to Gents. The
cutover is intentionally one-way: install Gents, create fresh state and
identity, reapply configuration, and re-pair the fleet. There are no aliases,
automatic migrations, dual-read paths, or compatibility shims.

DefraDB and `defradb.rs` keep their names. Domain vocabulary such as agent,
principal, behavior, deployment, and request also remains unchanged.

The behavior-readiness cut is also fresh-state only. The canonical
`AgentRuntime` baseline no longer stores runnable/unavailable behavior counts;
`AgentBehaviorReadiness` is the sole durable authority. Any pre-cut v0.14
server database and desktop/phone state must be wiped and re-created together.
The runtime deliberately does not upgrade or dual-read the old AgentRuntime
version.

## Locked mapping

| Before the cutover | Gents |
| --- | --- |
| Repository `sourcenetwork/defra-agent` or `source-inc/defra-agent` | `source-inc/gents` |
| CLI binary and package `defra-agent` | `gents` and `gents-cli` |
| Native filesystem runner `defra-native-fs-runner` | `gents-fs-runner` |
| Runtime, protocol, schema, proof, lens, CLI, and desktop crate prefixes `defra-agent-*` | `gents-*` |
| Runtime home `~/.defra-agent` | `~/.gents` |
| Environment prefix `DEFRA_AGENT_*` | `GENTS_*` |
| Desktop app path `apps/desktop-tauri` | `apps/gents-desktop` |
| Desktop data root `<data_local>/defra-agent/desktop` | `<data_local>/gents/desktop` |
| Desktop environment override `DEFRA_AGENT_DESKTOP_HOME` | `GENTS_DESKTOP_HOME` |
| Desktop bundle identifier `com.sourcenetwork.defra-agent-desktop` | `com.source-inc.gents` |
| macOS identity service `defra-agent.identity` | `com.source-inc.gents.identity` |
| Signed CLI identifier `org.sourcenetwork.defra-agent` | `com.source-inc.gents.cli` |

The mapping is a documentation aid, not a runtime compatibility contract.
Gents does not inspect the former environment prefix, home, desktop state,
bundle identifier, or keychain service.

| Surface | Cutover policy |
| --- | --- |
| CLI | No legacy binary name, subcommand alias, or shim binary |
| Environment | No fallback reads of the former product prefix |
| Runtime home | No automatic discovery, copy, or migration |
| Identity/keychain | No dual-read service and no identity reuse |
| Desktop | No old bundle, data-root, preference, or peer-directory import |
| P2P peers | No pairing-row or invitation migration; enroll and pair the new DIDs |

## Hard-cutover rules

- Do not run the two product generations against the same home or database.
- Do not copy a private identity key or macOS keychain identity into Gents.
- Do not import a pre-cutover database into the new home.
- Do not add aliases, symlinks, fallback environment variables, shim binaries,
  or dual-read keychain behavior.
- Do not delete pre-cutover state automatically. Keep an offline snapshot until
  the new fleet is verified, then remove it deliberately under the applicable
  retention policy.
- Treat every Gents principal as a new principal with a new DID. Reissue grants,
  manifests, invitations, and peer relationships against that DID.
- Use only `source-inc/gents` as the canonical repository coordinate. Redirects
  are discovery aids, not configuration.

`--home <path>` remains an explicit operator escape hatch. You may point an
individual Gents command at a path you selected, including a copied directory
for best-effort inspection, but Gents does not promise to recognize or migrate
pre-cutover contents. Never test that path against the only copy of operator
state. The supported deployment path is a fresh `~/.gents`.

## Before the cutover window

Complete these once for the fleet:

- [ ] Record the final pre-cutover release, repository SHA, binary checksum,
  configuration export, and database snapshot.
- [ ] Record each host's supervisor unit, inference endpoint, model, secrets
  binding, listening ports, peer addresses, and current DID.
- [ ] Stop configuration changes and peer enrollment until the Gents fleet is
  healthy.
- [ ] Confirm the signed Gents artifact and checksum came from
  `source-inc/gents` and that `gents version` succeeds.
- [ ] Prepare a portable manifest root for each host. Keep secrets out of the
  manifest and record how each host receives them.
- [ ] Provision the GitHub Actions secret `GENTS_API_KEY` for live smoke. Do
  not retain `AGENT_DAEMON_API_KEY` as an alias.
- [ ] Decide the new fleet topology and which DIDs require network membership,
  conversation data-plane edges, or cross-deployment subagent permission.
- [ ] Schedule a quiet repository rename window and update local remotes only
  after the GitHub rename is complete.

Use this operator record during the window:

| Host | Pre-cutover DID | Gents DID | Manifest/config record | New peer ID/address | Verified by / time |
| --- | --- | --- | --- | --- | --- |
| `strangenas` |  |  |  |  |  |
| `workstation-1` |  |  |  |  |  |
| `spark-1` |  |  |  |  |  |
| `spark-2` |  |  |  |  |  |
| `studio-1` demo database |  |  |  |  |  |

Do not leave the DID columns implicit. The record is the handoff between fresh
identity creation and reissuing fleet trust.

## Common host procedure

The deployment supervisor and inference configuration differ by host, but the
order is fixed.

1. Stop the pre-cutover runtime and verify that no process or supervisor retry
   can write to its home or database.
2. Snapshot the old home and record its checksum/location. Do not move it into
   `~/.gents` and do not delete it during the cutover.
3. Install the signed `gents` binary and verify it:

   ```bash
   gents version
   codesign --verify --strict --verbose=2 "$(command -v gents)" # macOS
   ```

4. Initialize a fresh home with the host's selected backend:

   ```bash
   gents_home="$HOME/.gents"
   gents init --home "$gents_home" \
     --inference-url "$INFERENCE_URL" \
     --model-name "$MODEL_NAME"
   gents_did="$(jq -r .agent_did "$gents_home/init.json")"
   test -n "$gents_did" && test "$gents_did" != null
   ```

   These lowercase names are shell variables for the runbook, not Gents
   environment-variable aliases. Pass `--home` explicitly.

5. Rebind and apply the host's portable manifest to the fresh DID:

   ```bash
   gents config validate \
     --home "$gents_home" \
     --root "$MANIFEST_ROOT" \
     --bind-agent-did home \
     --force-rebind-concrete-did
   gents config diff \
     --home "$gents_home" \
     --root "$MANIFEST_ROOT" \
     --bind-agent-did home \
     --force-rebind-concrete-did
   gents config apply \
     --home "$gents_home" \
     --root "$MANIFEST_ROOT" \
     --bind-agent-did home \
     --force-rebind-concrete-did
   ```

   Review the diff before apply. Do not add `--prune` during initial
   provisioning.

6. Update the supervisor to execute `gents server --home "$HOME/.gents"` with
   only `GENTS_*` product variables. Keep provider-standard variables such as
   `OPENAI_API_KEY` unchanged.
7. Start Gents and wait for readiness. Record the new DID, peer ID, listen
   addresses, GraphQL endpoint, and binary version.
8. Recreate network membership and peer pairings using the new DIDs. A copied
   pairing row that names an old DID is not valid authority for the new
   principal.
9. Verify runtime, P2P, and a real request:

   ```bash
   gents status --home "$gents_home"
   gents p2p status --home "$gents_home"
   gents p2p pairings list --home "$gents_home" --output table
   gents request submit \
     --home "$gents_home" \
     --agent-did "$gents_did" \
     --content "Gents cutover health check"
   ```

10. Mark the host complete in the operator record only after the request reaches
    a terminal success and every required pairing reports subscribed and
    replicating.

See [operations.md](operations.md) for the signed-network and data-plane pairing
commands.

## Host checklists

### `strangenas`

- [ ] Stop and disable the pre-cutover runtime supervisor on `strangenas`.
- [ ] Snapshot its home/database and record the inference and storage mounts.
- [ ] Install Gents and initialize a fresh `~/.gents`.
- [ ] Capture the new `strangenas` DID and peer endpoint in the operator record.
- [ ] Rebind and apply the `strangenas` manifest/configuration.
- [ ] Reissue its network membership and re-pair every required storage/fleet
  peer against the new DID.
- [ ] Restart under the Gents supervisor and verify runtime, P2P, replication,
  and request health.

### `workstation-1`

- [ ] Stop and disable the pre-cutover runtime supervisor on `workstation-1`;
  leave its model server configuration unchanged.
- [ ] Snapshot its home/database and record the local model endpoint and model
  name.
- [ ] Install Gents and initialize a fresh `~/.gents` against that endpoint.
- [ ] Capture the new `workstation-1` DID and peer endpoint.
- [ ] Rebind and apply the `workstation-1` behavior, tool, and deployment
  configuration.
- [ ] Reissue membership and pairings for each orchestrator or worker that may
  route work to this host.
- [ ] Restart and verify backend, runtime, P2P, request, and delegated-work
  health.

### `spark-1`

- [ ] Stop and disable the pre-cutover runtime supervisor on `spark-1`.
- [ ] Snapshot its home/database and record its inference binding.
- [ ] Install Gents and initialize a fresh `~/.gents`.
- [ ] Capture the new `spark-1` DID and peer endpoint.
- [ ] Rebind and apply the `spark-1` manifest/configuration.
- [ ] Reissue membership and re-pair `spark-1` with its required fleet peers.
- [ ] Restart and verify runtime, P2P, replication, request, and worker-slot
  health.

### `spark-2`

- [ ] Stop and disable the pre-cutover runtime supervisor on `spark-2`.
- [ ] Snapshot its home/database and record its inference binding.
- [ ] Install Gents and initialize a fresh `~/.gents`.
- [ ] Capture the new `spark-2` DID and peer endpoint.
- [ ] Rebind and apply the `spark-2` manifest/configuration.
- [ ] Reissue membership and re-pair `spark-2` with its required fleet peers.
- [ ] Restart and verify runtime, P2P, replication, request, and worker-slot
  health.

### `studio-1` demo database

- [ ] Stop every demo/runtime process that can write the `studio-1` demo
  database.
- [ ] Snapshot and label the pre-cutover demo database for historical reference;
  do not import it into Gents.
- [ ] Install the signed Gents artifact and initialize a fresh `~/.gents` and
  fresh demo database.
- [ ] Capture the new `studio-1` demo DID and peer endpoint.
- [ ] Rebind and apply only the active demo manifest/configuration; do not copy
  historical request, response, session, or pairing rows.
- [ ] Recreate the demo network membership, invitations, data-plane pairings,
  and any cross-deployment subagent permission using the new DIDs.
- [ ] Restart the demo, open the Gents desktop app with fresh desktop state, and
  verify runtime, backend, P2P, replication, chat, and delegated-request health.

## Fleet verification and completion

After all five records are populated:

- [ ] Confirm no active supervisor, cron entry, workflow, shell profile, or
  deployment manifest invokes the former binary, home, environment prefix, or
  repository coordinate.
- [ ] Confirm all running binaries report the intended Gents version and SHA.
- [ ] Confirm every active principal DID equals the Gents DID in the operator
  record.
- [ ] Confirm signed network membership and pairing rows name only those new
  DIDs.
- [ ] Confirm required peer edges are connected, subscribed, and replicating.
- [ ] Submit and observe one local request per host and one cross-host delegated
  request for each routed worker role.
- [ ] Confirm the desktop app uses `com.source-inc.gents` and fresh
  `<data_local>/gents/desktop` state.
- [ ] Update clones, automation, package references, and links to
  `source-inc/gents`; do not configure redirects as canonical remotes.
- [ ] Record explicit follow-up issues for any Source-owned repository or
  automation reference that cannot be updated in the cutover window.

The cutover is complete when the Gents fleet passes these checks without any
compatibility path. Retain or delete pre-cutover snapshots according to the
operator's recovery and data-retention decision; Gents never removes them.
