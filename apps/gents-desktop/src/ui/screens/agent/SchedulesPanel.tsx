import type { DeploymentView, Schedule } from '@source-inc/gents-desktop-client'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { FactRow, TextRow } from './editors'
import { newId, useDraft } from './draft'
import { DeleteButton, ListDetail } from './ListDetail'
import { Group } from './rows'

function cadenceLabel(s: Schedule) {
  return s.cadence.kind === 'cron' ? s.cadence.expression : `every ${s.cadence.interval_secs}s`
}

function Editor({
  shell,
  deployment,
  schedule,
}: {
  shell: Shell
  deployment: DeploymentView
  schedule: Schedule
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'schedules' }
  const saved = {
    displayName: schedule.display_name ?? '',
    expression: schedule.cadence.kind === 'cron' ? schedule.cadence.expression : '',
    timezone: schedule.cadence.kind === 'cron' ? schedule.cadence.timezone : 'UTC',
  }
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveScheduleConfig({
        document: {
          ...schedule,
          display_name: next.displayName || null,
          cadence: {
            kind: 'cron',
            expression: next.expression,
            timezone: next.timezone,
            missed_run_policy: 'latest_only',
          },
        },
      }),
    ),
  )
  const id = (f: string) => `${schedule.schedule_id}-${f}`
  return (
    <>
      <Group title={schedule.display_name ?? schedule.schedule_id}>
        <FactRow label="Schedule ID" mono>
          {schedule.schedule_id}
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
          id={id('cron')}
          label="Cron"
          value={d.draft.expression}
          onChange={(v) => d.set('expression', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id('tz')}
          label="Timezone"
          value={d.draft.timezone}
          onChange={(v) => d.set('timezone', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
      </Group>
      <DeleteButton
        label={schedule.display_name ?? schedule.schedule_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteScheduleConfig({
              scheduleId: schedule.schedule_id,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  )
}

export function SchedulesPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell
  deployment: DeploymentView
  item?: string
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'schedules' }
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.schedules.map((s) => ({
        id: s.schedule_id,
        title: s.display_name ?? s.schedule_id,
        meta: cadenceLabel(s),
      }))}
      createLabel="New schedule"
      empty="No schedules. A trigger binds a task to a schedule."
      onCreate={async () => {
        const schedule_id = newId('sched')
        await shell.applyConfig((api) =>
          api.saveScheduleConfig({
            document: {
              agent_did: deployment.agentDid,
              schedule_id,
              display_name: 'New schedule',
              cadence: { kind: 'cron', expression: '0 * * * *', timezone: 'UTC' },
            },
          }),
        )
        navigate({
          name: 'agent',
          agentDid: deployment.agentDid,
          section: 'schedules',
          item: schedule_id,
        })
      }}
      detail={(id) => {
        const schedule = deployment.schedules.find((s) => s.schedule_id === id)!
        return (
          <Editor
            key={JSON.stringify(schedule)}
            shell={shell}
            deployment={deployment}
            schedule={schedule}
          />
        )
      }}
    />
  )
}
