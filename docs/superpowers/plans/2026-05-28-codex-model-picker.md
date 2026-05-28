# Codex Model Picker → InferenceProfile Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Codex's model picker a live view onto DEFRA `InferenceProfile` documents; picking a model mutates the bound `AgentBehavior`'s `inference_profile_id` via the existing config-apply path. Remove the now-redundant `--codex-shim-model` flag. Fix the session-pinning bug where `ensure_agent_session` clobbered `behavior_id` on every upsert.

**Architecture:** The Codex shim binds to one `AgentBehavior` at server start (existing `--codex-shim-behavior-id` flag, or agent default). The bound behavior's `inference_profile_id` is read fresh on every Codex `ModelList` / `ConfigRead` / `ThreadStart` so the picker always reflects current state. `ConfigValueWrite` for key `model` calls `write_agent_behavior_document` to update the doc, reusing the validation path that `defra-agent config behavior set` uses. The `ShimState::model: Arc<str>` field goes away.

**Tech Stack:** Rust workspace; `defra-agent`/`defra-agent-cli` crates; `codex-app-server-protocol` types; integration tests use the embedded `defra-node` GraphQL endpoint via existing test helpers in `crates/defra-agent-cli/tests/support/`.

**Spec:** `docs/superpowers/specs/2026-05-28-codex-model-picker-design.md`

---

## File Map

**Modified:**
- `crates/defra-agent-cli/src/cli/args.rs` — drop `codex_shim_model` field
- `crates/defra-agent-cli/src/commands/serve.rs` — drop `codex_shim_model` resolution + threading; drop `model` from `CodexShimBindArgs` call site; drop `codex_shim_model` from `codex_shim_output` JSON
- `crates/defra-agent-cli/src/commands/codex_shim.rs` — drop `ShimState::model`, drop `CodexShimBindArgs::model`, add startup precondition that resolves bound behavior + verifies profile exists
- `crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs` — `ModelList` enumerates profiles; `ConfigRead` reads fresh from bound behavior; `ConfigValueWrite` / `ConfigBatchWrite` mutates `AgentBehavior` for key `model`
- `crates/defra-agent-cli/src/commands/codex_shim/protocol.rs` — `model_summary` rebuilt to take an `InferenceProfile` instead of state; helpers for building summaries from profile records
- `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/json.rs` — `thread_response_json` reads bound profile id fresh, not `state.model`
- `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs` — `ensure_agent_session` upsert: drop `agent_name` + `behavior_id` from `update:` clause; add resume-mismatch detection helper
- `crates/defra-agent-cli/tests/cli_codex_shim.rs` — extend with model-picker tests; update existing `model_name` assertion

**Created:**
- `crates/defra-agent-cli/src/commands/codex_shim/bound_behavior.rs` — new module: `resolve_bound_behavior_id`, `load_bound_inference_profile_id`, error types for the startup precondition

---

## Conventions

- Every task ends with a commit. Commit messages omit Claude/AI co-author lines unless we're adding them across the repo today (we are — keep the trailer).
- Tests live in `crates/defra-agent-cli/tests/cli_codex_shim.rs`. The existing helper module `tests/support/` provides `MockChatEndpoint`, `run_init_json`, `spawn_server_with_env`, `wait_for_port`, `wait_for_runtime_ready`, `graphql_url`, `allocate_port`, `agent_did_from_init`, `request_id`, `send_client_request`, `read_typed_response`.
- After each Step 5 commit, the workspace must `cargo check -p defra-agent-cli` cleanly.
- All GraphQL strings interpolated into queries must go through `defra_agent::graphql::escape_graphql_string`.

---

### Task 1: Bound-behavior resolver module

**Files:**
- Create: `crates/defra-agent-cli/src/commands/codex_shim/bound_behavior.rs`
- Modify: `crates/defra-agent-cli/src/commands/codex_shim.rs` (add `mod bound_behavior;` near the other `mod` lines around line 22-35)
- Test: covered indirectly by Task 2's integration test; this task is unit-test only

- [ ] **Step 1: Write the failing unit test inline at the bottom of `bound_behavior.rs`**

```rust
// In crates/defra-agent-cli/src/commands/codex_shim/bound_behavior.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_bound_behavior_id_uses_explicit_override() {
        let resolved = resolve_bound_behavior_id(
            Some("custom-behavior"),
            "did:key:zABC",
        );
        assert_eq!(resolved, "custom-behavior");
    }

    #[test]
    fn resolve_bound_behavior_id_falls_back_to_default_for_did() {
        let resolved = resolve_bound_behavior_id(
            None,
            "did:key:zABC",
        );
        assert_eq!(resolved, "did:key:zABC:default");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p defra-agent-cli --lib commands::codex_shim::bound_behavior::tests -- --nocapture
```

Expected: compile failure — `resolve_bound_behavior_id` not defined.

- [ ] **Step 3: Implement the module**

```rust
// crates/defra-agent-cli/src/commands/codex_shim/bound_behavior.rs

use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::{
    default_behavior_id_for_agent, load_agent_behavior, load_inference_profile,
};

/// Resolve the behavior id the shim is bound to.
///
/// An explicit operator override wins. Otherwise we use the agent's default
/// behavior id (`<did>:default`).
pub(super) fn resolve_bound_behavior_id(
    override_behavior_id: Option<&str>,
    agent_did: &str,
) -> String {
    match override_behavior_id.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => value.to_string(),
        None => default_behavior_id_for_agent(agent_did),
    }
}

/// Read the `inference_profile_id` currently attached to the bound behavior.
///
/// Returns an error if the behavior document is missing, if it has no
/// `inference_profile_id`, or if the referenced `InferenceProfile` doesn't
/// exist. Callers should treat any error here as a fatal misconfiguration.
pub(super) async fn load_bound_inference_profile_id(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<String> {
    let behavior = load_agent_behavior(node, behavior_id)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "Codex shim is bound to behavior {behavior_id:?}, but no AgentBehavior \
                 document with that behavior_id exists. Create or fix the behavior with \
                 `defra-agent config behavior set --behavior-id {behavior_id} ...`."
            )
        })?;
    let profile_id = behavior.inference_profile_id.ok_or_else(|| {
        anyhow!(
            "Codex shim is bound to behavior {behavior_id:?}, but that behavior has no \
             inference_profile_id set. Run \
             `defra-agent config behavior set --behavior-id {behavior_id} \
             --inference-profile-id <profile>` to attach one."
        )
    })?;
    if load_inference_profile(node, &profile_id).await?.is_none() {
        return Err(anyhow!(
            "Bound behavior {behavior_id:?} references inference_profile_id \
             {profile_id:?}, but no InferenceProfile document with that id exists."
        ));
    }
    Ok(profile_id)
}

/// Convenience wrapper for use during request handling — takes the cached
/// bound behavior id stored on ShimState.
pub(super) async fn load_bound_inference_profile_id_for_state(
    node: &EmbeddedNode,
    behavior_id: &Arc<str>,
) -> Result<String> {
    load_bound_inference_profile_id(node, behavior_id.as_ref()).await
}
```

