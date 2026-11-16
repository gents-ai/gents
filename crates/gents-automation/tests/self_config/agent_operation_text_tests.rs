use super::*;
use gents_protocol::tool_names::CONFIG_TOOL_NAME;

/// Everything the model reads for `agent create` and `agent clone`: verb
/// help, receipts and failure envelopes, with run-specific identifiers named.
struct Transcript {
    text: String,
    names: Vec<(String, String)>,
}

impl Transcript {
    fn name(&mut self, value: &str, name: &str) {
        self.names.push((value.to_owned(), name.to_owned()));
    }

    async fn record(&mut self, tool: &dyn ToolDyn, label: &str, args: Value) -> Option<Value> {
        let (outcome, raw) = match tool
            .call(
                args.to_string(),
                &gents_loop::tool::ToolCallEnvelope::ambient(),
            )
            .await
        {
            Ok(text) => ("ok", text),
            Err(error) => ("refused", error.to_string()),
        };
        let parsed = serde_json::from_str::<Value>(&raw).ok();
        if outcome == "ok" {
            if let Some(agent) = parsed.as_ref().and_then(|value| value.get("agent")) {
                let index = self.names.len();
                for (field, kind) in [("agent_did", "CREATED"), ("creation_nonce", "NONCE")] {
                    if let Some(value) = agent.get(field).and_then(Value::as_str) {
                        self.name(value, &format!("<{kind}_{index}>"));
                    }
                }
            }
        }
        let mut text = raw;
        for (value, name) in &self.names {
            text = text.replace(value, name);
        }
        self.text
            .push_str(&format!("## {label} ({outcome})\n{text}\n"));
        parsed
    }
}

