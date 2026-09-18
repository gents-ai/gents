import type { DeploymentView, Tools } from "@source-inc/gents-desktop-client";
import { AreaRow, ChoiceRow, NumberRow, SwitchRow } from "./editors";
import { DocumentSelection } from "./DocumentSelection";
import { Group } from "./rows";
import type { Shell } from "@/hooks/useShell";
import { RemoteToolDiscovery } from "./RemoteToolDiscovery";
import { useEffect, useState } from "react";

// Mirrors the canonical effective defaults used by Tools::validation_violations.
// The desktop bridge does not currently publish these values in its catalog.
export const TOOL_LIMIT_DEFAULTS = {
  bashTimeout: 120,
  waitTimeout: 30,
  maxWaitTimeout: 600,
  lspTimeout: 20,
  maxLspTimeout: 300,
} as const;

function SecondsRow({
  id,
  label,
  value,
  onChange,
  onInvalid,
  placeholder,
}: {
  id: string;
  label: string;
  value: number | null | undefined;
  onChange: (value: number | null) => void;
  onInvalid: (id: string, label: string | null) => void;
  placeholder: string;
}) {
  const canonical = value == null ? "" : String(value);
  const [raw, setRaw] = useState(canonical);
  useEffect(() => {
    setRaw(canonical);
    onInvalid(id, null);
  }, [canonical, id, onInvalid]);
  useEffect(() => () => onInvalid(id, null), [id, onInvalid]);
  return (
    <NumberRow
      id={id}
      label={label}
      value={raw}
      placeholder={placeholder}
      onCommit={() => {}}
      onEnter={() => {}}
      onChange={(text) => {
        setRaw(text);
        const valid =
          text === "" ||
          (/^[1-9]\d*$/.test(text) && Number.isSafeInteger(Number(text)));
        onInvalid(id, valid ? null : label);
        if (valid) onChange(text === "" ? null : Number(text));
      }}
    />
  );
}

export function parseToolGroups(value: string): Partial<Tools> | null {
  try {
    const parsed: unknown = JSON.parse(value);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    const document = parsed as Partial<Tools>;
    for (const key of [
      "host",
      "remote",
      "subagents",
      "built_ins",
      "datastore",
      "integrations",
      "self_config",
    ] as const) {
      const group = document[key];
      if (group != null && (typeof group !== "object" || Array.isArray(group)))
        return null;
    }
    for (const list of [
      document.subagents?.target_ids,
      document.datastore?.datastore_tool_surface_ids,
    ]) {
      if (
        list != null &&
        (!Array.isArray(list) || list.some((id) => typeof id !== "string"))
      )
        return null;
    }
    if (
      document.remote?.services != null &&
      (!Array.isArray(document.remote.services) ||
        document.remote.services.some(
          (service) =>
            !service ||
            typeof service !== "object" ||
            typeof service.mcp_service_id !== "string" ||
            (service.tool_names != null &&
              (!Array.isArray(service.tool_names) ||
                service.tool_names.some((name) => typeof name !== "string"))),
        ))
    )
      return null;
    return document;
  } catch {
    return null;
  }
}

