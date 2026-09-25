import type {
  DeploymentView,
  Tools,
  SubagentTargetDocument,
} from "@source-inc/gents-desktop-client";
import { dependentsWarning } from "./dependents";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  SwitchRow,
  TextRow,
  PathRow,
} from "./editors";
import { newId, optionalAbsolutePath, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import {
  FILE_LIMITS,
  TOOL_LIMIT_DEFAULTS,
  ToolGroupControls,
} from "./ToolGroupControls";
import { RowMenu } from "./RowMenu";
import { useCallback, useState } from "react";

/* the document a draft starts from: read-only files, no commands */
export function newToolsDocument(deployment: DeploymentView): Tools {
  return {
    tools_id: newId("tools"),
    agent_did: deployment.agentDid,
    display_name: "",
    host: { files: { mode: "ReadOnly" }, bash: { mode: "Off" } },
  };
}

export function ToolsEditor({
  shell,
  deployment,
  tools,
  embedded = false,
  draft: draftMode,
}: {
  shell: Shell;
  deployment: DeploymentView;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
  tools: Tools;
  /* a new document that exists only here until Save */
  draft?: { onSaved: (toolsId: string) => void; onCancel: () => void };
}) {
  const [invalidLimits, setInvalidLimits] = useState<Record<string, string>>({});
  const [controlsGeneration, setControlsGeneration] = useState(0);
  const reportInvalidLimit = useCallback((id: string, label: string | null) => {
    setInvalidLimits((current) => {
      if ((current[id] ?? null) === label) return current;
      const next = { ...current };
      if (label === null) delete next[id];
      else next[id] = label;
      return next;
    });
  }, []);
  const limitError = Object.values(invalidLimits).length
    ? `${Object.values(invalidLimits).join(", ")} must be a positive whole number`
    : null;
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
    pendingTargets: [] as SubagentTargetDocument[],
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
  const d = useDraft(
    saved,
    async (next) => {
      if (limitError) throw new Error(limitError);
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
      if (
        "tools_id" in advanced ||
        "agent_did" in advanced ||
        "display_name" in advanced
      )
        throw new Error("IDs and display name are edited in their dedicated fields");
      const positiveWholeNumber = (label: string, value: unknown) => {
        if (
          value != null &&
          (typeof value !== "number" || !Number.isInteger(value) || value < 1)
        )
          throw new Error(`${label} must be a positive whole number`);
      };
      const boundedSeconds = (
        label: string,
        authored: number | null | undefined,
        maximum: number | null | undefined,
        fallback: number,
        maximumFallback?: number,
      ) => {
        const effective = authored ?? fallback;
        const effectiveMaximum = maximum ?? maximumFallback ?? effective;
        if (effectiveMaximum < effective)
          throw new Error(`${label} maximum must be at least its default`);
      };
      for (const [label, value] of [
        ["Bash timeout", advanced.host?.bash?.timeout_secs],
        ["Maximum bash timeout", advanced.host?.bash?.max_timeout_secs],
        ["Background bash timeout", advanced.host?.bash?.background_timeout_secs],
        ["Bash wait timeout", advanced.host?.bash?.wait_timeout_secs],
        ["Maximum bash wait timeout", advanced.host?.bash?.max_wait_timeout_secs],
        [
          "Subagent spawn timeout",
          advanced.subagents?.cross_principal_spawn_timeout_secs,
        ],
        ["Language server timeout", advanced.integrations?.lsp?.timeout_secs],
        [
          "Maximum language server timeout",
          advanced.integrations?.lsp?.max_timeout_secs,
        ],
      ] as const)
        positiveWholeNumber(label, value);
      for (const [field, { label, max }] of Object.entries(FILE_LIMITS)) {
        const value = advanced.host?.files?.[field as keyof typeof FILE_LIMITS];
        positiveWholeNumber(label, value);
        if (typeof value === "number" && value > max)
          throw new Error(`${label} must be at most ${max.toLocaleString("en-US")}`);
      }
      boundedSeconds(
        "Bash timeout",
        advanced.host?.bash?.timeout_secs,
        advanced.host?.bash?.max_timeout_secs,
        TOOL_LIMIT_DEFAULTS.bashTimeout,
      );
      boundedSeconds(
        "Bash wait timeout",
        advanced.host?.bash?.wait_timeout_secs,
        advanced.host?.bash?.max_wait_timeout_secs,
        TOOL_LIMIT_DEFAULTS.waitTimeout,
        TOOL_LIMIT_DEFAULTS.maxWaitTimeout,
      );
      boundedSeconds(
        "Language server timeout",
        advanced.integrations?.lsp?.timeout_secs,
        advanced.integrations?.lsp?.max_timeout_secs,
        TOOL_LIMIT_DEFAULTS.lspTimeout,
        TOOL_LIMIT_DEFAULTS.maxLspTimeout,
      );
      for (const target of advanced.subagents?.target_ids ?? []) {
        if (
          ![...(deployment.subagentTargets ?? []), ...next.pendingTargets].some(
            (row) => row.target_id === target,
          )
        )
          throw new Error(`Unknown subagent target: ${target}`);
      }
      for (const service of advanced.remote?.services ?? []) {
        const names = [
          ...new Set(
            (service.tool_names ?? []).map((name) => name.trim()).filter(Boolean),
          ),
        ];
        service.tool_names = names.length ? names : null;
        if (Object.prototype.hasOwnProperty.call(service, "background_tool_names")) {
          const backgroundNames = [
            ...new Set(
              (service.background_tool_names ?? [])
                .map((name) => name.trim())
                .filter(Boolean),
            ),
          ];
          service.background_tool_names = backgroundNames.length
            ? backgroundNames
            : null;
        }
        if (
          !deployment.toolServiceRegistries.some(
            (row) => row.service_id === service.mcp_service_id,
          )
        )
          throw new Error(`Unknown remote service: ${service.mcp_service_id}`);
        if (service.tool_names?.some((name) => /[*?]/.test(name)))
          throw new Error("Remote tool names must be exact names, not wildcards");
        if (
          service.background_tool_names?.some(
            (name) => !service.tool_names?.includes(name),
          )
        )
          throw new Error("Background remote tools must be selected tool names");
        for (const [label, value] of [
          ["Remote connection timeout", service.connect_timeout_secs],
          ["Remote discovery timeout", service.discovery_timeout_secs],
          ["Remote call timeout", service.timeout_secs],
          ["Remote stale-health timeout", service.stale_timeout_secs],
          ["Remote background timeout", service.background_timeout_secs],
          ["Remote wait timeout", service.wait_timeout_secs],
          ["Maximum remote wait timeout", service.max_wait_timeout_secs],
        ] as const)
          positiveWholeNumber(label, value);
        boundedSeconds(
          "Remote wait timeout",
          service.wait_timeout_secs,
          service.max_wait_timeout_secs,
          TOOL_LIMIT_DEFAULTS.waitTimeout,
          TOOL_LIMIT_DEFAULTS.maxWaitTimeout,
        );
      }
      for (const surface of advanced.datastore?.datastore_tool_surface_ids ?? []) {
        if (
          !deployment.datastoreToolSurfaces?.some((row) => row.surface_id === surface)
        )
          throw new Error(`Unknown datastore surface: ${surface}`);
      }
      const advancedHost =
        advanced.host && typeof advanced.host === "object" ? advanced.host : {};
      const document: Tools = {
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
            mode: next.bash as NonNullable<NonNullable<Tools["host"]>["bash"]>["mode"],
            background_enabled: next.background,
          },
        },
      };
      if (next.pendingTargets.length) {
        await shell.applyConfig((api) =>
          api.applyConfigComponents({
            document: {
              agent_principal: { agent_did: deployment.agentDid },
              tools: [document],
              subagent_targets: next.pendingTargets,
            },
          }),
        );
      } else {
        await shell.applyConfig((api) => api.saveToolsConfig({ document }));
      }
    },
    { isNew: draftMode !== undefined },
  );
  const id = (f: string) => `${tools.tools_id}-${f}`;
  return (
    <>
      <Group
        title={
          embedded || draftMode ? undefined : (tools.display_name ?? tools.tools_id)
        }
      >
        {!draftMode && (
          <FactRow label="Tools ID" mono>
            {tools.tools_id}
          </FactRow>
        )}
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <PathRow
          id={id("root")}
          label="Workspace root"
          description="The directory file and command tools are confined to."
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
      <ToolGroupControls
        key={controlsGeneration}
        onInvalid={reportInvalidLimit}
        shell={shell}
        value={d.draft.advanced}
        onChange={(value) => d.set("advanced", value)}
        deployment={{
          ...deployment,
          subagentTargets: [
            ...(deployment.subagentTargets ?? []),
            ...d.draft.pendingTargets.filter(
              (target) =>
                !deployment.subagentTargets?.some(
                  (saved) => saved.target_id === target.target_id,
                ),
            ),
          ],
        }}
        onCreateTarget={(behaviorId) => {
          const behavior = deployment.behaviorConfigs.find(
            (row) => row.behavior_id === behaviorId,
          );
          if (!behavior) return;
          const target: SubagentTargetDocument = {
            target_id: newId("target"),
            agent_did: deployment.agentDid,
            target_agent_did: deployment.agentDid,
            behavior_id: behaviorId,
            name: behavior.display_name ?? behaviorId,
            description: behavior.description ?? null,
          };
          d.set("pendingTargets", [...d.draft.pendingTargets, target]);
        }}
      />
      <Group title="Advanced tool groups">
        {/* the whole document as JSON: an escape hatch, closed by default; open, it
            shows in full so the page is the only thing that scrolls */}
        <details>
          <summary className="cursor-pointer px-5 py-4 text-sm text-muted-foreground">
            Canonical JSON
          </summary>
          <AreaRow
            id={id("advanced")}
            label="Canonical JSON"
            stacked
            expandedByDefault
            description="Host limits, MCP grants, subagents, built-ins, datastore, integrations, self-config, and tags. Invalid or unknown fields are rejected before persistence."
            value={d.draft.advanced}
            onChange={(v) => d.set("advanced", v)}
            onCommit={d.commit}
            rows={12}
            mono
          />
        </details>
      </Group>
      <DraftActions
        dirty={d.dirty || limitError !== null}
        saving={d.saving}
        error={limitError ?? d.error}
        saveLabel={draftMode ? "Create" : undefined}
        onSave={() => {
          if (limitError) return;
          if (draftMode) {
            void d.save().then((ok) => ok && draftMode.onSaved(tools.tools_id));
            return;
          }
          void d.save();
        }}
        onCancel={() => {
          if (draftMode) {
            draftMode.onCancel();
            return;
          }
          d.reset();
          setInvalidLimits({});
          setControlsGeneration((current) => current + 1);
        }}
      />
      {!embedded && !draftMode && (
        <DeleteButton
          label={tools.display_name ?? tools.tools_id}
          warning={dependentsWarning(deployment, "tools", tools.tools_id)}
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
      )}
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
        tags: t.tags,
        trailing: (
          <RowMenu
            name={t.display_name ?? t.tools_id}
            base={base}
            id={t.tools_id}
            onDuplicate={async () => {
              const tools_id = newId("tools");
              await shell.applyConfig((api) =>
                api.saveToolsConfig({
                  document: {
                    ...t,
                    tools_id,
                    display_name: `${t.display_name ?? t.tools_id} copy`,
                  },
                }),
              );
              return tools_id;
            }}
            onDelete={() =>
              shell.applyConfig((api) =>
                api.deleteToolsConfig({
                  toolsId: t.tools_id,
                  agentDid: deployment.agentDid,
                }),
              )
            }
            warning={dependentsWarning(deployment, "tools", t.tools_id)}
          />
        ),
      }))}
      createLabel="New tools"
      empty="No Tools documents. A behavior reaches tools only through its context."
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
          <ToolsEditor
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
