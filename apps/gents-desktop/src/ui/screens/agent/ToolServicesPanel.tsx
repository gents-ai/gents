import type { DeploymentView, ToolServiceRegistry } from '@source-inc/gents-desktop-client'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { FactRow, NumberRow, SwitchRow, TextRow } from './editors'
import { newId, useDraft } from './draft'
import { DeleteButton, ListDetail } from './ListDetail'
import { Group } from './rows'

function Editor({
  shell,
  deployment,
  service,
}: {
  shell: Shell
  deployment: DeploymentView
  service: ToolServiceRegistry
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'tool-services' }
  const saved = {
    displayName: service.display_name ?? '',
    hostname: service.hostname ?? '',
    mcpPort: service.mcp_port != null ? String(service.mcp_port) : '3333',
    mcpPath: service.mcp_path ?? '',
    enabled: service.enabled ?? true,
  }
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveToolServiceConfig({
        document: {
          ...service,
          display_name: next.displayName || null,
          hostname: next.hostname || null,
          mcp_port: Number(next.mcpPort) || null,
          mcp_path: next.mcpPath || null,
          enabled: next.enabled,
        },
      }),
    ),
  )
  const id = (f: string) => `${service.service_id}-${f}`
  return (
    <>
      <Group title={service.display_name ?? service.service_id}>
        <FactRow label="Service ID" mono>
          {service.service_id}
        </FactRow>
        <TextRow
          id={id('name')}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set('displayName', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id('host')}
          label="Hostname"
          value={d.draft.hostname}
          onChange={(v) => d.set('hostname', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id('port')}
          label="MCP port"
          value={d.draft.mcpPort}
          onChange={(v) => d.set('mcpPort', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <SwitchRow
          id={id('enabled')}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose('enabled', v)}
        />
      </Group>
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
  )
}

export function ToolServicesPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell
  deployment: DeploymentView
  item?: string
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'tool-services' }
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.toolServiceRegistries.map((s) => ({
        id: s.service_id,
        title: s.display_name ?? s.service_id,
        meta: s.hostname ?? '',
      }))}
      createLabel="New tool service"
      empty="No MCP tool services."
      onCreate={async () => {
        const service_id = newId('mcp')
        await shell.applyConfig((api) =>
          api.saveToolServiceConfig({
            document: {
              service_id,
              agent_did: deployment.agentDid,
              display_name: 'New service',
              enabled: true,
            },
          }),
        )
        navigate({
          name: 'agent',
          agentDid: deployment.agentDid,
          section: 'tool-services',
          item: service_id,
        })
      }}
      detail={(id) => {
        const service = deployment.toolServiceRegistries.find((s) => s.service_id === id)!
        return (
          <Editor
            key={JSON.stringify(service)}
            shell={shell}
            deployment={deployment}
            service={service}
          />
        )
      }}
    />
  )
}
