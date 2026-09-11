import type { DeploymentView } from '@source-inc/gents-desktop-client'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'

/* a new behaviour with the agent's first backend and profile, then its
   settings page; ids are minted here, outside render */
export async function createBehavior(shell: Shell, deployment: DeploymentView) {
  const id = `behavior-${Date.now().toString(36)}`
  const profileId = deployment.inferenceProfiles[0]?.profile_id
  if (!profileId) throw new Error('Create an inference profile first')
  await shell.saveBehaviorConfig({
    document: {
      behavior_id: id,
      agent_did: deployment.agentDid,
      display_name: 'New behaviour',
      description: null,
      context_id: deployment.contexts[0]?.context_id ?? null,
      inference_profile_id: profileId,
      enabled: true,
      tags: null,
      created_at: null,
    },
  })
  navigate({ name: 'agent', agentDid: deployment.agentDid, section: 'behaviors', item: id })
}
