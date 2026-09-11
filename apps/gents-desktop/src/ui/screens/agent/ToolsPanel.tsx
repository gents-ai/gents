import type { DeploymentView, Tools } from '@source-inc/gents-desktop-client'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { ChoiceRow, FactRow, TextRow } from './editors'
import { newId, useDraft } from './draft'
import { DeleteButton, ListDetail } from './ListDetail'
import { Group } from './rows'

function Editor({
  shell,
  deployment,
  tools,
}: {
  shell: Shell
  deployment: DeploymentView
  tools: Tools
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'tools' }
  const saved = {
    displayName: tools.display_name ?? '',
    root: tools.host?.root ?? '',
    files: (tools.host?.files?.mode ?? 'Off') as 'Off' | 'ReadOnly' | 'ReadWrite',
    bash: (tools.host?.bash?.mode ?? 'Off') as 'Off' | 'ReadOnly' | 'Unrestricted',
  }
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveToolsConfig({
        document: {
          ...tools,
          display_name: next.displayName || null,
          host: {
            ...(tools.host ?? {}),
            root: next.root || null,
            files: {
              mode: next.files as NonNullable<NonNullable<Tools['host']>['files']>['mode'],
              timeout_secs: null,
            },
            bash: {
              ...(tools.host?.bash ?? {}),
              mode: next.bash as NonNullable<NonNullable<Tools['host']>['bash']>['mode'],
            },
          },
        },
      }),
    ),
  )
  const id = (f: string) => `${tools.tools_id}-${f}`
  return (
    <>
      <Group title={tools.display_name ?? tools.tools_id}>
        <FactRow label="Tools ID" mono>
          {tools.tools_id}
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
          id={id('root')}
          label="Workspace root"
          value={d.draft.root}
          onChange={(v) => d.set('root', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id('files')}
          label="Files"
          value={d.draft.files}
          onChange={(v) => d.choose('files', v as 'Off' | 'ReadOnly' | 'ReadWrite')}
          items={[
            { value: 'Off', label: 'Off' },
            { value: 'ReadOnly', label: 'Read only' },
            { value: 'ReadWrite', label: 'Read / write' },
          ]}
        />
        <ChoiceRow
          id={id('bash')}
          label="Bash"
          value={d.draft.bash}
          onChange={(v) => d.choose('bash', v as 'Off' | 'ReadOnly' | 'Unrestricted')}
          items={[
            { value: 'Off', label: 'Off' },
            { value: 'ReadOnly', label: 'Read only' },
            { value: 'Unrestricted', label: 'Unrestricted' },
          ]}
        />
      </Group>
      <DeleteButton
        label={tools.display_name ?? tools.tools_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteToolsConfig({ toolsId: tools.tools_id, agentDid: deployment.agentDid }),
          )
        }
      />
    </>
  )
}

export function ToolsPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell
  deployment: DeploymentView
  item?: string
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'tools' }
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.tools.map((t) => ({
        id: t.tools_id,
        title: t.display_name ?? t.tools_id,
        meta: t.host?.files?.mode ?? 'no host tools',
      }))}
      createLabel="New tools"
      empty="No Tools documents. A behaviour reaches tools only through its context."
      onCreate={async () => {
        const tools_id = newId('tools')
        await shell.applyConfig((api) =>
          api.saveToolsConfig({
            document: {
              tools_id,
              agent_did: deployment.agentDid,
              display_name: 'New tools',
              host: { files: { mode: 'ReadOnly' }, bash: { mode: 'Off' } },
              tags: null,
            },
          }),
        )
        navigate({ name: 'agent', agentDid: deployment.agentDid, section: 'tools', item: tools_id })
      }}
      detail={(id) => {
        const tools = deployment.tools.find((t) => t.tools_id === id)!
        return (
          <Editor key={JSON.stringify(tools)} shell={shell} deployment={deployment} tools={tools} />
        )
      }}
    />
  )
}