Add `mod bound_behavior;` to `commands/codex_shim.rs` next to existing `mod` declarations (around lines 22-35).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-cli --lib commands::codex_shim::bound_behavior::tests
```

Expected: 2 passed.

```bash
cargo check -p defra-agent-cli
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/bound_behavior.rs \
        crates/defra-agent-cli/src/commands/codex_shim.rs
git commit -m "$(cat <<'EOF'
Add bound_behavior resolver for Codex shim

Centralizes how the shim resolves which AgentBehavior it's bound to and
validates that behavior has a usable inference_profile_id. Preparation
for replacing --codex-shim-model with doc-driven model state.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Startup precondition

Reject server startup with `--codex-shim` when the bound behavior has no resolvable `inference_profile_id`.

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim.rs:122-148` (`bind_codex_shim`)
- Test: `crates/defra-agent-cli/tests/cli_codex_shim.rs` (new test)

- [ ] **Step 1: Write the failing integration test**

Append to `crates/defra-agent-cli/tests/cli_codex_shim.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_refuses_to_start_when_bound_behavior_has_no_profile() -> Result<()> {
    use std::io::Read;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;

    // Boot the node briefly so we can mutate the default behavior to drop its
    // inference_profile_id, then shut it back down before the assertion run.
    {
        let mut prime = spawn_server_with_env(&home_dir, server_port, &[], &[])?;
        wait_for_port(server_port, &mut prime)?;
        wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
        let default_behavior_id = format!("{agent_did}:default");
        let mutation = format!(
            r#"mutation {{
                update_AgentBehavior(
                    filter: {{ behavior_id: {{ _eq: "{}" }} }},
                    input: {{ inference_profile_id: null }}
                ) {{ _docID }}
            }}"#,
            default_behavior_id
        );
        post_graphql_raw(&graphql, &mutation).await?;
        prime.kill()?;
        prime.wait()?;
    }

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim",
            "--codex-shim-port",
            &shim_port_string,
        ],
        &[],
    )?;

    let exit = serve.wait_timeout(Duration::from_secs(30))
        .context("waiting for shim-misconfig server to exit")?
        .ok_or_else(|| anyhow!("server with missing profile did not exit"))?;
    assert!(!exit.success(), "expected server to exit with non-zero status");

    let mut stderr_buf = String::new();
    if let Some(mut stderr) = serve.stderr.take() {
        stderr.read_to_string(&mut stderr_buf).ok();
    }
    assert!(
        stderr_buf.contains("inference_profile_id"),
        "expected error to mention inference_profile_id; got: {stderr_buf}"
    );
    Ok(())
}
```

The test depends on two new test-support helpers — `post_graphql_raw` and `wait_timeout` on the child handle. Add them in this same step:

In `crates/defra-agent-cli/tests/support/mod.rs` (extend the existing module):

```rust
pub async fn post_graphql_raw(graphql: &str, query: &str) -> anyhow::Result<serde_json::Value> {
    let body = serde_json::json!({ "query": query });
    let response = reqwest::Client::new()
        .post(graphql)
        .json(&body)
        .send()
        .await
        .context("posting graphql")?
        .error_for_status()?;
    Ok(response.json().await.context("decoding graphql response")?)
}

