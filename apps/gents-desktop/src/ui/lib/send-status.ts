/* Whether a message can be sent right now, and why not, in the desktop's
   words (chat-shell's ChatBlockedReason and hintFor). The admission
   layers come first: client, agent, route, behaviour; then the turn. */
import type { DeploymentView } from '@source-inc/gents-desktop-client'

export type SendStatus = { kind: 'ready' } | { kind: 'disabled'; reason: string; hint: string }

export const READINESS_REASON: Record<string, string> = {
  behavior_disabled: 'behaviour is disabled',
  runtime_configuration_invalid: 'runtime configuration is invalid',
  backend_not_configured: 'no inference backend',
  backend_disabled: 'inference backend is disabled',
  backend_temporarily_unavailable: 'inference backend is unavailable right now',
  credentials_required: 'inference credentials are required',
  inference_profile_invalid: 'inference profile is invalid',
  tool_configuration_invalid: 'tool configuration is invalid',
  tool_surface_unavailable: 'tool surface is unavailable',
  executor_start_failed: 'executor failed to start',
}

export function behaviorReadiness(deployment: DeploymentView | null, behaviorId: string | null) {
  const entry = deployment?.behaviorReadiness.behaviors.find((b) => b.behaviorId === behaviorId)
  if (!entry) return { ready: true, reason: null as string | null }
  if (entry.state === 'ready') return { ready: true, reason: null }
  const word = 'reason' in entry ? String(entry.reason) : 'unknown'
  return { ready: false, reason: READINESS_REASON[word] ?? word.replace(/_/g, ' ') }
}

export function sendStatus(input: {
  clientOnline: boolean
  deployment: DeploymentView | null
  behaviorId: string | null
  sending: boolean
  inFlight: boolean
  turnState: string | null | undefined
}): SendStatus {
  if (!input.clientOnline)
    return { kind: 'disabled', reason: 'clientOffline', hint: 'Secure client is not running' }
  if (!input.deployment)
    return { kind: 'disabled', reason: 'agentNotSelected', hint: 'Select an agent before sending' }
  if (!input.deployment.dialSucceeded || !input.deployment.chatSafe)
    return {
      kind: 'disabled',
      reason: 'routeNotReady',
      hint: input.deployment.lastError
        ? `Secure route to the agent is not ready: ${input.deployment.lastError}`
        : 'Secure route to the agent is not ready',
    }
  const readiness = behaviorReadiness(input.deployment, input.behaviorId)
  if (!readiness.ready)
    return {
      kind: 'disabled',
      reason: 'behaviorUnavailable',
      hint: `The selected behaviour is unavailable: ${readiness.reason}`,
    }
  if (input.sending)
    return { kind: 'disabled', reason: 'submittingRequest', hint: 'Submitting request' }
  if (input.inFlight)
    return {
      kind: 'disabled',
      reason: 'awaitingTurnTerminality',
      hint:
        input.turnState === 'waitingForClaim'
          ? 'Waiting for the active turn to start'
          : 'Turn still streaming',
    }
  return { kind: 'ready' }
}