/** These controls edit the canonical nested groups, not a second config schema. */
export function ToolGroupControls({
  value,
  onChange,
  deployment,
  onCreateTarget,
  shell,
  onInvalid,
}: {
  value: string;
  onChange: (value: string) => void;
  deployment: DeploymentView;
  onCreateTarget: (behaviorId: string) => void;
  shell: Shell;
  onInvalid: (id: string, label: string | null) => void;
}) {
  const groups = parseToolGroups(value);
  if (!groups)
    return (
      <p role="alert" className="mb-4 text-sm text-destructive">
        Repair the canonical JSON to use the guided controls.
      </p>
    );
  const update = <K extends keyof Tools>(
    key: K,
    patch: Partial<NonNullable<Tools[K]>>,
  ) =>
    onChange(
      JSON.stringify(
        { ...groups, [key]: { ...(groups[key] as object), ...patch } },
        null,
        2,
      ),
    );
  const flags = <K extends "built_ins" | "self_config" | "subagents" | "datastore">(
    key: K,
    entries: [keyof NonNullable<Tools[K]>, string][],
  ) =>
    entries.map(([field, label]) => (
      <SwitchRow
        key={String(field)}
        id={`tools-${key}-${String(field)}`}
        label={label}
        checked={(groups[key] as NonNullable<Tools[K]> | undefined)?.[field] === true}
        onChange={(checked) =>
          update(key, { [field]: checked } as Partial<NonNullable<Tools[K]>>)
        }
      />
    ));
  const seconds = (
    id: string,
    label: string,
    value: number | null | undefined,
    onValidChange: (value: number | null) => void,
    placeholder: string,
  ) => (
    <SecondsRow
      id={id}
      label={label}
      value={value}
      placeholder={placeholder}
      onInvalid={onInvalid}
      onChange={onValidChange}
    />
  );
  return (
    <>
      <Group title="Host execution policy">
        <ChoiceRow
          id="tools-bash-execution"
          label="Bash execution restrictions"
          value={groups.host?.bash?.execution_mode ?? ""}
          items={[
            { value: "", label: "Mode-derived policy" },
            { value: "read_only", label: "Read only" },
            { value: "workspace_write", label: "Workspace write" },
            { value: "artifact_write", label: "Artifact write" },
            { value: "unrestricted", label: "Unrestricted" },
          ]}
          onChange={(mode) =>
            update("host", {
              bash: {
                ...groups.host?.bash,
                execution_mode: mode
                  ? (mode as NonNullable<
                      NonNullable<Tools["host"]>["bash"]
                    >["execution_mode"])
                  : null,
              },
            })
          }
        />
        {seconds(
          "tools-files-timeout",
          "File operation timeout seconds",
          groups.host?.files?.timeout_secs,
          (timeout_secs) =>
            update("host", {
              files: { ...groups.host?.files, timeout_secs },
            }),
          "Request deadline",
        )}
        {seconds(
          "tools-bash-timeout",
          "Bash timeout seconds",
          groups.host?.bash?.timeout_secs,
          (timeout_secs) =>
            update("host", { bash: { ...groups.host?.bash, timeout_secs } }),
          String(TOOL_LIMIT_DEFAULTS.bashTimeout),
        )}
        {seconds(
          "tools-bash-max-timeout",
          "Maximum bash timeout seconds",
          groups.host?.bash?.max_timeout_secs,
          (max_timeout_secs) =>
            update("host", { bash: { ...groups.host?.bash, max_timeout_secs } }),
          "Bash timeout",
        )}
        {seconds(
          "tools-bash-background-timeout",
          "Background lifetime seconds",
          groups.host?.bash?.background_timeout_secs,
          (background_timeout_secs) =>
            update("host", {
              bash: { ...groups.host?.bash, background_timeout_secs },
            }),
          "36000",
        )}
        <ChoiceRow
          id="tools-bash-network"
          label="Bash network access"
          value={groups.host?.bash?.network_mode ?? ""}
          items={[
            { value: "", label: "Runtime default" },
            { value: "inherit", label: "Inherit" },
            { value: "disabled", label: "Disabled" },
            { value: "enabled", label: "Enabled" },
          ]}
          onChange={(mode) =>
            update("host", {
              bash: {
                ...groups.host?.bash,
                network_mode: mode
                  ? (mode as "inherit" | "disabled" | "enabled")
                  : null,
              },
            })
          }
        />
        {seconds(
          "tools-subagent-spawn-timeout",
          "Remote spawn claim timeout seconds",
          groups.subagents?.cross_principal_spawn_timeout_secs,
          (cross_principal_spawn_timeout_secs) =>
            update("subagents", { cross_principal_spawn_timeout_secs }),
          "60",
        )}
        {seconds(
          "tools-subagent-wait-timeout",
          "Background wait seconds",
          groups.subagents?.wait_timeout_secs,
          (wait_timeout_secs) => update("subagents", { wait_timeout_secs }),
          String(TOOL_LIMIT_DEFAULTS.waitTimeout),
        )}
        {seconds(
          "tools-subagent-max-wait-timeout",
          "Maximum background wait seconds",
          groups.subagents?.max_wait_timeout_secs,
          (max_wait_timeout_secs) => update("subagents", { max_wait_timeout_secs }),
          String(TOOL_LIMIT_DEFAULTS.maxWaitTimeout),
        )}
      </Group>
      <Group title="Runtime tools">
        {flags("built_ins", [
          ["enable_graph_tools", "Graph tools"],
          ["enable_goal_tools", "Read and update goals"],
          ["enable_goal_creation", "Create goals"],
          ["enable_memory", "Memory"],
          ["enable_session_history_tool", "Session history"],
          ["enable_context_budget", "Context budget"],
        ])}
      </Group>
      <Group title="Self-configuration">
        {flags("self_config", [
          ["enable_self_config", "Configure this agent"],
          ["enable_pack_install", "Install graph packs"],
          ["self_config_no_lockout", "Prevent self-configuration lockout"],
          ["self_config_dry_run", "Preview configuration changes"],
        ])}
        <p className="px-4 pb-4 text-sm text-muted-foreground">
          Permissions are independent opt-ins. Process authority remains the ceiling.
        </p>
      </Group>
      <Group title="Subagents">
        {flags("subagents", [
          ["spawn_enabled", "Spawn subagents"],
          ["steering_enabled", "Steer subagents"],
          ["background_enabled", "Background subagents"],
          ["allow_cross_principal", "Allow cross-principal delegation"],
        ])}
        <ChoiceRow
          id="tools-subagent-await"
          label="Default subagent wait"
          value={groups.subagents?.default_await_mode ?? ""}
          items={[
            { value: "", label: "Runtime default (foreground)" },
            { value: "foreground", label: "Foreground" },
            { value: "background", label: "Background" },
          ]}
          onChange={(mode) => update("subagents", { default_await_mode: mode || null })}
        />
        <DocumentSelection
          label="Subagent targets"
          options={(deployment.subagentTargets ?? []).map((target) => ({
            value: target.target_id,
            label: target.name,
            description: `${target.behavior_id}${target.target_agent_did !== deployment.agentDid ? ` · ${target.target_agent_did}` : ""}`,
          }))}
          selected={groups.subagents?.target_ids ?? []}
          onChange={(ids) =>
            update("subagents", { target_ids: ids.length ? ids : null })
          }
        />
        <p className="px-4 pb-4 text-sm text-muted-foreground">
          Select explicit delegation targets. Enabling spawn does not grant access to
          unselected behaviors.
        </p>
        <ChoiceRow
          id="tools-create-target"
          label="Add a local delegation target"
          value=""
          items={[
            { value: "", label: "Choose a behavior…" },
            ...deployment.behaviorConfigs
              .filter(
                (behavior) =>
                  !deployment.subagentTargets.some(
                    (target) =>
                      target.target_agent_did === deployment.agentDid &&
                      target.behavior_id === behavior.behavior_id,
                  ),
              )
              .map((behavior) => ({
                value: behavior.behavior_id,
                label: behavior.display_name ?? behavior.behavior_id,
              })),
          ]}
          onChange={(id) => {
            if (id) onCreateTarget(id);
          }}
        />
        <p className="px-4 pb-4 text-sm text-muted-foreground">
          New targets are created with Save. Select the target above to grant access.
        </p>
      </Group>
      <Group title="Remote Tools">
        <DocumentSelection
          label="Remote services"
          options={deployment.toolServiceRegistries.map((service) => ({
            value: service.service_id,
            label: service.display_name ?? service.service_id,
            description: service.description,
          }))}
          selected={(groups.remote?.services ?? []).map(
            (service) => service.mcp_service_id,
          )}
          onChange={(ids) =>
            update("remote", {
              services: ids.length
                ? ids.map(
                    (id) =>
                      groups.remote?.services?.find(
                        (service) => service.mcp_service_id === id,
                      ) ?? { mcp_service_id: id, tool_names: null },
                  )
                : null,
            })
          }
        />
        {(groups.remote?.services ?? []).map((service, index) => (
          <div key={service.mcp_service_id}>
            <RemoteToolDiscovery
              key={JSON.stringify(
                deployment.toolServiceRegistries.find(
                  (row) => row.service_id === service.mcp_service_id,
                ),
              )}
              selected={service.tool_names ?? []}
              onChange={(names) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index
                      ? {
                          ...row,
                          tool_names: names.length ? names : null,
                          ...(Object.prototype.hasOwnProperty.call(
                            row,
                            "background_tool_names",
                          )
                            ? {
                                background_tool_names:
                                  row.background_tool_names?.filter((name) =>
                                    names.includes(name),
                                  ) || null,
                              }
                            : {}),
                        }
                      : row,
                  ),
                })
              }
              discover={async () => {
                const config = deployment.toolServiceRegistries.find(
                  (row) => row.service_id === service.mcp_service_id,
                );
                if (!config) throw new Error("Remote service is unavailable");
                const result = await shell.api.testToolService({
                  serviceId: config.service_id,
                  hostname: config.hostname ?? null,
                  tailscaleIp: config.tailscale_ip ?? null,
                  lanIp: config.lan_ip ?? null,
                  mcpPort: config.mcp_port ?? null,
                  mcpPath: config.mcp_path ?? null,
                });
                if (result.error) throw new Error(result.error);
                return result.tools;
              }}
            />
            <AreaRow
              id={`remote-tools-${index}`}
              label={`${deployment.toolServiceRegistries.find((row) => row.service_id === service.mcp_service_id)?.display_name ?? service.mcp_service_id} tool names`}
              description="Exact tool names, one per line. Empty grants no tools; discovery never grants new tools automatically."
              value={(service.tool_names ?? []).join("\n")}
              onCommit={() => {}}
              onChange={(value) => {
                const names = value
                  .split("\n")
                  .map((name) => name.trim())
                  .filter(Boolean);
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index
                      ? {
                          ...row,
                          tool_names: names.length ? names : null,
                          ...(Object.prototype.hasOwnProperty.call(
                            row,
                            "background_tool_names",
                          )
                            ? {
                                background_tool_names:
                                  row.background_tool_names?.filter((name) =>
                                    names.includes(name),
                                  ) || null,
                              }
                            : {}),
                        }
                      : row,
                  ),
                });
              }}
            />
            <DocumentSelection
              label="Background remote tools"
              options={(service.tool_names ?? []).map((name) => ({
                value: name,
                label: name,
              }))}
              selected={service.background_tool_names ?? []}
              onChange={(names) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index
                      ? {
                          ...row,
                          background_tool_names: names.length ? names : null,
                        }
                      : row,
                  ),
                })
              }
            />
            <ChoiceRow
              id={`remote-style-${index}`}
              label="Remote tool presentation"
              value={service.style ?? "discovery"}
              items={[
                { value: "discovery", label: "Discover tools on demand" },
                { value: "flat", label: "Expose selected tools directly" },
              ]}
              onChange={(style) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index
                      ? { ...row, style: style as "flat" | "discovery" }
                      : row,
                  ),
                })
              }
            />
            <SwitchRow
              id={`remote-required-${index}`}
              label="Required for admission"
              description="Block new work while this selected service is unavailable."
              checked={service.required === true}
              onChange={(required) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index ? { ...row, required } : row,
                  ),
                })
              }
            />
            {seconds(
              `remote-connect-timeout-${index}`,
              "Connection timeout seconds",
              service.connect_timeout_secs,
              (connect_timeout_secs) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index ? { ...row, connect_timeout_secs } : row,
                  ),
                }),
              "15",
            )}
            {seconds(
              `remote-discovery-timeout-${index}`,
              "Discovery timeout seconds",
              service.discovery_timeout_secs,
              (discovery_timeout_secs) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index ? { ...row, discovery_timeout_secs } : row,
                  ),
                }),
              "30",
            )}
            {seconds(
              `remote-call-timeout-${index}`,
              "Tool call timeout seconds",
              service.timeout_secs,
              (timeout_secs) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index ? { ...row, timeout_secs } : row,
                  ),
                }),
              "300",
            )}
            {seconds(
              `remote-stale-timeout-${index}`,
              "Stale-health timeout seconds",
              service.stale_timeout_secs,
              (stale_timeout_secs) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index ? { ...row, stale_timeout_secs } : row,
                  ),
                }),
              "120",
            )}
            {seconds(
              `remote-background-timeout-${index}`,
              "Background lifetime seconds",
              service.background_timeout_secs,
              (background_timeout_secs) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index ? { ...row, background_timeout_secs } : row,
                  ),
                }),
              "36000",
            )}
            {seconds(
              `remote-wait-timeout-${index}`,
              "Background wait seconds",
              service.wait_timeout_secs,
              (wait_timeout_secs) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index ? { ...row, wait_timeout_secs } : row,
                  ),
                }),
              String(TOOL_LIMIT_DEFAULTS.waitTimeout),
            )}
            {seconds(
              `remote-max-wait-timeout-${index}`,
              "Maximum background wait seconds",
              service.max_wait_timeout_secs,
              (max_wait_timeout_secs) =>
                update("remote", {
                  services: groups.remote!.services!.map((row, i) =>
                    i === index ? { ...row, max_wait_timeout_secs } : row,
                  ),
                }),
              String(TOOL_LIMIT_DEFAULTS.maxWaitTimeout),
            )}
          </div>
        ))}
      </Group>
      <Group title="Datastore">
        {flags("datastore", [["enable_defra_query", "Query datastore"]])}
        <DocumentSelection
          label="Datastore surfaces"
          options={(deployment.datastoreToolSurfaces ?? []).map((surface) => ({
            value: surface.surface_id,
            label: surface.display_name ?? surface.surface_id,
          }))}
          selected={groups.datastore?.datastore_tool_surface_ids ?? []}
          onChange={(ids) =>
            update("datastore", { datastore_tool_surface_ids: ids.length ? ids : null })
          }
        />
      </Group>
      <Group title="Integrations">
        <SwitchRow
          id="tools-lsp"
          label="Language server tools"
          description="Uses configured language servers and the selected host root. Additional language-server settings remain in canonical JSON."
          checked={groups.integrations?.lsp != null}
          onChange={(enabled) => update("integrations", { lsp: enabled ? {} : null })}
        />
        {groups.integrations?.lsp != null && (
          <>
            <AreaRow
              id="tools-lsp-config"
              label="Language server configuration"
              description="Runtime-owned language-server flags and catalog overrides."
              value={groups.integrations.lsp.config ?? ""}
              onCommit={() => {}}
              onChange={(config) =>
                update("integrations", {
                  lsp: { ...groups.integrations?.lsp, config: config || null },
                })
              }
              rows={5}
              mono
            />
            {seconds(
              "tools-lsp-timeout",
              "Language server action timeout seconds",
              groups.integrations.lsp.timeout_secs,
              (timeout_secs) =>
                update("integrations", {
                  lsp: { ...groups.integrations?.lsp, timeout_secs },
                }),
              String(TOOL_LIMIT_DEFAULTS.lspTimeout),
            )}
            {seconds(
              "tools-lsp-max-timeout",
              "Maximum language server timeout seconds",
              groups.integrations.lsp.max_timeout_secs,
              (max_timeout_secs) =>
                update("integrations", {
                  lsp: { ...groups.integrations?.lsp, max_timeout_secs },
                }),
              String(TOOL_LIMIT_DEFAULTS.maxLspTimeout),
            )}
            {seconds(
              "tools-lsp-rpc-timeout",
              "Language server RPC timeout seconds",
              groups.integrations.lsp.rpc_timeout_secs,
              (rpc_timeout_secs) =>
                update("integrations", {
                  lsp: { ...groups.integrations?.lsp, rpc_timeout_secs },
                }),
              "30",
            )}
          </>
        )}
      </Group>
    </>
  );
}