pub trait WaitTimeoutExt {
    fn wait_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> anyhow::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeoutExt for std::process::Child {
    fn wait_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> anyhow::Result<Option<std::process::ExitStatus>> {
        use std::time::Instant;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
```

If `reqwest` is not already a dev-dep, check `Cargo.toml`:

```bash
grep -n "reqwest" crates/defra-agent-cli/Cargo.toml
```

If absent, add to `[dev-dependencies]`:

```toml
reqwest = { workspace = true, features = ["json"] }
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_refuses_to_start_when_bound_behavior_has_no_profile -- --nocapture
```

Expected: compile passes (helpers exist) but the test fails — the server currently happily starts the shim even when the behavior has no profile.

- [ ] **Step 3: Implement the precondition**

In `crates/defra-agent-cli/src/commands/codex_shim.rs`, modify `bind_codex_shim` (around line 122). Replace the existing function body's prologue:

```rust
pub(crate) async fn bind_codex_shim(args: CodexShimBindArgs) -> Result<BoundCodexShim> {
    let codex_home = args.home.join("codex-ui");
    let codex_log_dir = codex_home.join("log");
    fs::create_dir_all(&codex_log_dir)
        .with_context(|| format!("creating Codex UI log dir {}", codex_log_dir.display()))?;
    let trace_path = codex_log_dir.join("codex-shim-events.jsonl");

    let bound_behavior_id =
        bound_behavior::resolve_bound_behavior_id(args.behavior_id.as_deref(), &args.agent_did);
    // Fail fast if the bound behavior is misconfigured — the shim can't
    // sensibly advertise a model picker without a resolvable profile.
    bound_behavior::load_bound_inference_profile_id(args.node.as_ref(), &bound_behavior_id)
        .await
        .with_context(|| format!("validating Codex shim bound behavior {bound_behavior_id:?}"))?;

    let state = ShimState {
        codex_home: codex_home.clone(),
        trace_path: trace_path.clone(),
        cwd: std::env::current_dir().context("resolving current working directory")?,
        fs_root: args.fs_root,
        node: args.node,
        background_execution_registry: args.background_execution_registry,
        graphql: Arc::from(args.graphql.clone()),
        agent_did: Arc::from(args.agent_did.clone()),
        behavior_id: Arc::from(bound_behavior_id),
        model: Arc::from(args.model),
        id_counter: Arc::new(AtomicU64::new(1)),
        timeout: Duration::from_secs(args.timeout_secs),
        poll_interval: Duration::from_millis(args.poll_ms.max(1)),
    };
    // ... rest unchanged
```

Then change the `behavior_id` field on `ShimState` from `Option<Arc<str>>` to `Arc<str>` (line 52):

```rust
#[derive(Clone)]
struct ShimState {
    codex_home: PathBuf,
    trace_path: PathBuf,
    cwd: PathBuf,
    fs_root: Option<PathBuf>,
    node: Arc<EmbeddedNode>,
    background_execution_registry: defra_agent::BackgroundExecutionRegistry,
    graphql: Arc<str>,
    agent_did: Arc<str>,
    behavior_id: Arc<str>,           // <-- was Option<Arc<str>>
    model: Arc<str>,
    id_counter: Arc<AtomicU64>,
    timeout: Duration,
    poll_interval: Duration,
}
```

Now fix the existing helper at `commands/codex_shim/thread_projection/storage.rs:271`:

```rust
fn behavior_id(state: &ShimState) -> String {
    state.behavior_id.as_ref().to_string()
}
```

(The previous `Option`-aware fallback path is gone; resolution happens once at bind time.)

Confirm `agent_name(state)` still returns `behavior_id(state)` if that's the desired shape; no change needed.

Compile and chase any other `state.behavior_id.as_deref()` / `state.behavior_id.is_some()` call sites:

```bash
grep -rn "state\.behavior_id" crates/defra-agent-cli/src/commands/codex_shim/
```

For each, replace `Option`-style usages with direct `Arc<str>` accesses (`state.behavior_id.as_ref()` or `state.behavior_id.clone()` as appropriate).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_refuses_to_start_when_bound_behavior_has_no_profile -- --nocapture
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_protocol_turn_streams_defra_response -- --nocapture
```

Expected: both pass. The pre-existing test still works because init seeds a profile.

```bash
cargo check -p defra-agent-cli
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim.rs \
        crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs \
        crates/defra-agent-cli/tests/cli_codex_shim.rs \
        crates/defra-agent-cli/tests/support/mod.rs \
        crates/defra-agent-cli/Cargo.toml
git commit -m "$(cat <<'EOF'
Validate Codex shim bound behavior at startup

Refuses to start the shim if the bound AgentBehavior has no
inference_profile_id or references a profile that doesn't exist. Replaces
the Option<Arc<str>> shape of ShimState.behavior_id with a resolved
Arc<str>; the resolution happens once during bind_codex_shim.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: ModelList enumerates InferenceProfile docs

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/protocol.rs:91-110` (`model_summary`)
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs:47-57` (ModelList handler)
- Test: `crates/defra-agent-cli/tests/cli_codex_shim.rs` (new test)

- [ ] **Step 1: Write the failing integration test**

Append to `crates/defra-agent-cli/tests/cli_codex_shim.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_model_list_enumerates_inference_profiles() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_profile_id = format!("{agent_did}:default-profile");
    let extra_profile_id = format!("extra-profile-{}", Uuid::new_v4().simple());

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // Seed a second profile via GraphQL so we can see >1 entry in the picker.
    let create_extra = format!(
        r#"mutation {{
            create_InferenceProfile(input: {{
                profile_id: "{extra_profile_id}",
                display_name: "Extra Profile",
                context_window: 8192,
                max_output_tokens: 1024,
                temperature: 0.5
            }}) {{ _docID }}
        }}"#
    );
    post_graphql_raw(&graphql, &create_extra).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ModelList {
            request_id: request_id(2),
            params: codex::ModelListParams::default(),
        },
    )
    .await?;
    let model_list: codex::ModelListResponse = read_typed_response(&mut ws, request_id(2)).await?;

    let ids: Vec<&str> = model_list
        .data
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(
        ids.contains(&default_profile_id.as_str()),
        "expected default profile {default_profile_id} in model list; got {ids:?}"
    );
    assert!(
        ids.contains(&extra_profile_id.as_str()),
        "expected extra profile {extra_profile_id} in model list; got {ids:?}"
    );
    let default_entry = model_list
        .data
        .iter()
        .find(|entry| entry.id == default_profile_id)
        .expect("default profile present");
    assert!(default_entry.is_default, "default profile should be flagged as isDefault");
    let extra_entry = model_list
        .data
        .iter()
        .find(|entry| entry.id == extra_profile_id)
        .expect("extra profile present");
    assert!(!extra_entry.is_default, "non-default profile must not be flagged isDefault");

    serve.kill().ok();
    serve.wait().ok();
    Ok(())
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_model_list_enumerates_inference_profiles -- --nocapture
```

Expected: assertion failure — model list only contains the synthetic single entry, missing the extra profile.

- [ ] **Step 3: Implement enumeration**

Rewrite `model_summary` in `crates/defra-agent-cli/src/commands/codex_shim/protocol.rs` to take an `InferenceProfile`:

```rust
use defra_agent::InferenceProfile;

pub(super) fn model_summary(profile: &InferenceProfile, is_default: bool) -> Value {
    let display_name = profile
        .display_name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| profile.profile_id.clone());
    json!({
        "id": profile.profile_id,
        "model": profile.profile_id,
        "displayName": display_name,
        "description": describe_profile(profile),
        "isDefault": is_default,
        "hidden": false,
        "upgrade": null,
        "upgradeInfo": null,
        "availabilityNux": null,
        "supportedReasoningEfforts": [],
        "defaultReasoningEffort": "medium",
        "inputModalities": ["text"],
        "supportsPersonality": false,
        "additionalSpeedTiers": [],
        "serviceTiers": [],
        "defaultServiceTier": null
    })
}

