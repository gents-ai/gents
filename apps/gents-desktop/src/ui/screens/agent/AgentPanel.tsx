/* The agent itself (remounted by its parent whenever the saved record
   changes, so the draft starts from what is saved): what a person can
   change (display name, default behaviour, enabled) through
   AgentConfigSaveRequest, and the identity facts the bridge reports.
   Every change saves as it is made: text on blur or Enter, choices at
   once; a toast confirms. */
import { useState } from 'react'
import type { DeploymentView } from '@source-inc/gents-desktop-client'
import { Input } from '@gents/ui/components/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@gents/ui/components/select'
import { Switch } from '@gents/ui/components/switch'
import { toast } from 'sonner'
import type { Shell } from '@/hooks/useShell'
import { Fact, Group, Row } from './rows'
import { LocalServer } from './LocalServer'

export function AgentPanel({ shell, deployment }: { shell: Shell; deployment: DeploymentView }) {
  const agent = deployment.agentPrincipal
  const [displayName, setDisplayName] = useState(agent.displayName ?? '')
  const behaviours = deployment.behaviors.map((b) => ({
    value: b.behaviorId,
    label: b.displayName,
  }))

  const save = async (
    patch: Partial<{ displayName: string; defaultBehaviorId: string; enabled: boolean }>,
  ) => {
    const next = {
      displayName: agent.displayName ?? '',
      defaultBehaviorId: agent.defaultBehaviorId ?? '',
      enabled: agent.enabled ?? true,
      ...patch,
    }
    try {
      await shell.saveAgentConfig({
        document: {
          agent_did: agent.agentDid,
          display_name: next.displayName,
          default_behavior_id: next.defaultBehaviorId,
          enabled: next.enabled,
          created_at: agent.createdAt,
          created_by: agent.createdBy,
          tags: null,
        },
      })
      toast('Saved')
    } catch (e) {
      toast(`Save failed: ${e instanceof Error ? e.message : String(e)}`)
    }
  }
  const commitName = () => {
    const name = displayName.trim()
    if (name && name !== (agent.displayName ?? '')) void save({ displayName: name })
  }

  return (
    <div>
      <Group title="Agent details">
        <Row
          label="Display name"
          description="How this agent is named across the desktop."
          htmlFor="agent-name"
        >
          <Input
            id="agent-name"
            value={displayName}
            onChange={(e) => setDisplayName(e.target.value)}
            onBlur={commitName}
            onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
            className="w-72 max-md:w-full"
          />
        </Row>
        <Row
          label="Default behaviour"
          description="Used when a session does not choose one."
          htmlFor="agent-default"
        >
          <Select
            items={behaviours}
            value={agent.defaultBehaviorId ?? ''}
            onValueChange={(v) =>
              v && v !== agent.defaultBehaviorId && save({ defaultBehaviorId: v })
            }
          >
            <SelectTrigger id="agent-default" className="w-72 max-md:w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {behaviours.map((b) => (
                <SelectItem key={b.value} value={b.value}>
                  {b.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Row>
        <Row
          label="Enabled"
          description="A disabled agent accepts no requests."
          htmlFor="agent-enabled"
        >
          <Switch
            id="agent-enabled"
            checked={agent.enabled ?? true}
            onCheckedChange={(v) => save({ enabled: v })}
          />
        </Row>
      </Group>

      <Group title="Identity">
        <Row
          label="Agent DID"
          description="The cryptographic principal. Permissions and audit are keyed by it."
        >
          <Fact mono>{agent.agentDid}</Fact>
        </Row>
        <Row label="Install name">
          <Fact>{shell.snapshot?.bootstrap.initAgentName}</Fact>
        </Row>
        <Row label="Tool ceiling" description="The most any behaviour on this agent may do.">
          <Fact>{shell.snapshot?.bootstrap.initToolCeiling ?? 'not configured'}</Fact>
        </Row>
        <Row label="Tool root" description="The directory tools are confined to.">
          <Fact mono>{shell.snapshot?.bootstrap.initToolRoot ?? 'not configured'}</Fact>
        </Row>
        <Row label="Peer" description="Where the agent's node runs.">
          <Fact mono>{deployment.peerId}</Fact>
        </Row>
        <Row label="Created">
          <Fact>{agent.createdAt ? new Date(agent.createdAt).toLocaleDateString() : null}</Fact>
        </Row>
      </Group>

      <Group title="Runtime">
        <Row
          label="Reconcile"
          description="The last pass of the agent's runtime over its configuration."
        >
          <Fact>
            {deployment.runtime?.reconcilePhase} · {deployment.runtime?.lastReconcileResult}
          </Fact>
        </Row>
        <Row label="Executors" description="Behaviour executors in use over capacity.">
          <Fact>
            {deployment.runtime?.behaviorExecutorQueueDepth ?? 0} /{' '}
            {deployment.runtime?.behaviorExecutorCapacity ?? 0}
          </Fact>
        </Row>
      </Group>
      {deployment.source === 'local' && <LocalServer shell={shell} />}
    </div>
  )
}