#[tokio::test]
async fn agent_create_and_clone_text_is_pinned() {
    let fixture = gents_automation::test_support::self_config_fixture("agent-operation-text").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    let root = fixture.admitted_workspace_root().await;
    let sibling = fixture.install_sibling("pinned-sibling").await;
    fixture.grant_sibling_configuration(&sibling).await;
    let tools = fixture.tools(&["node", "tools", "profile"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
        .as_ref();
    let profile = format!("{}:inference", fixture.agent_did());
    let mut transcript = Transcript {
        text: String::new(),
        names: Vec::new(),
    };
    transcript.name(&root, "<ROOT>");
    transcript.name(&sibling, "<SIBLING>");
    transcript.name(fixture.agent_did(), "<SELF>");
    transcript.name(fixture.node_did(), "<NODE>");

    for argv in [
        json!(["agent", "create", "--help"]),
        json!(["agent", "clone", "--help"]),
        json!(["agent", "preview", "--help"]),
    ] {
        transcript
            .record(tool, &format!("help {argv}"), json!({ "argv": argv }))
            .await;
    }
    let create = |options: Value| json!({"argv":["agent","create"],"options":options});
    let clone = |options: Value| json!({"argv":["agent","clone"],"options":options});
    transcript
        .record(
            tool,
            "preview create",
            json!({"argv":["agent","preview","create"],"options":{"display-name":"Planner","system-prompt":"Plan.","profile":profile}}),
        )
        .await;
    transcript
        .record(
            tool,
            "preview create rejected",
            json!({"argv":["agent","preview","create"],"options":{"display-name":"Planner","profile":profile}}),
        )
        .await;
    transcript
        .record(
            tool,
            "create without tools",
            create(json!({"display-name":"Planner","description":"Plans work","system-prompt":"Plan.","profile":profile})),
        )
        .await;
    transcript
        .record(
            tool,
            "create with host tools",
            create(json!({"display-name":"Scout","system-prompt":"Find.","preset":"readonly","root":root,"profile":profile})),
        )
        .await;
    transcript
        .record(
            tool,
            "clone",
            clone(json!({"from":sibling,"display-name":"Copy","profile":format!("{sibling}:inference")})),
        )
        .await;
    for (label, args) in [
        (
            "create without display-name",
            create(json!({"system-prompt":"Plan.","profile":profile})),
        ),
        (
            "create without system-prompt",
            create(json!({"display-name":"Planner","profile":profile})),
        ),
        (
            "create without profile",
            create(json!({"display-name":"Planner","system-prompt":"Plan."})),
        ),
        (
            "create with unpublished profile",
            create(json!({"display-name":"Planner","system-prompt":"Plan.","profile":"absent"})),
        ),
        (
            "create with unknown preset",
            create(
                json!({"display-name":"Planner","system-prompt":"Plan.","preset":"bogus","profile":profile}),
            ),
        ),
        (
            "create with unpublished root",
            create(
                json!({"display-name":"Planner","system-prompt":"Plan.","preset":"readonly","root":"/pinned/unpublished","profile":profile}),
            ),
        ),
        (
            "create with options.id",
            create(
                json!({"id":"x","display-name":"Planner","system-prompt":"Plan.","profile":profile}),
            ),
        ),
        (
            "create with unknown option",
            create(json!({"name":"Planner"})),
        ),
        (
            "create with a positional",
            json!({"argv":["agent","create","Planner"]}),
        ),
        (
            "create with set and cleared display_name",
            json!({"argv":["agent","create"],"options":{"display-name":"Planner"},"clear":["display_name"]}),
        ),
        (
            "create with target_id",
            json!({"argv":["agent","create"],"target_id":"planner","options":{"display-name":"Planner"}}),
        ),
        (
            "clone without from",
            clone(json!({"display-name":"Copy","profile":profile})),
        ),
        (
            "clone with preset",
            clone(
                json!({"from":sibling,"display-name":"Copy","preset":"readonly","profile":format!("{sibling}:inference")}),
            ),
        ),
        (
            "clone without profile",
            clone(json!({"from":sibling,"display-name":"Copy"})),
        ),
        (
            "clone from an unknown agent",
            clone(json!({"from":"did:key:absent","display-name":"Copy","profile":profile})),
        ),
        (
            "batch stopping at a refused create",
            json!({"argv":["batch"],"options":{"operations":[{"argv":["agent","create"],"options":{"display-name":"Planner","profile":profile}}]}}),
        ),
    ] {
        transcript.record(tool, label, args).await;
    }
    let chosen = transcript
        .record(
            tool,
            "create with --default",
            json!({"argv":["agent","create","--default"],"options":{"display-name":"Planner","system-prompt":"Plan.","profile":profile}}),
        )
        .await
        .and_then(|receipt| receipt["agent_did"].as_str().map(str::to_owned))
        .expect("the catalog holder chooses the default");
    transcript
        .record(
            tool,
            "disable the default agent",
            json!({"argv":["agent","disable"],"options":{"id":chosen}}),
        )
        .await;
    transcript
        .record(
            tool,
            "preview disabling the default agent",
            json!({"argv":["agent","preview","disable"],"options":{"id":chosen}}),
        )
        .await;
    transcript
        .record(
            tool,
            "disable the clone source",
            json!({"argv":["agent","disable"],"options":{"id":sibling}}),
        )
        .await;
    transcript
        .record(
            tool,
            "clone from a disabled agent",
            clone(json!({"from":sibling,"display-name":"Copy","profile":format!("{sibling}:inference")})),
        )
        .await;

    assert!(
        transcript.text == PINNED,
        "agent create/clone text changed\n<<<TRANSCRIPT\n{}TRANSCRIPT>>>",
        transcript.text
    );
}

const PINNED: &str = r###"## help ["agent","create","--help"] (ok)
agent [preview] create  options: display-name, system-prompt, preset (readonly|write), profile; readonly permits shell commands; to forbid shell set host.bash.mode Off; optional description, root; argv switch --default
ID is allocated by the runtime as a fresh agent_did, not from options.display-name; the receipt returns it with the new Context ID in agent.context_id. Omit target_id and set/clear. For later changes use agent update or context update with the returned ID.
A new Agent with host tools needs a User to grant it WorkspaceRoot access; the receipt's host_root_access gives the User's command. Never widen your own Tools or categories for it.
Page and recipe: ["help","agent"]
## help ["agent","clone","--help"] (ok)
agent [preview] clone  options: from, display-name, profile; optional system-prompt, root, description; argv switch --default
ID is allocated by the runtime as a fresh agent_did, not from options.display-name; the receipt returns it with the new Context ID in agent.context_id. Omit target_id and set/clear. For later changes use agent update or context update with the returned ID.
A new Agent with host tools needs a User to grant it WorkspaceRoot access; the receipt's host_root_access gives the User's command. Never widen your own Tools or categories for it.
Page and recipe: ["help","agent"]
## help ["agent","preview","--help"] (ok)
agent: an agent role. create bundles its Context and Tools; a profile supplies inference settings. get without an ID inspects your current role.
Verbs: list | get ID | create (options.display-name, system-prompt, preset, profile; allocates ID and Context/Tools) | update ID | delete ID. Preview: agent preview create (same options) | update|delete ID.
Also: agent clone, disable, default. Use context get/update for its selected Context.
Fields of Agent: ["display_name","description","context_id","inference_profile_id","enabled","tags"]. Append --help to create/update for shapes.
Create allocates and returns an agent_did; it takes no id. Copy that exact DID from the receipt or agent list when selecting the agent. A change is durable once its transaction commits and runs after the runtime reconciler swaps generations; select the agent in a new session, since existing sessions remain bound to their original agent; canonical documents replicate to authorized paired clients and are reloaded after restart. Creating, cloning and choosing the default need manage_agents on the Node; changing an Agent needs update on that Agent. A profile or backend pick may keep the current account, use an account-free backend, or move to another provider's default account; it never switches between accounts of one provider.
Recipe: a new agent with its own model and run limits
  {"argv":["execution","create"],"target_id":"lead-exec","set":{"deadline_duration_secs":86400,"max_turns":200}}
  {"argv":["profile","create","lead"],"set":{"backend_id":"<BACKEND_ID>","execution_id":"lead-exec","model_name":"<MODEL>"}}
  {"argv":["agent","create"],"options":{"display-name":"Lead","preset":"readonly","profile":"lead","root":"<ROOT>","system-prompt":"<PROMPT>"}}
  <ROOT> is a published WorkspaceRoot path; host tools wait for the User grant in host_root_access, so omit preset and root when the agent needs no files. Copy agent_did from this receipt into <CREATED_AGENT_DID> in the next call
  {"argv":["agent-target","create"],"target_id":"lead","set":{"agent_did":"<CREATED_AGENT_DID>","name":"lead"}}
  {"argv":["tools","update"],"set":{"agents":{"enabled":true,"target_ids":["lead"]}}}
  you can now start it; keep any target_ids tools get already shows
Next: give it tools (["help","tools"]) and test it in a fresh session.
## preview create (ok)
{
  "committed": false,
  "admitted": true,
  "operation": "create",
  "agent_did": "<CREATED_4>",
  "agent": {"agent_did":"<CREATED_4>","creation_nonce":"<NONCE_4>","node_did":"<NODE>","display_name":"Planner","context_id":"<CREATED_4>:context","inference_profile_id":"<SELF>:inference"},
  "make_default": false,
  "config_execution": {"version":1,"mutation_entered":false}
}
## preview create rejected (ok)
{
  "admitted": false,
  "committed": false,
  "refusal": "invalid_input",
  "message": "fresh agents require system_prompt",
  "next_call": {"argv":["agent","create","--help"]},
  "config_execution": {"version":1,"mutation_entered":false}
}
## create without tools (ok)
{
  "committed": true,
  "admitted": true,
  "operation": "create",
  "agent_did": "<CREATED_6>",
  "agent": {"agent_did":"<CREATED_6>","creation_nonce":"<NONCE_6>","node_did":"<NODE>","display_name":"Planner","description":"Plans work","context_id":"<CREATED_6>:context","inference_profile_id":"<SELF>:inference"},
  "make_default": false,
  "config_execution": {"version":1,"mutation_entered":true}
}
## create with host tools (ok)
{
  "committed": true,
  "admitted": true,
  "operation": "create",
  "agent_did": "<CREATED_8>",
  "host_root_access": {"answer":"Saved, but <CREATED_8> cannot use its host tools yet: a User must grant it reader on the published WorkspaceRoot <ROOT>. Until then the runtime cannot load its tools and reports the agent unavailable.","how_to_proceed":"Keep these Tools and give the User user_command to run from their own client; no config or policy call, and no change to your own Tools, can grant it.","published_root":"<ROOT>","state":"waiting_for_user","user_command":"gents config workspace-root grant-reader --path <ROOT> --agent-did <CREATED_8>"},
  "agent": {"agent_did":"<CREATED_8>","creation_nonce":"<NONCE_8>","node_did":"<NODE>","display_name":"Scout","context_id":"<CREATED_8>:context","inference_profile_id":"<SELF>:inference"},
  "make_default": false,
  "config_execution": {"version":1,"mutation_entered":true}
}
## clone (ok)
{
  "committed": true,
  "admitted": true,
  "operation": "clone",
  "agent_did": "<CREATED_10>",
  "agent": {"agent_did":"<CREATED_10>","creation_nonce":"<NONCE_10>","node_did":"<NODE>","display_name":"Copy","context_id":"<CREATED_10>:context","inference_profile_id":"<SIBLING>:inference"},
  "make_default": false,
  "config_execution": {"version":1,"mutation_entered":true}
}
## create without display-name (refused)
tool call error: {"error":"display_name is required","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":true}}
## create without system-prompt (refused)
tool call error: {"error":"fresh agents require system_prompt","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":true}}
## create without profile (refused)
tool call error: {"error":"select a published inference profile","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":true}}
## create with unpublished profile (refused)
tool call error: {"error":"select a published inference profile","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":true}}
## create with unknown preset (refused)
tool call error: {"error":"unknown permission preset","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":true}}
## create with unpublished root (refused)
tool call error: {"error":"Tools.host.root \"/pinned/unpublished\" is not admitted by published WorkspaceRoot policy: nonexistent path is outside the allowed roots","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":true}}
## create with options.id (refused)
tool call error: {"error":"agent create provisions a fresh agent DID and returns it; remove options.id","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":false}}
## create with unknown option (refused)
tool call error: {"error":"unknown agent option --name. agent create takes options.display-name, system-prompt, preset and profile; it allocates the ID and Context/Tools. Use agent update for an existing role; do not supply set fields to this command","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":false}}
## create with a positional (refused)
tool call error: {"error":"unexpected positional argument \"Planner\". agent create takes options.display-name, system-prompt, preset and profile; it allocates the ID and Context/Tools. Use agent update for an existing role; do not supply a positional ID","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":false}}
## create with set and cleared display_name (refused)
tool call error: {"error":"display_name cannot be both set and cleared","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":false}}
## create with target_id (refused)
tool call error: {"error":"agent create/clone allocates the ID from options.display-name; omit target_id. The receipt returns the new agent, Context and Tools IDs; use resource update for later changes","recovery":{"next_call":{"argv":["agent","create","--help"]}},"config_execution":{"version":1,"mutation_entered":false}}
## clone without from (refused)
tool call error: {"error":"clone requires from","recovery":{"next_call":{"argv":["help","agent"]}},"config_execution":{"version":1,"mutation_entered":true}}
## clone with preset (refused)
tool call error: {"error":"clone inherits its source tools; edit the cloned Tools to change grants","recovery":{"next_call":{"argv":["help","agent"]}},"config_execution":{"version":1,"mutation_entered":true}}
## clone without profile (refused)
tool call error: {"error":"select a published inference profile","recovery":{"next_call":{"argv":["help","agent"]}},"config_execution":{"version":1,"mutation_entered":true}}
## clone from an unknown agent (refused)
tool call error: {"error":"unknown agent_did \"did:key:absent\"; copy an exact DID from [\"agent\",\"list\"] or an agent create receipt","recovery":{"next_call":{"argv":["agent","list"]}},"config_execution":{"version":1,"mutation_entered":false}}
## batch stopping at a refused create (refused)
tool call error: {"error":"batch stopped at item 0; earlier items keep their results; 0 later items were not attempted","batch":{"atomic":false,"failed_index":0,"results":[{"config_execution":{"mutation_entered":true,"version":1},"error":"fresh agents require system_prompt","index":0,"ok":false,"recovery":{"next_call":{"argv":["agent","create","--help"]}}}],"unattempted":0},"recovery":{"next_call":{"argv":["help","batch"]}},"config_execution":{"version":1,"mutation_entered":true}}
## create with --default (ok)
{
  "committed": true,
  "admitted": true,
  "operation": "create",
  "agent_did": "<CREATED_12>",
  "agent": {"agent_did":"<CREATED_12>","creation_nonce":"<NONCE_12>","node_did":"<NODE>","display_name":"Planner","context_id":"<CREATED_12>:context","inference_profile_id":"<SELF>:inference"},
  "make_default": true,
  "config_execution": {"version":1,"mutation_entered":true}
}
## disable the default agent (refused)
tool call error: {"error":"agent \"<CREATED_12>\" is or would become the default agent, which must stay enabled; choose another enabled default first, or set enabled true in the same edit","recovery":{"next_call":{"argv":["agent","list"]}},"config_execution":{"version":1,"mutation_entered":true}}
## preview disabling the default agent (ok)
{
  "admitted": false,
  "committed": false,
  "refusal": "default_disabled",
  "message": "agent \"<CREATED_12>\" is or would become the default agent, which must stay enabled; choose another enabled default first, or set enabled true in the same edit",
  "next_call": {"argv":["agent","list"]},
  "config_execution": {"version":1,"mutation_entered":false}
}
## disable the clone source (ok)
{
  "committed": true,
  "admitted": true,
  "operation": "disable",
  "agent_did": "<SIBLING>",
  "agent": {"agent_did":"<SIBLING>","creation_nonce":"<NONCE_14>","node_did":"<NODE>","display_name":"pinned-sibling","context_id":"<SIBLING>:context","inference_profile_id":"<SIBLING>:inference","enabled":false},
  "make_default": false,
  "config_execution": {"version":1,"mutation_entered":true}
}
## clone from a disabled agent (refused)
tool call error: {"error":"clone source must be an enabled agent","recovery":{"next_call":{"argv":["help","agent"]}},"config_execution":{"version":1,"mutation_entered":true}}
"###;