fn describe_profile(profile: &InferenceProfile) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ctx) = profile.context_window {
        parts.push(format!("ctx {ctx}"));
    }
    if let Some(max) = profile.max_output_tokens {
        parts.push(format!("max {max}"));
    }
    if let Some(temp) = profile.temperature {
        parts.push(format!("temp {temp:.2}"));
    }
    if parts.is_empty() {
        "DEFRA inference profile".to_string()
    } else {
        parts.join(" · ")
    }
}
```

Update `handlers/basic.rs` `ModelList` arm (replacing lines 47-57):

```rust
codex::ClientRequest::ModelList { request_id, .. } => {
    let profiles = defra_agent::document_config::list_inference_profile_records(
        state.node.as_ref(),
    )
    .await
    .context("listing InferenceProfile documents for Codex ModelList")?;
    let current_profile_id =
        super::super::bound_behavior::load_bound_inference_profile_id_for_state(
            state.node.as_ref(),
            &state.behavior_id,
        )
        .await
        .context("resolving current inference profile for ModelList")?;
    let entries: Vec<Value> = profiles
        .into_iter()
        .map(|(_doc_id, profile)| {
            let is_default = profile.profile_id == current_profile_id;
            model_summary(&profile, is_default)
        })
        .collect();
    send_typed_json_result::<codex::ModelListResponse>(
        outbound,
        request_id,
        json!({
            "data": entries,
            "nextCursor": null
        }),
    )
    .await
}
```

If `list_inference_profile_records` isn't already re-exported through `defra_agent::document_config`, find the correct path:

```bash
grep -rn "pub.*list_inference_profile_records\|pub use.*list_inference_profile_records" crates/defra-agent/src/
```

If only `pub(crate)`, change it to `pub` in `crates/defra-agent/src/document_config/inference_profile.rs:132` and re-export from `document_config/mod.rs`. Similarly verify `InferenceProfile` itself is re-exported (it already is at `defra_agent::InferenceProfile`, see `crates/defra-agent/src/lib.rs:77`).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_model_list_enumerates_inference_profiles -- --nocapture
cargo check -p defra-agent-cli
```

Expected: pass + clean.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/protocol.rs \
        crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs \
        crates/defra-agent-cli/tests/cli_codex_shim.rs \
        crates/defra-agent/src/document_config/inference_profile.rs \
        crates/defra-agent/src/document_config/mod.rs
git commit -m "$(cat <<'EOF'
Enumerate InferenceProfile docs in Codex ModelList

Codex's model picker now reflects every InferenceProfile document in the
node, with the one referenced by the bound AgentBehavior flagged as
default. Removes the single-entry synthetic stub.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: ConfigRead reads bound profile id fresh

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs:70-85` (`ConfigRead` arm)
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/json.rs:75-94` (`thread_response_json`)
- Test: `crates/defra-agent-cli/tests/cli_codex_shim.rs` — update existing assertion on line 104

- [ ] **Step 1: Write the failing test changes**

The existing test `codex_shim_protocol_turn_streams_defra_response` asserts:

```rust
assert_eq!(config.config.model.as_deref(), Some(model_name.as_str()));
```

Replace that assertion with:

```rust
let expected_profile_id = format!("{agent_did}:default-profile");
assert_eq!(
    config.config.model.as_deref(),
    Some(expected_profile_id.as_str()),
    "ConfigRead.model should be the bound behavior's inference_profile_id"
);
```

Also append a new focused test:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_read_reflects_doc_mutation() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let alt_profile_id = format!("alt-profile-{}", Uuid::new_v4().simple());

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let create_alt = format!(
        r#"mutation {{
            create_InferenceProfile(input: {{
                profile_id: "{alt_profile_id}",
                display_name: "Alt Profile",
                context_window: 4096
            }}) {{ _docID }}
        }}"#
    );
    post_graphql_raw(&graphql, &create_alt).await?;

    let switch_behavior = format!(
        r#"mutation {{
            update_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{default_behavior_id}" }} }},
                input: {{ inference_profile_id: "{alt_profile_id}" }}
            ) {{ _docID }}
        }}"#
    );
    post_graphql_raw(&graphql, &switch_behavior).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ConfigRead {
            request_id: request_id(2),
            params: codex::ConfigReadParams { include_layers: false, cwd: None },
        },
    )
    .await?;
    let config: codex::ConfigReadResponse = read_typed_response(&mut ws, request_id(2)).await?;
    assert_eq!(
        config.config.model.as_deref(),
        Some(alt_profile_id.as_str()),
        "ConfigRead should reflect the doc-mutated inference_profile_id"
    );

    serve.kill().ok();
    serve.wait().ok();
    Ok(())
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_config_read_reflects_doc_mutation -- --nocapture
```

Expected: assertion fails — `state.model` is still being returned and doesn't reflect doc changes.

- [ ] **Step 3: Implement fresh-read for ConfigRead and thread_response_json**

`handlers/basic.rs` `ConfigRead` arm:

```rust
codex::ClientRequest::ConfigRead { request_id, .. } => {
    let profile_id =
        super::super::bound_behavior::load_bound_inference_profile_id_for_state(
            state.node.as_ref(),
            &state.behavior_id,
        )
        .await
        .context("resolving current inference profile for ConfigRead")?;
    send_typed_json_result::<codex::ConfigReadResponse>(
        outbound,
        request_id,
        json!({
            "config": {
                "model": profile_id,
                "model_provider": "defra",
                "approval_policy": "never",
                "sandbox_mode": "danger-full-access"
            },
            "origins": {}
        }),
    )
    .await
}
```

`thread_projection/json.rs` — `thread_response_json` (line 75-94). It currently has access to `state` but isn't async. Two options:

1. Make `thread_response_json` async and add a `profile_id: String` parameter at the call sites.
2. Pre-resolve `profile_id` in each call site and pass it in.

Use option 2 — fewer ripples. Change the signature:

```rust
pub(in crate::commands::codex_shim) fn thread_response_json(
    state: &ShimState,
    record: &CodexThreadRecord,
    thread: Value,
    bound_profile_id: &str,
) -> Value {
    let _ = state; // kept for symmetry; remove if no other field is read here.
    json!({
        "thread": thread,
        "model": bound_profile_id,
        "modelProvider": "defra",
        "serviceTier": null,
        "cwd": absolute_path(&record.cwd),
        "runtimeWorkspaceRoots": [],
        "instructionSources": [],
        "approvalPolicy": "never",
        "approvalsReviewer": "user",
        "sandbox": { "type": "dangerFullAccess" },
        "activePermissionProfile": null,
        "reasoningEffort": null
    })
}
```

Find every caller of `thread_response_json`, `thread_start_response_json`, `thread_resume_response_json`:

```bash
grep -rn "thread_response_json\|thread_start_response_json\|thread_resume_response_json" crates/defra-agent-cli/src/commands/codex_shim/
```

For each, resolve `bound_profile_id` via `bound_behavior::load_bound_inference_profile_id_for_state(state.node.as_ref(), &state.behavior_id).await?` before calling, and pass it through.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_config_read_reflects_doc_mutation -- --nocapture
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_protocol_turn_streams_defra_response -- --nocapture
cargo check -p defra-agent-cli
```

