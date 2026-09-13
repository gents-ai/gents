import type { DeploymentView, Tools } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  SwitchRow,
  TextRow,
} from "./editors";
import { newId, optionalAbsolutePath, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";

function Editor({
  shell,
  deployment,
  tools,
}: {
  shell: Shell;
  deployment: DeploymentView;
  tools: Tools;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "tools",
  };
  const saved = {
    displayName: tools.display_name ?? "",
    root: tools.host?.root ?? "",
    files: (tools.host?.files?.mode ?? "Off") as "Off" | "ReadOnly" | "ReadWrite",
    bash: (tools.host?.bash?.mode ?? "Off") as "Off" | "ReadOnly" | "Unrestricted",
    background: tools.host?.bash?.background_enabled ?? false,
    advanced: JSON.stringify(
      {
        host: tools.host ?? null,
        remote: tools.remote ?? null,
        subagents: tools.subagents ?? null,
        built_ins: tools.built_ins ?? null,
        datastore: tools.datastore ?? null,
        integrations: tools.integrations ?? null,
        self_config: tools.self_config ?? null,
        tags: tools.tags ?? null,
      },
      null,
      2,
    ),
  };
  const d = useDraft(saved, async (next) => {
    const root = optionalAbsolutePath("Workspace root", next.root);
    let advanced: Partial<Tools>;
    try {
      advanced = JSON.parse(next.advanced) as Partial<Tools>;
    } catch {
      throw new Error("Advanced configuration must be valid JSON");
    }
    if (!advanced || typeof advanced !== "object" || Array.isArray(advanced))
      throw new Error("Advanced configuration must be a JSON object");
    const allowed = new Set([
      "host",
      "remote",
      "subagents",
      "built_ins",
      "datastore",
      "integrations",
      "self_config",
      "tags",
    ]);
    const unknown = Object.keys(advanced).find((key) => !allowed.has(key));
    if (unknown) throw new Error(`Unknown advanced configuration field: ${unknown}`);
    if ("tools_id" in advanced || "agent_did" in advanced || "display_name" in advanced)
      throw new Error("IDs and display name are edited in their dedicated fields");
    const advancedHost =
      advanced.host && typeof advanced.host === "object" ? advanced.host : {};
    await shell.applyConfig((api) =>
      api.saveToolsConfig({
        document: {
          ...tools,
          ...advanced,
          tools_id: tools.tools_id,
          agent_did: deployment.agentDid,
          display_name: next.displayName.trim() || null,
          host: {
            ...advancedHost,
            root,
            files: {
              ...(advancedHost.files ?? {}),
              mode: next.files as NonNullable<
                NonNullable<Tools["host"]>["files"]
              >["mode"],
            },
            bash: {
              ...(advancedHost.bash ?? {}),
              mode: next.bash as NonNullable<
                NonNullable<Tools["host"]>["bash"]
              >["mode"],
              background_enabled: next.background,
            },
          },
        },
      }),
    );
  });
  const id = (f: string) => `${tools.tools_id}-${f}`;
  return (
    <>
      <Group title={tools.display_name ?? tools.tools_id}>
        <FactRow label="Tools ID" mono>
          {tools.tools_id}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("root")}
          label="Workspace root"
          value={d.draft.root}
          onChange={(v) => d.set("root", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id("files")}
          label="Files"
          value={d.draft.files}
          onChange={(v) => d.choose("files", v as "Off" | "ReadOnly" | "ReadWrite")}
          items={[
            { value: "Off", label: "Off" },
            { value: "ReadOnly", label: "Read only" },
            { value: "ReadWrite", label: "Read / write" },
          ]}
        />
        <ChoiceRow
          id={id("bash")}
          label="Bash"
          value={d.draft.bash}
          onChange={(v) => d.choose("bash", v as "Off" | "ReadOnly" | "Unrestricted")}
          items={[
            { value: "Off", label: "Off" },
            { value: "ReadOnly", label: "Read only" },
            { value: "Unrestricted", label: "Unrestricted" },
          ]}
        />
        <SwitchRow
          id={id("background")}
          label="Background processes"
          description="Permit background execution for the selected bash capability."
          checked={d.draft.background}
          onChange={(v) => d.choose("background", v)}
        />
      </Group>
      <Group title="Advanced tool groups">
        <AreaRow
          id={id("advanced")}
          label="Canonical JSON"
          description="Host limits, MCP grants, subagents, built-ins, datastore, integrations, self-config, and tags. Invalid or unknown fields are rejected before persistence."
          value={d.draft.advanced}
          onChange={(v) => d.set("advanced", v)}
          onCommit={d.commit}
          rows={18}
          mono
        />
      </Group>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        onSave={d.save}
        onCancel={d.reset}
      />
      <DeleteButton
        label={tools.display_name ?? tools.tools_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteToolsConfig({
              toolsId: tools.tools_id,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function ToolsPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell;
  deployment: DeploymentView;
  item?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "tools",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.tools.map((t) => ({
        id: t.tools_id,
        title: t.display_name ?? t.tools_id,
        meta: t.host?.files?.mode ?? "no host tools",
      }))}
      createLabel="New tools"
      empty="No Tools documents. A behaviour reaches tools only through its context."
      onCreate={async () => {
        const tools_id = newId("tools");
        await shell.applyConfig((api) =>
          api.saveToolsConfig({
            document: {
              tools_id,
              agent_did: deployment.agentDid,
              display_name: "New tools",
              host: { files: { mode: "ReadOnly" }, bash: { mode: "Off" } },
              tags: null,
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "tools",
          item: tools_id,
        });
      }}
      detail={(id) => {
        const tools = deployment.tools.find((t) => t.tools_id === id)!;
        return (
          <Editor
            key={tools.tools_id}
            shell={shell}
            deployment={deployment}
            tools={tools}
          />
        );
      }}
    />
  );
}
