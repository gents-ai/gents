import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { expect, it } from "vitest";

import { deployment } from "../config-panel-wiring/fixtures";
import { engineerPatches } from "../../src/ui/lib/engineer";

type Preset = { enabled: boolean; effective: string[] };
const run = promisify(execFile);

it("grants the Engineer exactly the executed Lean engineerGroup categories", async () => {
  const cwd = join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../crates/gents/proofs",
  );
  await run("lake", ["build", "Proofs.Conformance.Contracts.Json.SelfConfig"], {
    cwd,
    timeout: 240_000,
  });
  const { stdout } = await run(
    "lake",
    ["env", "lean", "--run", "EngineerSelfConfigConformance.lean"],
    { cwd, timeout: 240_000 },
  );
  const modeled = JSON.parse(stdout) as Preset;
  const context = {
    context_id: "context-a",
    node_did: deployment.nodeDid,
    display_name: "Default",
    description: null,
    system_prompt: null,
    tools_id: "tools-a",
    compaction_id: null,
    skill_ids: null,
    tags: [],
  };
  const tools = engineerPatches({
    ...deployment,
    contexts: [context],
    tools: [
      { tools_id: "tools-a", node_did: deployment.nodeDid, display_name: "Standard" },
    ],
  }).find((patch) => patch.collection === "Tools");
  const selfConfig = tools?.changes.self_config as
    { enable_self_config?: boolean; self_config_categories?: string[] } | undefined;
  expect(selfConfig?.enable_self_config).toBe(modeled.enabled);
  expect([...(selfConfig?.self_config_categories ?? [])].sort()).toEqual(
    [...modeled.effective].sort(),
  );
}, 300_000);
