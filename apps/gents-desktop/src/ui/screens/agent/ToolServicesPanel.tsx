import type { NodeView } from "../../../hooks/fleetStore";
import type { ToolServiceRegistry } from "@source-inc/gents-desktop-client";
import { dependentsWarning } from "./dependents";
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
  TagsRow,
} from "./editors";
import { newId, problemOf, requiredInteger, useDraft, type Problems } from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { RowMenu } from "./RowMenu";
import { useApp } from "@/app/AppContext";

function Editor({
  deployment,
  service,
}: {
  deployment: NodeView;
  service: ToolServiceRegistry;
}) {
  const {
    actions: { changeConfig, testToolService },
  } = useApp();
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
    sendNodeDid: service.send_node_did ?? false,
    enabled: service.enabled ?? true,
    tags: service.tags ?? [],
  };
  const port = (next: typeof saved) =>
    requiredInteger("MCP port", next.mcpPort, { min: 1, max: 65_535 });
  /* what the save and the test both need of the address */
  const endpointProblems = (next: typeof saved): Problems<typeof saved> => ({
    hostname: [next.hostname, next.tailscaleIp, next.lanIp].some((v) => v.trim())
      ? undefined
      : "Hostname, Tailscale IP, or LAN IP is required",
    mcpPort: problemOf(() => port(next)),
    mcpPath:
      next.mcpPath && !next.mcpPath.startsWith("/")
        ? "MCP path must be empty or start with /"
        : undefined,
  });
  /* the address as the bridge takes it, once it has no problems */
  const endpointOf = (next: typeof saved) => ({
    hostname: next.hostname.trim() || null,
    tailscaleIp: next.tailscaleIp.trim() || null,
    lanIp: next.lanIp.trim() || null,
    mcpPort: port(next),
    mcpPath: next.mcpPath.trim() || null,
  });
  const d = useDraft(
    saved,
    async (next) => {
      const endpoint = endpointOf(next);
      await changeConfig("saveToolServiceConfig", {
        document: {
          ...service,
          display_name: next.displayName.trim() || null,
          description: next.description.trim() || null,
          hostname: endpoint.hostname,
          tailscale_ip: endpoint.tailscaleIp,
          lan_ip: endpoint.lanIp,
          mcp_port: endpoint.mcpPort,
          mcp_path: endpoint.mcpPath,
          send_node_did: next.sendNodeDid,
          enabled: next.enabled,
          tags: next.tags.length ? next.tags : null,
        },
      });
    },
    { problems: endpointProblems },
  );
  const test = async () => {
    const problem = Object.values(d.problems).find(Boolean);
    if (problem) {
      toast(`Test failed: ${problem}`);
      return;
    }
    try {
      const endpoint = endpointOf(d.draft);
      const result = await testToolService({
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
        />
        <AreaRow
          id={id("description")}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          rows={2}
        />
        <TextRow
          id={id("host")}
          label="Hostname"
          value={d.draft.hostname}
          error={d.problems.hostname}
          onChange={(v) => d.set("hostname", v)}
        />
        <TextRow
          id={id("tailscale")}
          label="Tailscale IP"
          value={d.draft.tailscaleIp}
          onChange={(v) => d.set("tailscaleIp", v)}
          mono
        />
        <TextRow
          id={id("lan")}
          label="LAN IP"
          value={d.draft.lanIp}
          onChange={(v) => d.set("lanIp", v)}
          mono
        />
        <NumberRow
          id={id("port")}
          label="MCP port"
          value={d.draft.mcpPort}
          error={d.problems.mcpPort}
          onChange={(v) => d.set("mcpPort", v)}
        />
        <TextRow
          id={id("path")}
          label="MCP path"
          description="Empty uses the endpoint root; otherwise start with /."
          value={d.draft.mcpPath}
          error={d.problems.mcpPath}
          onChange={(v) => d.set("mcpPath", v)}
          mono
        />
        <SwitchRow
          id={id("send-agent")}
          label="Send node DID"
          checked={d.draft.sendNodeDid}
          onChange={(v) => d.set("sendNodeDid", v)}
        />
        <SwitchRow
          id={id("enabled")}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.set("enabled", v)}
        />
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>
      <div className="mb-3 flex justify-end">
        <Button variant="outline" onClick={test}>
          Test connection
        </Button>
      </div>
      <DraftActions
        draft={d}
        fields={{ hostname: id("host"), mcpPort: id("port"), mcpPath: id("path") }}
      />
      <DeleteButton
        label={service.display_name ?? service.service_id}
        warning={dependentsWarning(deployment, "tool-service", service.service_id)}
        base={base}
        onDelete={() =>
          changeConfig("deleteToolServiceConfig", {
            serviceId: service.service_id,
            nodeDid: deployment.nodeDid,
          })
        }
      />
    </>
  );
}

export function ToolServicesPanel({
  deployment,
  item,
}: {
  deployment: NodeView;
  item?: string;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
        trailing: (
          <RowMenu
            name={s.display_name ?? s.service_id}
            base={base}
            id={s.service_id}
            onDelete={() =>
              changeConfig("deleteToolServiceConfig", {
                serviceId: s.service_id,
                nodeDid: deployment.nodeDid,
              })
            }
          />
        ),
      }))}
      createLabel="New remote tools"
      empty="No remote tools. Add an MCP connection, then select its tools in a Tools document."
      onCreate={async () => {
        const service_id = newId("mcp");
        await changeConfig("saveToolServiceConfig", {
          document: {
            service_id,
            node_did: deployment.nodeDid,
            display_name: "New service",
            hostname: "127.0.0.1",
            mcp_port: 3333,
            enabled: false,
          },
        });
        navigate({
          name: "agent",
          nodeDid: deployment.nodeDid,
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

            deployment={deployment}
            service={service}
          />
        );
      }}
    />
  );
}
