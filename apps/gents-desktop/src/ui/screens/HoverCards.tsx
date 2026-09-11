/* Delayed cards on avatars: what a reader most wants to know without
   opening anything. The agent: online, what it may touch, how much it
   has, its DID. A behaviour: what it is for, what it runs on, its access. */
import type { ReactElement } from 'react'
import type { DeploymentView } from '@source-inc/gents-desktop-client'
import { HoverCard, HoverCardContent, HoverCardTrigger } from '@gents/ui/components/hover-card'
import { cn } from '@gents/ui/lib/utils'
import { access, bashAccess, behaviorName, fileAccess, network } from './behavior'

function Line({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 truncate text-foreground">{children}</dd>
    </>
  )
}

const shortDid = (did: string) => (did.length > 22 ? `${did.slice(0, 12)}…${did.slice(-4)}` : did)

export function AgentHoverCard({
  deployment,
  root,
  ceiling,
  children,
}: {
  deployment: DeploymentView
  root?: string | null
  ceiling?: string | null
  children: ReactElement
}) {
  const online = deployment.dialSucceeded
  const env =
    deployment.behaviorEnvironments.find((e) => e.isDefault) ?? deployment.behaviorEnvironments[0]
  return (
    <HoverCard>
      <HoverCardTrigger delay={500} render={children} />
      <HoverCardContent side="right" align="start" className="w-80">
        <div className="flex items-baseline justify-between gap-3">
          <span className="font-heading text-lg font-medium text-heading">
            {deployment.agentPrincipal.displayName ?? deployment.label}
          </span>
          <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <span className={cn('size-2 rounded-full', online ? 'bg-brand' : 'bg-destructive')} />
            {online ? 'Online' : 'Offline'}
          </span>
        </div>
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
          <Line label="Can touch">{root ?? env?.workspaceRoot ?? '—'}</Line>
          <Line label="At most">
            {ceiling ? access(ceiling) : env ? access(env.fileAccess) : '—'}
            {env ? `, ${network(env.networkAccess)}` : ''}
          </Line>
          <Line label="Behaviours">{deployment.behaviors.length}</Line>
          <Line label="Tasks">{deployment.tasks.length}</Line>
          <Line label="Inference">{deployment.inferenceBackends.length}</Line>
        </dl>
        <p
          className="mt-3 truncate font-mono text-[11px] text-muted-foreground"
          title={deployment.agentDid}
        >
          {shortDid(deployment.agentDid)}
        </p>
      </HoverCardContent>
    </HoverCard>
  )
}

export function BehaviorHoverCard({
  deployment,
  behaviorId,
  description,
  children,
}: {
  deployment: DeploymentView | null
  behaviorId: string | null
  description?: string
  children: ReactElement
}) {
  const b = deployment?.behaviors.find((x) => x.behaviorId === behaviorId)
  const env = deployment?.behaviorEnvironments.find((e) => e.behaviorId === behaviorId)
  if (!b) return children
  return (
    <HoverCard>
      <HoverCardTrigger delay={500} render={children} />
      <HoverCardContent side="right" align="start" className="w-80">
        <div className="flex items-baseline justify-between gap-3">
          <span className="font-heading text-lg font-medium text-heading">
            {behaviorName(behaviorId, deployment)}
          </span>
          {b.isDefault && <span className="text-xs text-muted-foreground">Default</span>}
        </div>
        {description && <p className="mt-1.5 text-sm text-muted-foreground">{description}</p>}
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
          <Line label="Runs on">{env?.modelName ?? 'no backend'}</Line>
          <Line label="Files">{fileAccess(env?.fileAccess ?? 'off')}</Line>
          <Line label="Commands">{bashAccess(env?.bashAccess ?? 'off')}</Line>
          <Line label="Network">{network(env?.networkAccess)}</Line>
          <Line label="Sessions">{env?.sessionCount ?? 0}</Line>
        </dl>
      </HoverCardContent>
    </HoverCard>
  )
}
