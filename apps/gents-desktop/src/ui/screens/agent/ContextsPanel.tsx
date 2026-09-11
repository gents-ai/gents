import type { AgentContext, DeploymentView } from '@source-inc/gents-desktop-client'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { AreaRow, ChoiceRow, FactRow, TextRow } from './editors'
import { fromLines, newId, toLines, useDraft } from './draft'
import { DeleteButton, ListDetail } from './ListDetail'
import { Group } from './rows'

function Editor({
  shell,
  deployment,
  context,
}: {
  shell: Shell
  deployment: DeploymentView
  context: AgentContext
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'contexts' }
  const saved = {
    displayName: context.display_name ?? '',
    description: context.description ?? '',
    systemPrompt: context.system_prompt ?? '',
    toolsId: context.tools_id ?? '',
    compactionId: context.compaction_id ?? '',
    skillIds: toLines(context.skill_ids ?? []),
  }
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.patchConfigComponents({
        agentDid: deployment.agentDid,
        patches: [
          {
            collection: 'AgentContext',
            id: context.context_id,
            changes: {
              display_name: next.displayName || null,
              description: next.description || null,
              system_prompt: next.systemPrompt || null,
              tools_id: next.toolsId || null,
              compaction_id: next.compactionId || null,
              skill_ids: fromLines(next.skillIds),
            },
          },
        ],
      }),
    ),
  )
  const id = (f: string) => `${context.context_id}-${f}`
  return (
    <>
      <Group title={context.display_name ?? context.context_id}>
        <FactRow label="Context ID" mono>
          {context.context_id}
        </FactRow>
        <TextRow
          id={id('name')}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set('displayName', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <AreaRow
          id={id('prompt')}
          label="System prompt"
          value={d.draft.systemPrompt}
          onChange={(v) => d.set('systemPrompt', v)}
          onCommit={d.commit}
          rows={8}
        />
        <ChoiceRow
          id={id('tools')}
          label="Tools"
          value={d.draft.toolsId}
          onChange={(v) => d.choose('toolsId', v)}
          items={[
            { value: '', label: 'None' },
            ...deployment.tools.map((t) => ({
              value: t.tools_id,
              label: t.display_name ?? t.tools_id,
            })),
          ]}
        />
        <ChoiceRow
          id={id('compact')}
          label="Compaction"
          value={d.draft.compactionId}
          onChange={(v) => d.choose('compactionId', v)}
          items={[
            { value: '', label: 'Runtime default' },
            ...deployment.compactions.map((c) => ({
              value: c.compaction_id,
              label: c.display_name ?? c.compaction_id,
            })),
          ]}
        />
        <AreaRow
          id={id('skills')}
          label="Skill IDs"
          description="One per line. Empty means no skills."
          value={d.draft.skillIds}
          onChange={(v) => d.set('skillIds', v)}
          onCommit={d.commit}
          rows={3}
          mono
        />
      </Group>
      <DeleteButton
        label={context.display_name ?? context.context_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.applyConfigComponents({
              document: {
                agent_principal: { agent_did: deployment.agentDid },
                contexts: deployment.contexts.filter((c) => c.context_id !== context.context_id),
              },
            }),
          )
        }
      />
    </>
  )
}

export function ContextsPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell
  deployment: DeploymentView
  item?: string
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'contexts' }
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.contexts.map((c) => ({
        id: c.context_id,
        title: c.display_name ?? c.context_id,
        meta: c.tools_id ?? 'no tools',
      }))}
      createLabel="New context"
      empty="No contexts. A context is the prompt, tools and skills a behaviour runs with."
      onCreate={async () => {
        const context_id = newId('ctx')
        await shell.applyConfig((api) =>
          api.applyConfigComponents({
            document: {
              agent_principal: { agent_did: deployment.agentDid },
              contexts: [
                ...deployment.contexts,
                {
                  context_id,
                  agent_did: deployment.agentDid,
                  display_name: 'New context',
                  system_prompt: '',
                  tools_id: deployment.tools[0]?.tools_id ?? null,
                  compaction_id: deployment.compactions[0]?.compaction_id ?? null,
                  skill_ids: [],
                  tags: null,
                },
              ],
            },
          }),
        )
        navigate({
          name: 'agent',
          agentDid: deployment.agentDid,
          section: 'contexts',
          item: context_id,
        })
      }}
      detail={(id) => {
        const context = deployment.contexts.find((c) => c.context_id === id)!
        return (
          <Editor
            key={JSON.stringify(context)}
            shell={shell}
            deployment={deployment}
            context={context}
          />
        )
      }}
    />
  )
}