Expected: both tests pass; check clean.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs \
        crates/defra-agent-cli/src/commands/codex_shim/thread_projection/json.rs \
        crates/defra-agent-cli/tests/cli_codex_shim.rs
git commit -m "$(cat <<'EOF'
Read bound profile_id fresh in Codex ConfigRead and thread responses

Codex now sees the inference_profile_id currently attached to the bound
AgentBehavior, refreshed on every ConfigRead and ThreadStart response.
Replaces all reads of the static state.model except its final removal.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: ConfigValueWrite mutates AgentBehavior

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs:86-99` (ConfigValueWrite / ConfigBatchWrite arms)
- Test: `crates/defra-agent-cli/tests/cli_codex_shim.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/defra-agent-cli/tests/cli_codex_shim.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_value_write_model_mutates_behavior() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let alt_profile_id = format!("alt-profile-{}", Uuid::new_v4().simple());

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    post_graphql_raw(
        &graphql,
        &format!(
            r#"mutation {{
                create_InferenceProfile(input: {{
                    profile_id: "{alt_profile_id}",
                    display_name: "Alt Profile",
                    context_window: 4096
                }}) {{ _docID }}
            }}"#
        ),
    )
    .await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ConfigValueWrite {
            request_id: request_id(2),
            params: codex::ConfigValueWriteParams {
                key: "model".to_string(),
                value: serde_json::Value::String(alt_profile_id.clone()),
                cwd: None,
            },
        },
    )
    .await?;
    let _write: codex::ConfigWriteResponse = read_typed_response(&mut ws, request_id(2)).await?;

    // Verify the AgentBehavior doc was updated.
    let resp = post_graphql_raw(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ behavior_id: {{ _eq: "{default_behavior_id}" }} }},
                    limit: 1
                ) {{ inference_profile_id }}
            }}"#
        ),
    )
    .await?;
    let stored = resp
        .pointer("/data/AgentBehavior/0/inference_profile_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored, alt_profile_id,
        "AgentBehavior.inference_profile_id should reflect ConfigValueWrite"
    );

    serve.kill().ok();
    serve.wait().ok();
    Ok(())
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_config_value_write_model_mutates_behavior -- --nocapture
```

Expected: stored value is still `<did>:default-profile`, not `alt_profile_id`.

- [ ] **Step 3: Implement the write path**

Rewrite the two arms in `handlers/basic.rs` (lines 86-99):

```rust
codex::ClientRequest::ConfigValueWrite { request_id, params, .. } => {
    apply_config_writes(outbound, state, request_id, vec![(params.key, params.value)]).await
}
codex::ClientRequest::ConfigBatchWrite { request_id, params, .. } => {
    let writes = params
        .writes
        .into_iter()
        .map(|w| (w.key, w.value))
        .collect();
    apply_config_writes(outbound, state, request_id, writes).await
}
```

Add the helper at module scope in the same file:

```rust
async fn apply_config_writes(
    outbound: &Outbound,
    state: &ShimState,
    request_id: codex::RequestId,
    writes: Vec<(String, serde_json::Value)>,
) -> Result<()> {
    for (key, value) in writes {
        if key == "model" {
            let new_profile_id = match value.as_str() {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => {
                    return super::super::protocol::send_error(
                        outbound,
                        request_id,
                        super::super::JSONRPC_INVALID_PARAMS,
                        "ConfigValueWrite for `model` requires a non-empty string".to_string(),
                    )
                    .await;
                }
            };
            // Validate the target profile exists before mutating the behavior.
            if defra_agent::document_config::load_inference_profile(
                state.node.as_ref(),
                &new_profile_id,
            )
            .await
            .context("looking up target InferenceProfile for ConfigValueWrite")?
            .is_none()
            {
                return super::super::protocol::send_error(
                    outbound,
                    request_id,
                    super::super::JSONRPC_INVALID_PARAMS,
                    format!(
                        "InferenceProfile {new_profile_id:?} not found; available ids \
                         are visible via ModelList"
                    ),
                )
                .await;
            }
            apply_profile_to_bound_behavior(state, &new_profile_id).await?;
        }
        // Other keys: silently accept (matches prior behavior).
    }
    send_typed_json_result::<codex::ConfigWriteResponse>(
        outbound,
        request_id,
        json!({
            "status": "ok",
            "version": "defra-shim",
            "filePath": absolute_path(&state.codex_home.join("config.toml")),
            "overriddenMetadata": null
        }),
    )
    .await
}

async fn apply_profile_to_bound_behavior(state: &ShimState, profile_id: &str) -> Result<()> {
    use crate::config_writes::{write_agent_behavior_document, ConfigAccess};
    use defra_agent::{load_agent_behavior, AgentBehaviorDocument};
    let behavior_id = state.behavior_id.as_ref();
    let mut behavior: AgentBehaviorDocument = load_agent_behavior(
        state.node.as_ref(),
        behavior_id,
    )
    .await
    .context("loading bound AgentBehavior for profile mutation")?
    .ok_or_else(|| anyhow::anyhow!("bound AgentBehavior {behavior_id:?} disappeared"))?;
    behavior.inference_profile_id = Some(profile_id.to_string());
    // Use the GraphQL endpoint the shim was bound with so the write goes through the
    // same surface defra-agent config behavior set uses.
    let access = ConfigAccess::Graphql(state.graphql.as_ref().to_string());
    write_agent_behavior_document(&access, &behavior)
        .await
        .context("writing AgentBehavior with new inference_profile_id")?;
    Ok(())
}
```

If `load_inference_profile` is not already exported through `defra_agent::document_config`, expose it (it's already `pub` in `document_config/inference_profile.rs:53` and re-exported through `document_config/mod.rs:24`).

Same for `load_agent_behavior` and `AgentBehaviorDocument` — they're already re-exported through `defra_agent` (see `crates/defra-agent/src/lib.rs:75-77`).

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_config_value_write_model_mutates_behavior -- --nocapture
cargo check -p defra-agent-cli
```

