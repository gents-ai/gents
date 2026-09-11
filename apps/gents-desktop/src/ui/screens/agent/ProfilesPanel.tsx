import type { DeploymentView, InferenceProfile } from '@source-inc/gents-desktop-client'
import type { Shell } from '@/hooks/useShell'
import { navigate } from '@/lib/router'
import { ChoiceRow, FactRow, NumberRow, TextRow } from './editors'
import { newId, useDraft } from './draft'
import { DeleteButton, ListDetail } from './ListDetail'
import { Group } from './rows'

function Editor({
  shell,
  deployment,
  profile,
}: {
  shell: Shell
  deployment: DeploymentView
  profile: InferenceProfile
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'profiles' }
  const saved = {
    displayName: profile.display_name ?? '',
    backendId: profile.backend_id,
    modelName: profile.model_name,
    maxOutputTokens: profile.max_output_tokens != null ? String(profile.max_output_tokens) : '',
    reasoningEffort: profile.reasoning_effort ?? '',
  }
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveInferenceProfileConfig({
        document: {
          ...profile,
          display_name: next.displayName || null,
          backend_id: next.backendId,
          model_name: next.modelName,
          max_output_tokens: next.maxOutputTokens ? Number(next.maxOutputTokens) : null,
          reasoning_effort: (next.reasoningEffort || null) as InferenceProfile['reasoning_effort'],
        },
      }),
    ),
  )
  const id = (f: string) => `${profile.profile_id}-${f}`
  return (
    <>
      <Group title={profile.display_name ?? profile.profile_id}>
        <FactRow label="Profile ID" mono>
          {profile.profile_id}
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
          id={id('backend')}
          label="Backend"
          value={d.draft.backendId}
          onChange={(v) => d.choose('backendId', v)}
          items={deployment.inferenceBackends.map((b) => ({
            value: b.backendId,
            label: b.name ?? b.backendId,
          }))}
        />
        <TextRow
          id={id('model')}
          label="Model"
          value={d.draft.modelName}
          onChange={(v) => d.set('modelName', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id('maxout')}
          label="Max output tokens"
          value={d.draft.maxOutputTokens}
          onChange={(v) => d.set('maxOutputTokens', v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
      </Group>
      <DeleteButton
        label={profile.display_name ?? profile.profile_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteInferenceProfileConfig({
              profileId: profile.profile_id,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  )
}

export function ProfilesPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell
  deployment: DeploymentView
  item?: string
}) {
  const base = { name: 'agent' as const, agentDid: deployment.agentDid, section: 'profiles' }
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.inferenceProfiles.map((p) => ({
        id: p.profile_id,
        title: p.display_name ?? p.profile_id,
        meta: p.model_name,
      }))}
      createLabel="New profile"
      empty="No inference profiles."
      onCreate={async () => {
        const profile_id = newId('profile')
        const backend = deployment.inferenceBackends[0]
        if (!backend) throw new Error('Add a backend first')
        await shell.applyConfig((api) =>
          api.saveInferenceProfileConfig({
            document: {
              agent_did: deployment.agentDid,
              profile_id,
              display_name: 'New profile',
              backend_id: backend.backendId,
              model_name: backend.models[0] ?? 'model',
            },
          }),
        )
        navigate({
          name: 'agent',
          agentDid: deployment.agentDid,
          section: 'profiles',
          item: profile_id,
        })
      }}
      detail={(id) => {
        const profile = deployment.inferenceProfiles.find((p) => p.profile_id === id)!
        return (
          <Editor
            key={JSON.stringify(profile)}
            shell={shell}
            deployment={deployment}
            profile={profile}
          />
        )
      }}
    />
  )
}
