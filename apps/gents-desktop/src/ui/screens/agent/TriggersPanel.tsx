import type { DeploymentView, TriggerView } from '@source-inc/gents-desktop-client'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { ChoiceRow, FactRow, SwitchRow, TextRow } from './editors'
import { newId, useDraft } from './draft'
import { DeleteButton, ListDetail } from './ListDetail'
import { Group } from './rows'

function Editor({
  shell,
  deployment,
  trigger,
}: {
  shell: Shell
  deployment: DeploymentView
  trigger: TriggerView
}) {
  const cfg = trigger.config
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'triggers' }
  const saved = {
    displayName: cfg.display_name ?? '',
    taskId: cfg.task_id,
    enabled: cfg.enabled ?? true,
    sourceKind: cfg.source.kind,
    sourceId: cfg.source.kind === 'schedule' ? cfg.source.schedule_id : cfg.source.event_source_id,
  }
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveTriggerConfig({
        document: {
          ...cfg,
          display_name: next.displayName || null,
          task_id: next.taskId,
          enabled: next.enabled,
          source:
            next.sourceKind === 'schedule'
              ? { kind: 'schedule', schedule_id: next.sourceId }
              : { kind: 'event', event_source_id: next.sourceId },
        },
      }),
    ),
  )
  const id = (f: string) => `${cfg.trigger_id}-${f}`
  return (
    <>
      <Group title={cfg.display_name ?? cfg.trigger_id}>
        <FactRow label="Trigger ID" mono>
          {cfg.trigger_id}
        </FactRow>
        <TextRow
          id={id('name')}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set('displayName', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id('task')}
          label="Task"
          value={d.draft.taskId}
          onChange={(v) => d.choose('taskId', v)}
          items={deployment.tasks.map((t) => ({
            value: t.taskId,
            label: t.name ?? t.taskId,
          }))}
        />
        <ChoiceRow
          id={id('kind')}
          label="Source"
          value={d.draft.sourceKind}
          onChange={(v) => d.choose('sourceKind', v as 'schedule' | 'event')}
          items={[
            { value: 'schedule', label: 'Schedule' },
            { value: 'event', label: 'Event source' },
          ]}
        />
        <ChoiceRow
          id={id('sid')}
          label={d.draft.sourceKind === 'schedule' ? 'Schedule' : 'Event source'}
          value={d.draft.sourceId}
          onChange={(v) => d.choose('sourceId', v)}
          items={
            d.draft.sourceKind === 'schedule'
              ? deployment.schedules.map((s) => ({
                  value: s.schedule_id,
                  label: s.display_name ?? s.schedule_id,
                }))
              : deployment.eventSources.map((s) => ({
                  value: s.event_source_id,
                  label: s.display_name ?? s.event_source_id,
                }))
          }
        />
        <SwitchRow
          id={id('enabled')}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose('enabled', v)}
        />
      </Group>
      <DeleteButton
        label={cfg.display_name ?? cfg.trigger_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteTriggerConfig({ triggerId: cfg.trigger_id, agentDid: deployment.agentDid }),
          )
        }
      />
    </>
  )
}

export function TriggersPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell
  deployment: DeploymentView
  item?: string
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'triggers' }
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.triggers.map((t) => ({
        id: t.config.trigger_id,
        title: t.config.display_name ?? t.config.trigger_id,
        meta: t.config.source.kind,
      }))}
      createLabel="New trigger"
      empty="No triggers. A trigger fires a task from a schedule or event source."
      onCreate={async () => {
        const trigger_id = newId('trig')
        const task = deployment.tasks[0]
        const schedule = deployment.schedules[0]
        if (!task || !schedule) throw new Error('Add a task and schedule first')
        await shell.applyConfig((api) =>
          api.saveTriggerConfig({
            document: {
              agent_did: deployment.agentDid,
              trigger_id,
              display_name: 'New trigger',
              task_id: task.taskId,
              source: { kind: 'schedule', schedule_id: schedule.schedule_id },
              enabled: true,
            },
          }),
        )
        navigate({
          name: 'agent',
          agentDid: deployment.agentDid,
          section: 'triggers',
          item: trigger_id,
        })
      }}
      detail={(id) => {
        const trigger = deployment.triggers.find((t) => t.config.trigger_id === id)!
        return (
          <Editor
            key={JSON.stringify(trigger)}
            shell={shell}
            deployment={deployment}
            trigger={trigger}
          />
        )
      }}
    />
  )
}