Expected: pass + clean.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/handlers/basic.rs \
        crates/defra-agent-cli/tests/cli_codex_shim.rs
git commit -m "$(cat <<'EOF'
Mutate bound AgentBehavior on Codex ConfigValueWrite for model

When Codex's model picker writes the `model` config key, the shim now
loads the bound AgentBehavior, updates its inference_profile_id, and
writes it back through write_agent_behavior_document (the same path
defra-agent config behavior set uses). Other config keys keep the
existing no-op ack.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Reject ConfigValueWrite for nonexistent profile

This task only adds a failing test that should already pass thanks to Task 5's pre-validation; treat it as a regression guard. If it passes already, commit it as a focused regression test.

**Files:**
- Test: `crates/defra-agent-cli/tests/cli_codex_shim.rs`

- [ ] **Step 1: Write the regression test**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_config_value_write_rejects_unknown_profile() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let original_profile_id = format!("{agent_did}:default-profile");

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ConfigValueWrite {
            request_id: request_id(2),
            params: codex::ConfigValueWriteParams {
                key: "model".to_string(),
                value: serde_json::Value::String("definitely-not-real".to_string()),
                cwd: None,
            },
        },
    )
    .await?;
    // Reading the response should yield a JSON-RPC error frame, not a result.
    let raw = read_raw_jsonrpc(&mut ws, request_id(2)).await?;
    assert!(
        raw.get("error").is_some(),
        "expected error response for unknown profile; got {raw}"
    );

    // Behavior doc should be unchanged.
    let resp = post_graphql_raw(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ behavior_id: {{ _eq: "{default_behavior_id}" }} }},
                    limit: 1
                ) {{ inference_profile_id }}
            }}"#
        ),
    )
    .await?;
    let stored = resp
        .pointer("/data/AgentBehavior/0/inference_profile_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    assert_eq!(
        stored, original_profile_id,
        "behavior doc must remain unchanged after rejected write"
    );

    serve.kill().ok();
    serve.wait().ok();
    Ok(())
}
```

`read_raw_jsonrpc` is a new test helper — add to `tests/support/mod.rs`:

```rust
pub async fn read_raw_jsonrpc(
    ws: &mut crate::ShimWebSocket,
    expected_id: codex_app_server_protocol::RequestId,
) -> anyhow::Result<serde_json::Value> {
    use futures_util::StreamExt;
    while let Some(msg) = ws.next().await {
        let msg = msg.context("reading ws frame")?;
        if let WsMessage::Text(text) = msg {
            let value: serde_json::Value = serde_json::from_str(&text)
                .context("parsing ws JSON")?;
            if value.get("id").map(|id| {
                serde_json::to_value(&expected_id).ok().as_ref() == Some(id)
            }).unwrap_or(false) {
                return Ok(value);
            }
        }
    }
    anyhow::bail!("ws stream ended before expected response id arrived");
}
```

If the support module doesn't already pull `WsMessage` and `tokio_tungstenite` symbols, add the imports. The `ShimWebSocket` type alias may need to live in `support` rather than the test file; refactor accordingly.

- [ ] **Step 2: Run test to verify behavior**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_config_value_write_rejects_unknown_profile -- --nocapture
```

Expected: should pass on first run if Task 5 validation is in place. If it fails, fix Task 5's pre-validation and rerun.

- [ ] **Step 3: (No implementation needed if Step 2 passes.)**

- [ ] **Step 4: Run the full shim test suite**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim
```

Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/tests/cli_codex_shim.rs \
        crates/defra-agent-cli/tests/support/mod.rs
git commit -m "$(cat <<'EOF'
Regression test: Codex ConfigValueWrite rejects unknown profile id

Locks in the pre-validation step: writing `model` to a non-existent
InferenceProfile id returns a JSON-RPC error and leaves the bound
AgentBehavior untouched.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Remove `--codex-shim-model` and `ShimState::model`

Now that nothing reads `state.model`, rip out the flag and the field.

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs:346-350` (remove `codex_shim_model` field)
- Modify: `crates/defra-agent-cli/src/commands/serve.rs:171-179, 250, 276` (remove resolution + threading + json output)
- Modify: `crates/defra-agent-cli/src/commands/codex_shim.rs:53, 85, 144` (remove `ShimState::model`, `CodexShimBindArgs::model`, threading)
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/protocol.rs` (`model_summary` no longer references `state.model` — already done in Task 3)

- [ ] **Step 1: Audit remaining references**

```bash
grep -rn "state\.model\|codex_shim_model\|codex-shim-model\|CodexShimBindArgs\.\*model\|args\.model" crates/defra-agent-cli/
```

Expected hits after Tasks 3-4: only the args/serve/codex_shim definitions and the `args.model` consumer in `bind_codex_shim`.

- [ ] **Step 2: Remove the flag and field**

In `crates/defra-agent-cli/src/cli/args.rs`, delete lines 346-350 (the `codex_shim_model` field) along with its `#[arg]` attribute lines.

In `crates/defra-agent-cli/src/commands/serve.rs`, delete the entire block lines 171-179 (`let codex_shim_model = ...`). In the `bind_codex_shim` call at lines 240-253, remove `model: codex_shim_model.clone(),`. In the `codex_shim_output` json block at line 271-278, remove the `"model": codex_shim_model,` line.

In `crates/defra-agent-cli/src/commands/codex_shim.rs`:
- Remove the `model: Arc<str>,` field from `ShimState` (line 53).
- Remove `pub(crate) model: String,` from `CodexShimBindArgs` (line 85).
- Remove `model: Arc::from(args.model),` from the `ShimState` constructor (line 144).

