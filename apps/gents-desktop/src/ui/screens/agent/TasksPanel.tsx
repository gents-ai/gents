/* Tasks, as the desktop app's Tasks tab: TaskSaveRequest fields, the
   run facts, and a manual run with JSON args. */
import { useState } from 'react'
import { toast } from 'sonner'
import type { DeploymentView, TaskView } from '@source-inc/gents-desktop-client'
import { Button } from '@gents/ui/components/button'
import { Textarea } from '@gents/ui/components/textarea'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { AreaRow, ChoiceRow, FactRow, NumberRow, SwitchRow, TextRow } from './editors'
import { intOrNull, str, useDraft } from './draft'
import { DeleteButton, ListDetail } from './ListDetail'
import { newId } from './draft'
import { Group, Row } from './rows'

const when = (iso: string | null | undefined) => (iso ? new Date(iso).toLocaleString() : '—')

function Editor({
  shell,
  deployment,
  task,
}: {
  shell: Shell
  deployment: DeploymentView
  task: TaskView
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'tasks' }
  const saved = {
    name: task.name ?? '',
    behaviorId: task.behaviorId ?? '',
    enabled: task.enabled ?? true,
    description: task.description ?? '',
    promptTemplate: task.promptTemplate ?? '',
    goalObjectiveTemplate: task.goalObjectiveTemplate ?? '',
    goalTokenBudget: str(task.goalTokenBudget),
    outputSchemaRef: task.outputSchemaRef ?? '',
  }
  const d = useDraft(saved, (n) => {
    if (n.goalTokenBudget.trim() && !n.goalObjectiveTemplate.trim()) {
      return Promise.reject(new Error('A goal budget needs a goal objective'))
    }
    return shell.applyConfig((api) =>
      api.saveTaskConfig({
        document: {
          agent_did: deployment.agentDid,
          task_id: task.taskId,
          display_name: n.name.trim() || task.taskId,
          description: n.description || null,
          behavior_id: n.behaviorId,
          prompt_template: n.promptTemplate,
          goal_objective_template: n.goalObjectiveTemplate || null,
          goal_token_budget: intOrNull(n.goalTokenBudget),
          enabled: n.enabled,
          output_schema_ref: n.outputSchemaRef || null,
          hooks: task.hooks,
          tags: task.tags,
        },
      }),
    )
  })
  const [args, setArgs] = useState('{}')
  const [lastRun, setLastRun] = useState<string | null>(null)
  const id = (f: string) => `${task.taskId}-${f}`
  const behaviours = deployment.behaviors.map((b) => ({
    value: b.behaviorId,
    label: b.displayName,
  }))
  const run = async () => {
    let parsed: unknown
    try {
      parsed = JSON.parse(args)
      if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error()
    } catch {
      toast('Args must be a JSON object')
      return
    }
    const r = await shell.api.runTask({ taskId: task.taskId, args: parsed })
    setLastRun(r.requestId)
    toast(`Task started · ${r.requestId}`)
    void shell.refreshSnapshot()
  }
  return (
    <>
      <Group title={task.name ?? task.taskId}>
        <FactRow label="Task ID" mono>
          {task.taskId}
        </FactRow>
        <TextRow
          id={id('name')}
          label="Name"
          value={d.draft.name}
          onChange={(v) => d.set('name', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id('behavior')}
          label="Behaviour"
          value={d.draft.behaviorId}
          onChange={(v) => d.choose('behaviorId', v)}
          items={behaviours}
          none="Unset"
        />
        <SwitchRow
          id={id('enabled')}
          label="Enabled"
          checked={d.draft.enabled}
          onChange={(v) => d.choose('enabled', v)}
        />
        <AreaRow
          id={id('desc')}
          label="Description"
          value={d.draft.description}
          onChange={(v) => d.set('description', v)}
          onCommit={d.commit}
          rows={2}
        />
        <AreaRow
          id={id('prompt')}
          label="Prompt template"
          value={d.draft.promptTemplate}
          onChange={(v) => d.set('promptTemplate', v)}
          onCommit={d.commit}
          rows={5}
        />
        <AreaRow
          id={id('goal')}
          label="Durable goal objective"
          description="Optional. Provisions this goal before the first request becomes runnable."
          value={d.draft.goalObjectiveTemplate}
          onChange={(v) => d.set('goalObjectiveTemplate', v)}
          onCommit={d.commit}
          rows={2}
        />
        <NumberRow
          id={id('budget')}
          label="Goal token budget"
          description="A positive whole number, or blank; needs an objective."
          value={d.draft.goalTokenBudget}
          onChange={(v) => d.set('goalTokenBudget', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          placeholder="Optional"
        />
        <TextRow
          id={id('schema')}
          label="Output schema ref"
          value={d.draft.outputSchemaRef}
          onChange={(v) => d.set('outputSchemaRef', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          mono
        />
      </Group>
      <Group title="Runs">
        <FactRow label="Total fires">{task.recentRuns.totalFires}</FactRow>
        <FactRow label="Last attempt">{when(task.recentRuns.lastAttemptAt)}</FactRow>
        <FactRow label="Last status">{task.recentRuns.lastStatus ?? '—'}</FactRow>
        <FactRow label="Last error">{task.recentRuns.lastError ?? '—'}</FactRow>
        <FactRow label="Wired to">
          {task.recentRuns.scheduleCount} schedules · {task.recentRuns.eventCount} events
        </FactRow>
        {task.runHistory.slice(0, 5).map((r) => (
          <FactRow
            key={r.requestId}
            label={<span className="font-mono text-xs">{r.requestId}</span>}
            description={`${r.causedByTriggerKind ?? 'manual'}${r.causedByTriggerId ? `:${r.causedByTriggerId}` : ''}`}
          >
            {r.lifecycleState ?? '—'}
          </FactRow>
        ))}
      </Group>
      <Group
        title="Manual run"
        action={
          <Button size="sm" variant="brand" onClick={run}>
            Run task
          </Button>
        }
      >
        <Row label="Args" description="Must be a JSON object." htmlFor={id('args')}>
          <Textarea
            id={id('args')}
            value={args}
            onChange={(e) => setArgs(e.target.value)}
            rows={3}
            className="w-96 max-md:w-full font-mono text-xs"
          />
        </Row>
        {lastRun && (
          <FactRow label="Started" mono>
            {lastRun}
          </FactRow>
        )}
      </Group>
      <DeleteButton
        label={task.name ?? task.taskId}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteTaskConfig({ taskId: task.taskId, agentDid: deployment.agentDid }),
          )
        }
      />
    </>
  )
}

export function TasksPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell
  deployment: DeploymentView
  item?: string
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'tasks' }
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.tasks.map((t) => ({
        id: t.taskId,
        title: t.name ?? t.taskId,
        meta: `${deployment.behaviors.find((b) => b.behaviorId === t.behaviorId)?.displayName ?? 'no behaviour'} · ${t.recentRuns.totalFires} fires${t.enabled === false ? ' · disabled' : ''}`,
        badge: t.recentRuns.lastStatus ?? undefined,
        badgeTone: t.recentRuns.lastStatus === 'failed' ? 'bad' : 'default',
      }))}
      createLabel="New task"
      empty="No tasks. A task is a prompt the agent runs on a schedule or a trigger."
      onCreate={async () => {
        const taskId = newId('task')
        await shell.applyConfig((api) =>
          api.saveTaskConfig({
            document: {
              agent_did: deployment.agentDid,
              task_id: taskId,
              display_name: 'New task',
              description: null,
              behavior_id:
                deployment.behaviors.find((b) => b.isDefault)?.behaviorId ??
                deployment.behaviors[0]?.behaviorId ??
                '',
              prompt_template: '',
              goal_objective_template: null,
              goal_token_budget: null,
              enabled: true,
              output_schema_ref: null,
            },
          }),
        )
        navigate({ name: 'agent', agentDid: deployment.agentDid, section: 'tasks', item: taskId })
      }}
      detail={(id) => {
        const task = deployment.tasks.find((t) => t.taskId === id)!
        return (
          <Editor key={JSON.stringify(task)} shell={shell} deployment={deployment} task={task} />
        )
      }}
    />
  )
}
