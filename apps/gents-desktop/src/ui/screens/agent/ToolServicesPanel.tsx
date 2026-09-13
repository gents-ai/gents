import type {
  DeploymentView,
  ToolServiceRegistry,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { Button } from "@gents/ui/components/button";
import { toast } from "sonner";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  DraftActions,
  FactRow,
  NumberRow,
  SwitchRow,
  TextRow,
} from "./editors";
import { fromLinesOrNull, newId, optionalInteger, toLines, useDraft } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";

function Editor({
  shell,
  deployment,
  service,
}: {
  shell: Shell;
  deployment: DeploymentView;
  service: ToolServiceRegistry;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "tool-services",
  };
  const saved = {
    displayName: service.display_name ?? "",
    description: service.description ?? "",
    hostname: service.hostname ?? "",
    tailscaleIp: service.tailscale_ip ?? "",
    lanIp: service.lan_ip ?? "",
    mcpPort: service.mcp_port != null ? String(service.mcp_port) : "",
    mcpPath: service.mcp_path ?? "",
    sendAgentDid: service.send_agent_did ?? false,
    enabled: service.enabled ?? true,
    tags: toLines(service.tags ?? []),
  };
  const validatedEndpoint = (next: typeof saved) => {
    if (![next.hostname, next.tailscaleIp, next.lanIp].some((v) => v.trim()))
      throw new Error("Hostname, Tailscale IP, or LAN IP is required");
    const mcpPort = optionalInteger("MCP port", next.mcpPort, {
      min: 1,
      max: 65_535,
    });
    if (mcpPort == null) throw new Error("MCP port is required");
    if (next.mcpPath && !next.mcpPath.startsWith("/"))
      throw new Error("MCP path must be empty or start with /");
    return {
      hostname: next.hostname.trim() || null,
      tailscaleIp: next.tailscaleIp.trim() || null,
      lanIp: next.lanIp.trim() || null,
      mcpPort,
      mcpPath: next.mcpPath.trim() || null,
    };
  };
  const d = useDraft(saved, async (next) => {
    const endpoint = validatedEndpoint(next);
    await shell.applyConfig((api) =>
      api.saveToolServiceConfig({
        document: {
          ...service,
          display_name: next.displayName.trim() || null,
          description: next.description.trim() || null,
          hostname: endpoint.hostname,
          tailscale_ip: endpoint.tailscaleIp,
          lan_ip: endpoint.lanIp,
          mcp_port: endpoint.mcpPort,
          mcp_path: endpoint.mcpPath,
          send_agent_did: next.sendAgentDid,
          enabled: next.enabled,
          tags: fromLinesOrNull(next.tags),
        },
      }),
    );
  });
  const test = async () => {
    try {
      const endpoint = validatedEndpoint(d.draft);
      const result = await shell.api.testToolService({
        serviceId: service.service_id,
        ...endpoint,
      });
      toast(
        result.error
          ? `Test failed: ${result.error}`
          : `${result.status} · ${result.toolCount} tools`,
      );
    } catch (error) {
      toast(`Test failed: ${error instanceof Error ? error.message : String(error)}`);
    }
  };
  const id = (f: string) => `${service.service_id}-${f}`;
  return (
    <>
      <Group title={service.display_name ?? service.service_id}>
        <FactRow label="Service ID" mono>
          {service.service_id}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <AreaRow
          id={id("description")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          onCommit={d.commit}
          rows={2}
        />
        <TextRow
          id={id("host")}
          label="Hostname"
          value={d.draft.hostname}
          onChange={(v) => d.set("hostname", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("tailscale")}
          label="Tailscale IP"
          value={d.draft.tailscaleIp}
          onChange={(v) => d.set("tailscaleIp", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
        <TextRow
          id={id("lan")}
          label="LAN IP"
          value={d.draft.lanIp}
          onChange={(v) => d.set("lanIp", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
        <NumberRow
          id={id("port")}
          label="MCP port"
          value={d.draft.mcpPort}
          onChange={(v) => d.set("mcpPort", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("path")}
          label="MCP path"
          description="Empty uses the endpoint root; otherwise start with /."
          value={d.draft.mcpPath}
          onChange={(v) => d.set("mcpPath", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
        <SwitchRow
          id={id("send-agent")}
          label="Send agent DID"
          checked={d.draft.sendAgentDid}
          onChange={(v) => d.choose("sendAgentDid", v)}
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose("enabled", v)}
        />
        <AreaRow
          id={id("tags")}
          label="Tags"
          description="One per line."
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
          onCommit={d.commit}
          rows={3}
        />
      </Group>
      <div className="mb-3 flex justify-end">
        <Button variant="outline" onClick={test}>
          Test connection
        </Button>
      </div>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        onSave={d.save}
        onCancel={d.reset}
      />
      <DeleteButton
        label={service.display_name ?? service.service_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteToolServiceConfig({
              serviceId: service.service_id,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function ToolServicesPanel({
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
    section: "tool-services",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.toolServiceRegistries.map((s) => ({
        id: s.service_id,
        title: s.display_name ?? s.service_id,
        meta: s.hostname ?? "",
      }))}
      createLabel="New tool service"
      empty="No MCP tool services."
      onCreate={async () => {
        const service_id = newId("mcp");
        await shell.applyConfig((api) =>
          api.saveToolServiceConfig({
            document: {
              service_id,
              agent_did: deployment.agentDid,
              display_name: "New service",
              hostname: "127.0.0.1",
              mcp_port: 3333,
              enabled: false,
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "tool-services",
          item: service_id,
        });
      }}
      detail={(id) => {
        const service = deployment.toolServiceRegistries.find(
          (s) => s.service_id === id,
        )!;
        return (
          <Editor
            key={service.service_id}
            shell={shell}
            deployment={deployment}
            service={service}
          />
        );
      }}
    />
  );
}