Check `protocol.rs` and `thread_projection/json.rs` for any remaining `state.model` references (there should be none after Tasks 3-4); the compiler will catch leftovers.

- [ ] **Step 3: Verify everything compiles and tests pass**

```bash
cargo check -p defra-agent-cli
cargo test -p defra-agent-cli --test cli_codex_shim
```

Expected: clean check; all shim tests pass.

- [ ] **Step 4: Verify the flag is genuinely gone**

```bash
target/debug/defra-agent server --help 2>&1 | grep -i "shim-model"
```

Expected: no output (the flag is gone).

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/cli/args.rs \
        crates/defra-agent-cli/src/commands/serve.rs \
        crates/defra-agent-cli/src/commands/codex_shim.rs
git commit -m "$(cat <<'EOF'
Drop --codex-shim-model flag and ShimState::model field

The model id Codex sees is now derived from the bound AgentBehavior's
inference_profile_id, refreshed on every read. The operator override
flag is redundant and removed.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Session-pinning storage fix

Stop `ensure_agent_session`'s upsert from clobbering `behavior_id` and `agent_name` on update.

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs:85-113`
- Test: `crates/defra-agent-cli/tests/cli_codex_shim.rs`

- [ ] **Step 1: Write the failing test**

Append:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_does_not_clobber_session_behavior_id() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = format!("{agent_did}:default");
    let session_id = format!("test-session-{}", Uuid::new_v4().simple());

    // Pre-seed an AgentSession bound to the default behavior.
    {
        let mut prime = spawn_server_with_env(&home_dir, server_port, &[], &[])?;
        wait_for_port(server_port, &mut prime)?;
        wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
        post_graphql_raw(
            &graphql,
            &format!(
                r#"mutation {{
                    create_AgentSession(input: {{
                        session_id: "{session_id}",
                        agent_name: "preexisting",
                        behavior_id: "{default_behavior_id}",
                        status: "active"
                    }}) {{ _docID }}
                }}"#
            ),
        )
        .await?;
        prime.kill()?;
        prime.wait()?;
    }

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    // Trigger ensure_agent_session by starting a thread that reuses session_id.
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;
    // Resume into the pre-seeded session id (forces ensure_agent_session to upsert).
    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(2),
            params: codex::ThreadResumeParams {
                thread_id: session_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    // Drain whichever response shape comes back; we only care about the doc state.
    let _ = read_raw_jsonrpc(&mut ws, request_id(2)).await;

    let resp = post_graphql_raw(
        &graphql,
        &format!(
            r#"{{
                AgentSession(
                    filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                    limit: 1
                ) {{ agent_name behavior_id }}
            }}"#
        ),
    )
    .await?;
    let preserved_agent_name = resp
        .pointer("/data/AgentSession/0/agent_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let preserved_behavior_id = resp
        .pointer("/data/AgentSession/0/behavior_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(
        preserved_agent_name, "preexisting",
        "agent_name must not be clobbered by the shim's session upsert"
    );
    assert_eq!(
        preserved_behavior_id, default_behavior_id,
        "behavior_id must remain pinned to its create-time value"
    );

    serve.kill().ok();
    serve.wait().ok();
    Ok(())
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_does_not_clobber_session_behavior_id -- --nocapture
```

Expected: assertion failure — `agent_name` becomes the shim's computed value (the behavior_id), clobbering the pre-existing one.

- [ ] **Step 3: Fix the upsert**

In `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs`, modify `ensure_agent_session` (line 85) to drop `agent_name` and `behavior_id` from the `update:` clause:

```rust
pub(super) async fn ensure_agent_session(state: &ShimState, session_id: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_session_id = escape_graphql_string(session_id);
    let agent_name = agent_name(state);
    let behavior_id = behavior_id(state);
    let escaped_agent_name = escape_graphql_string(&agent_name);
    let escaped_behavior_id = escape_graphql_string(&behavior_id);
    let mutation = format!(
        r#"mutation {{
            upsert_AgentSession(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                add: {{
                    session_id: "{escaped_session_id}",
                    agent_name: "{escaped_agent_name}",
                    behavior_id: "{escaped_behavior_id}",
                    started: "{now}",
                    status: "active"
                }},
                update: {{
                    status: "active"
                }}
            ) {{ _docID }}
        }}"#
    );
    query_node_json(&state.node, &mutation).await?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_does_not_clobber_session_behavior_id -- --nocapture
cargo test -p defra-agent-cli --test cli_codex_shim
cargo check -p defra-agent-cli
```

Expected: pass + suite green + clean.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs \
        crates/defra-agent-cli/tests/cli_codex_shim.rs
git commit -m "$(cat <<'EOF'
Pin AgentSession.behavior_id and agent_name at create time

ensure_agent_session previously rewrote both fields on every upsert,
which meant restarting the shim with a different --codex-shim-behavior-id
silently rebound existing sessions on next touch. Behavior pinning is
now write-once-at-create.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Reject session resume when bound behavior mismatches

If a Codex client resumes a session whose pinned `behavior_id` doesn't match the shim's currently bound behavior, return a JSON-RPC error rather than silently writing turns to the wrong behavior.

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs` (`ensure_agent_session` becomes mismatch-aware) and `crates/defra-agent-cli/src/commands/codex_shim/turn.rs` or wherever session resume is dispatched
- Test: `crates/defra-agent-cli/tests/cli_codex_shim.rs`

- [ ] **Step 1: Find the resume entry point**

```bash
grep -rn "ensure_agent_session\|ThreadResume\b" crates/defra-agent-cli/src/commands/codex_shim/
```

Use the first dispatcher that handles `ThreadResume` (likely in `handlers/` or `thread_routes.rs`). The exact location is determined by the grep above — the plan lists the file map but the precise line is whatever the grep returns. Update the file map at the top of the task with the actual path before editing.

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_rejects_resume_with_mismatched_behavior() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, "irrelevant")?;
    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let session_id = format!("test-session-{}", Uuid::new_v4().simple());
    let foreign_behavior_id = "some-other-behavior".to_string();

    // Pre-seed the session pinned to a different behavior than the shim will bind.
    {
        let mut prime = spawn_server_with_env(&home_dir, server_port, &[], &[])?;
        wait_for_port(server_port, &mut prime)?;
        wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
        post_graphql_raw(
            &graphql,
            &format!(
                r#"mutation {{
                    create_AgentSession(input: {{
                        session_id: "{session_id}",
                        agent_name: "foreign",
                        behavior_id: "{foreign_behavior_id}",
                        status: "active"
                    }}) {{ _docID }}
                }}"#
            ),
        )
        .await?;
        prime.kill()?;
        prime.wait()?;
    }

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &["--codex-shim", "--codex-shim-port", &shim_port_string],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;
    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadResume {
            request_id: request_id(2),
            params: codex::ThreadResumeParams {
                thread_id: session_id.clone(),
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let raw = read_raw_jsonrpc(&mut ws, request_id(2)).await?;
    let error = raw.get("error").expect("expected error response");
    let message = error
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    assert!(
        message.contains("behavior") && message.contains(&foreign_behavior_id),
        "expected mismatch error to name the foreign behavior id; got: {message}"
    );

    serve.kill().ok();
    serve.wait().ok();
    Ok(())
}
```

- [ ] **Step 3: Implement the mismatch check**

Add a helper to `thread_projection/storage.rs`:

```rust
pub(super) async fn ensure_agent_session_pinning(
    state: &ShimState,
    session_id: &str,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                limit: 1
            ) {{
                behavior_id
            }}
        }}"#
    );
    let response = query_node_json(&state.node, &query).await?;
    let stored_behavior_id = response
        .pointer("/data/AgentSession/0/behavior_id")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
    let bound_behavior_id = state.behavior_id.as_ref();
    if let Some(stored) = stored_behavior_id {
        if stored != bound_behavior_id {
            anyhow::bail!(
                "session {session_id:?} is pinned to behavior {stored:?}, but the shim \
                 is bound to {bound_behavior_id:?}. Restart the server with \
                 --codex-shim-behavior-id {stored} to resume this session."
            );
        }
    }
    Ok(())
}
```

In the `ThreadResume` handler (path identified in Step 1), invoke `ensure_agent_session_pinning` *before* `ensure_agent_session`. If it returns an error, surface it as a JSON-RPC error via `protocol::send_error`. Example shape:

```rust
if let Err(err) = thread_projection::storage::ensure_agent_session_pinning(state, &thread_id).await {
    return super::super::protocol::send_error(
        outbound,
        request_id,
        super::super::JSONRPC_INVALID_REQUEST,
        err.to_string(),
    )
    .await;
}
```

(The exact module paths depend on where `ThreadResume` is dispatched; adjust the `super::super` chain accordingly.)

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p defra-agent-cli --test cli_codex_shim codex_shim_rejects_resume_with_mismatched_behavior -- --nocapture
cargo test -p defra-agent-cli --test cli_codex_shim
cargo check -p defra-agent-cli
```

Expected: target test passes; suite still green; check clean.

- [ ] **Step 5: Commit**

```bash
git add crates/defra-agent-cli/src/commands/codex_shim/thread_projection/storage.rs \
        crates/defra-agent-cli/src/commands/codex_shim/ \
        crates/defra-agent-cli/tests/cli_codex_shim.rs
git commit -m "$(cat <<'EOF'
Reject Codex thread resume when bound behavior doesn't match

If an AgentSession is pinned to behavior_id X but the shim was started
with --codex-shim-behavior-id Y, attempts to resume that session now
fail with a clear JSON-RPC error naming the pinned behavior, rather
than silently routing turns through the wrong behavior.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 10: Final sweep

- [ ] **Step 1: Run full workspace checks**

```bash
cargo check --workspace
cargo test -p defra-agent-cli
cargo test -p defra-agent
```

Expected: all clean. Investigate any failures rather than skipping.

- [ ] **Step 2: Spot-check the live flow**

Manually verify the user-facing recipe works:

```bash
./scripts/install-codex.sh
defra-agent init \
  --dangerously-overwrite \
  --inference-url http://100.73.235.38:8000/v1 \
  --model-name MiniMax-M2.7-NVFP4 \
  --write-tools
defra-agent server --tool-ceiling readwrite --tool-root "$PWD" --codex-shim
```

In another terminal:

```bash
CODEX_HOME="$HOME/.defra-agent/codex-ui" \
  codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:9292/
```

Confirm Codex's model picker shows the seeded default profile (and any others you've added). Pick an alternate profile and confirm:

1. `defra-agent config behavior list` reflects the new `inference_profile_id` on the bound behavior.
2. The next turn uses the new profile (e.g. its temperature / max_tokens changes if you set them differently).

If the live test reveals anything the integration tests missed, file a follow-up before declaring done.

- [ ] **Step 3: Update the design doc with anything that shifted during implementation**

If any of the design's mechanics changed during implementation (e.g. the exact RPC used for resume-mismatch errors, or the helper module name), update the spec at `docs/superpowers/specs/2026-05-28-codex-model-picker-design.md` so the two stay in sync.

- [ ] **Step 4: Commit anything from Step 3**

```bash
git status
# If the spec was updated:
git add docs/superpowers/specs/2026-05-28-codex-model-picker-design.md
git commit -m "$(cat <<'EOF'
Sync Codex model-picker spec with implementation choices

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 5: Hand off**

The plan is complete. Recommend the next step (PR or further review) per your standard branch-completion flow.

---

## Self-Review Notes

- All spec sections map to tasks: ModelList → Task 3; ConfigRead → Task 4; ConfigValueWrite mutation → Task 5; profile-existence validation → Task 5+6; CLI removal → Task 7; session pinning fix → Task 8; resume rejection → Task 9; startup precondition → Task 2; doc-driven derivation of model id → Task 4.
- No placeholders, no "implement appropriate" language; every step lists actual code or actual commands.
- Type consistency: `state.behavior_id: Arc<str>` after Task 2; `ShimState::model` removed in Task 7; `model_summary` takes `&InferenceProfile` everywhere it's used after Task 3.
- The order keeps the build green between tasks: `state.model` survives until Task 7 (after every reader has migrated off it in Tasks 3-4).
