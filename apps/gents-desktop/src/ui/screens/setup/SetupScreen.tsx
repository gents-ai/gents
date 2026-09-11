/* First run, from the Startup designs: choose where the agent lives,
   name it, watch it come online, then set up inference. Every step
   calls the bridge the way the desktop app does: initLocalStandardRuntime,
   requestStatusEnrollment, probeInferenceEndpoint, the provider logins
   and saveBackendConfig. */
import { useEffect, useState } from 'react'
import {
  ArrowLeft,
  ArrowRight,
  Circle,
  CircleCheck,
  KeyRound,
  Moon,
  Orbit,
  Server,
  Sparkles,
  Sun,
  Wifi,
} from 'lucide-react'
import type { DesktopClientSnapshot } from '@source-inc/gents-desktop-client'
import { Button } from '@gents/ui/components/button'
import { Input } from '@gents/ui/components/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@gents/ui/components/select'
import { Spinner } from '@gents/ui/components/spinner'
import { cn } from '@gents/ui/lib/utils'
import { ScrollArea } from '@gents/ui/components/scroll-area'
import {
  projectStartupLoadingStatus,
  type DesktopStartupPhase,
  type LoadingStepState,
} from '../../../lib/loadingStatus'
import type { Shell } from '@/hooks/useShell'
import { isMobileTauriShell } from '../../../lib/shellPlatform'
import { AgentAvatar } from '@/screens/AgentAvatar'
import { applyTheme, themePreference } from '@/theme'

type Step = 'welcome' | 'remote' | 'agent' | 'starting' | 'inference'

/* the desktop app's five inference options; the icons are stand-ins for the provider marks */
const PROVIDERS = [
  {
    id: 'local',
    title: 'Local server',
    hint: 'Ollama, llama.cpp or any OpenAI-compatible server on this machine.',
    icon: Server,
    logo: undefined,
  },
  {
    id: 'openai',
    title: 'OpenAI API key',
    hint: 'Your own key, billed to you.',
    icon: KeyRound,
    logo: '/logos/openai.svg',
  },
  {
    id: 'codex',
    title: 'ChatGPT / Codex',
    hint: 'Sign in with an eligible ChatGPT subscription.',
    icon: Sparkles,
    logo: '/logos/codex.svg',
  },
  {
    id: 'grok',
    title: 'Grok',
    hint: 'Sign in with SuperGrok or X Premium+.',
    icon: Orbit,
    logo: '/logos/grok.svg',
  },
] as const
type ProviderId = (typeof PROVIDERS)[number]['id']

function Frame({ children }: { children: React.ReactNode }) {
  const [theme, setTheme] = useState(themePreference)
  const flip = () => {
    const next = theme === 'dark' ? 'light' : 'dark'
    applyTheme(next)
    setTheme(next)
  }
  return (
    <ScrollArea
      className="viewport-frame relative bg-background text-foreground"
      data-testid="setup-screen"
    >
      <div className="px-8">
        {/* anchored a fixed way down, not centred: a step can grow or shrink without moving its title */}
        <div className="mx-auto w-full max-w-xl pt-[22vh] pb-16">{children}</div>
        <Button
          variant="ghost"
          size="icon-sm"
          className="absolute right-6 bottom-6"
          aria-label="Toggle theme"
          onClick={flip}
        >
          {theme === 'dark' ? <Sun /> : <Moon />}
        </Button>
      </div>
    </ScrollArea>
  )
}

function Title({ children, note }: { children: React.ReactNode; note: string }) {
  return (
    <div className="mb-6">
      <h1 className="font-heading text-2xl font-medium text-heading">{children}</h1>
      <p className="mt-1.5 text-sm text-muted-foreground">{note}</p>
    </div>
  )
}

/* a choice card: radio dot, title, mark on the right; selected carries the brand ring */
function Option({
  selected,
  onSelect,
  title,
  hint,
  icon: Icon,
  logo,
}: {
  selected: boolean
  onSelect: () => void
  title: string
  hint?: string
  icon: typeof Server
  /** the provider's own mark, in place of the generic icon */
  logo?: string
}) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      onClick={onSelect}
      className={cn(
        'flex w-full items-center gap-3 rounded-2xl border bg-raised px-4 py-3.5 text-left transition-shadow',
        selected ? 'border-brand ring-1 ring-brand' : 'border-border/60 hover:bg-accent',
      )}
    >
      {selected ? (
        <CircleCheck className="size-4 shrink-0 text-foreground" />
      ) : (
        <Circle className="size-4 shrink-0 text-muted-foreground" />
      )}
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium">{title}</span>
        {hint && <span className="block truncate text-xs text-muted-foreground">{hint}</span>}
      </span>
      {logo ? (
        <img src={logo} alt="" className="size-5 shrink-0 object-contain dark:invert" />
      ) : (
        <Icon className="size-5 shrink-0 text-heading" />
      )}
    </button>
  )
}

function Nav({
  onBack,
  next,
  nextLabel = 'Next',
  busy,
  disabled,
}: {
  onBack?: () => void
  next?: () => void
  nextLabel?: string
  busy?: boolean
  disabled?: boolean
}) {
  return (
    <div className="mt-8 flex items-center justify-between">
      {onBack ? (
        <button
          type="button"
          onClick={onBack}
          className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="size-3.5" /> Back
        </button>
      ) : (
        <span />
      )}
      {next && (
        <Button
          variant="brand"
          onClick={next}
          disabled={disabled || busy}
          data-testid="setup-next"
        >
          {busy ? <Spinner /> : null} {nextLabel} <ArrowRight />
        </Button>
      )}
    </div>
  )
}

const stepIcon = (state: LoadingStepState | null) =>
  state === 'complete' ? (
    <CircleCheck className="size-4 text-muted-foreground" />
  ) : state === 'active' ? (
    <Spinner className="text-foreground" />
  ) : (
    <span className="size-1.5 rounded-full bg-border" />
  )

export function SetupScreen({
  shell,
  onDone,
}: {
  shell: Shell
  onDone: (snapshot: DesktopClientSnapshot) => void
}) {
  const [step, setStep] = useState<Step>('welcome')
  const allowLocal = !isMobileTauriShell()
  const [where, setWhere] = useState<'local' | 'remote'>(allowLocal ? 'local' : 'remote')
  const [address, setAddress] = useState('')
  const [name, setName] = useState('Forge')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [phase, setPhase] =
    useState<Exclude<DesktopStartupPhase, 'ready'>>('checking-managed-server')
  const [provider, setProvider] = useState<ProviderId>('local')
  const [apiKey, setApiKey] = useState('')
  const [model, setModel] = useState('')
  const [probe, setProbe] = useState<{
    status: 'idle' | 'probing' | 'found' | 'none'
    url: string
    models: string[]
  }>({ status: 'idle', url: '', models: [] })
  const [signedIn, setSignedIn] = useState<string | null>(null)
  const api = shell.api
  const root = shell.snapshot?.bootstrap.defaultAgentHome ?? '~/.gents'

  /* the agent comes online: the desktop app's startup phases, then the client starts */
  useEffect(() => {
    if (step !== 'starting') return
    let cancelled = false
    const run = async () => {
      setPhase('checking-managed-server')
      await new Promise((r) => setTimeout(r, 900))
      if (cancelled) return
      setPhase('loading-configuration')
      await new Promise((r) => setTimeout(r, 900))
      if (cancelled) return
      setPhase('starting-client')
      try {
        await api.startDesktopClient()
        const snapshot = await api.fetchDesktopSnapshot()
        if (!cancelled) {
          if (snapshot.client?.deployments[0]?.inferenceBackends.length) onDone(snapshot)
          else setStep('inference')
        }
      } catch (e) {
        if (!cancelled) {
          setPhase('client-error')
          setError(e instanceof Error ? e.message : String(e))
        }
      }
    }
    void run()
    return () => {
      cancelled = true
    }
  }, [step, api, onDone])

  /* the local server probe runs when that option is chosen, as in the wizard */
  useEffect(() => {
    if (step !== 'inference' || provider !== 'local' || probe.status !== 'idle') return
    void (async () => {
      await Promise.resolve()
      setProbe({ status: 'probing', url: '', models: [] })
      for (const url of ['http://127.0.0.1:8080/v1', 'http://127.0.0.1:11434/v1']) {
        const result = await api.probeInferenceEndpoint(url).catch(() => null)
        if (result?.reachable && result.models.length) {
          setProbe({ status: 'found', url, models: result.models })
          setModel((m) => m || result.models[0]!)
          return
        }
      }
      setProbe({ status: 'none', url: '', models: [] })
    })()
  }, [step, provider, probe.status, api])

  const createAgent = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.initLocalStandardRuntime({
        label: name.trim() || 'Local Agent',
        dangerouslyOverwrite: false,
        reset: false,
      })
      setStep('starting')
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }
  const enrol = async () => {
    setBusy(true)
    setError(null)
    try {
      await api.requestStatusEnrollment(address.trim())
      setStep('starting')
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }
  const signIn = async () => {
    setBusy(true)
    try {
      const agentDid = shell.snapshot?.client?.deployments[0]?.agentDid ?? ''
      const result =
        provider === 'codex' ? await api.codexLogin(agentDid) : await api.grokLogin(agentDid)
      setSignedIn(result.credentialId)
    } finally {
      setBusy(false)
    }
  }
  const saveInference = async () => {
    setBusy(true)
    setError(null)
    try {
      const deployment =
        shell.snapshot?.client?.deployments[0] ??
        (await api.fetchDesktopSnapshot()).client?.deployments[0]
      if (!deployment) throw new Error('No agent to configure')
      const backend =
        provider === 'openai'
          ? {
              backendId: 'openai',
              name: 'OpenAI',
              providerKind: 'OpenAiCompatible',
              openaiWireApi: 'responses',
              endpoint: 'https://api.openai.com/v1',
              apiKey,
              models: [model || 'gpt-5.4-mini'],
            }
          : provider === 'codex'
            ? {
                backendId: 'chatgpt-codex',
                name: 'ChatGPT / Codex',
                providerKind: 'ChatGptCodex',
                openaiWireApi: 'responses',
                endpoint: 'https://chatgpt.com/backend-api/codex',
                apiKey: null,
                models: ['gpt-5.5'],
              }
            : provider === 'grok'
              ? {
                  backendId: 'grok',
                  name: 'Grok',
                  providerKind: 'XaiGrokOAuth',
                  openaiWireApi: 'chat_completions',
                  endpoint: 'https://cli-chat-proxy.grok.com/v1',
                  apiKey: null,
                  models: ['grok-4.5'],
                }
              : {
                  backendId: 'local',
                  name: 'Local server',
                  providerKind: 'OpenAiCompatible',
                  openaiWireApi: 'chat_completions',
                  endpoint: probe.url || 'http://127.0.0.1:11434/v1',
                  apiKey: null,
                  models: [model || probe.models[0] || 'gents-7b'],
                }
      await api.saveBackendConfig({
        document: {
          agent_did: deployment.agentDid,
          backend_id: backend.backendId,
          name: backend.name,
          provider_kind: backend.providerKind as
            | 'OpenAiCompatible'
            | 'OpenRouter'
            | 'ChatGptCodex'
            | 'XaiGrokOAuth'
            | 'ClaudeCliSubscription',
          openai_wire_api: backend.openaiWireApi as 'responses' | 'chat_completions',
          endpoint: backend.endpoint,
          auth: backend.apiKey
            ? { kind: 'api_key', key: backend.apiKey }
            : { kind: 'unauthenticated' },
          max_concurrent: 2,
          max_queue_depth: 8,
          enabled: true,
        },
      })
      const profileId = `profile-${backend.backendId}`
      await api.saveInferenceProfileConfig({
        document: {
          agent_did: deployment.agentDid,
          profile_id: profileId,
          display_name: backend.name,
          backend_id: backend.backendId,
          model_name: backend.models[0] ?? 'model',
        },
      })
      for (const b of deployment.behaviors) {
        if (!b.inferenceProfileId) {
          await api.saveBehaviorConfig({
            document: {
              behavior_id: b.behaviorId,
              agent_did: deployment.agentDid,
              display_name: b.displayName,
              description: b.description,
              context_id: b.contextId,
              inference_profile_id: profileId,
              enabled: b.enabled,
              tags: b.tags,
              created_at: b.createdAt,
            },
          })
        }
      }
      onDone(await api.fetchDesktopSnapshot())
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
    }
  }

  if (step === 'welcome') {
    return (
      <Frame>
        <Title note="Gents runs agents whose every step is a document. Start one here, or connect to one that already runs.">
          Let’s get set up
        </Title>
        <div className="grid gap-3">
          {allowLocal && (
            <Option
              selected={where === 'local'}
              onSelect={() => setWhere('local')}
              title="Local agent"
              hint="Create an agent on this Mac."
              icon={Server}
            />
          )}
          <Option
            selected={where === 'remote'}
            onSelect={() => setWhere('remote')}
            title="Remote connect"
            hint="Join a Gents server someone else runs."
            icon={Wifi}
          />
        </div>
        <Nav next={() => setStep(where === 'local' ? 'agent' : 'remote')} />
      </Frame>
    )
  }
  if (step === 'remote') {
    return (
      <Frame>
        <Title note="The server's status address. Its admin approves the enrolment.">
          Connect to a server
        </Title>
        <Input
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          placeholder="https://gents.example.net:8787"
          autoFocus
        />
        {error && <p className="mt-3 text-sm text-destructive">{error}</p>}
        <Nav
          onBack={() => setStep('welcome')}
          next={enrol}
          nextLabel="Request access"
          busy={busy}
          disabled={!address.trim()}
        />
      </Frame>
    )
  }
  if (step === 'agent') {
    return (
      <Frame>
        <div className="mb-4 flex items-center gap-1">
          <AgentAvatar name={name} className="size-9" />
        </div>
        <Title note="Its name is how it appears everywhere; its home is where its documents live.">
          Configure your agent
        </Title>
        <div className="rounded-2xl border border-border/60 bg-raised p-4">
          <Input
            value={name}
            onChange={(e) => setName(e.target.value)}
            aria-label="Agent name"
            autoFocus
          />
          <p className="mt-3 font-mono text-xs text-muted-foreground">
            root: <span className="text-foreground">{root}</span>
          </p>
        </div>
        {error && <p className="mt-3 text-sm text-destructive">{error}</p>}
        <Nav
          onBack={() => setStep('welcome')}
          next={createAgent}
          nextLabel="Start"
          busy={busy}
          disabled={!name.trim()}
        />
      </Frame>
    )
  }
  if (step === 'starting') {
    const status = projectStartupLoadingStatus(phase, true)
    const steps: [string, LoadingStepState | null][] = [
      ['Restore hosted agent', status.managedServerState],
      ['Read saved connections', status.connectionState],
      ['Start secure client', status.clientState],
    ]
    /* the line under the title, in the voice of the design */
    const saying: Record<string, string> = {
      'checking-managed-server': 'Waking the hosted agent…',
      'loading-configuration': 'Teaching the gossip network some manners…',
      'starting-client': 'Turning the secure client on…',
    }
    return (
      <Frame>
        <h1 className="font-heading text-2xl font-medium text-heading">
          {status.failed ? status.title : 'Startup'}
        </h1>
        <p className="mt-3 flex items-center gap-2 text-sm text-muted-foreground">
          {status.failed ? null : <Spinner className="text-foreground" />}
          {saying[phase] ?? status.currentLabel}
        </p>
        <ol className="mt-6 grid gap-2">
          {steps.map(([label, state], i) =>
            /* cards appear as steps become active, in the design's numbered style */
            state === 'pending' ? null : (
              <li
                key={label}
                className="flex items-center gap-3 rounded-2xl border border-border/60 bg-raised px-4 py-3 text-sm animate-in fade-in-0 slide-in-from-bottom-1 duration-300 fill-mode-both"
              >
                <span className="font-mono text-[11px] text-muted-foreground">0{i + 1}.</span>
                <span className="flex-1">{label}</span>
                {stepIcon(state)}
              </li>
            ),
          )}
        </ol>
        {error && <p className="mt-3 text-sm text-destructive">{error}</p>}
      </Frame>
    )
  }
  return (
    <Frame>
      <Title note="Connect and configure inference. The desktop app supports these; a behaviour can use a different one later.">
        Let’s get set up
      </Title>
      <div className="grid grid-cols-2 gap-3" role="radiogroup" aria-label="Inference provider">
        {PROVIDERS.map((p) => (
          <Option
            key={p.id}
            selected={provider === p.id}
            onSelect={() => {
              setProvider(p.id)
              setSignedIn(null)
              setModel('')
            }}
            title={p.title}
            icon={p.icon}
            logo={p.logo}
          />
        ))}
      </div>
      {/* as tall as its tallest variant, the two-field key form, so Next never moves */}
      <div className="mt-4 grid min-h-40 content-center rounded-2xl border border-border/60 bg-raised p-4 text-sm">
        {provider === 'local' &&
          (probe.status === 'found' ? (
            <div className="grid gap-2">
              <p className="text-muted-foreground">
                Found a server at <span className="font-mono text-foreground">{probe.url}</span>
              </p>
              <label className="grid gap-1">
                <span className="text-xs text-muted-foreground">Model</span>
                <Select
                  items={probe.models.map((m) => ({ value: m, label: m }))}
                  value={model}
                  onValueChange={(v) => v && setModel(v)}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {probe.models.map((m) => (
                      <SelectItem key={m} value={m}>
                        {m}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </label>
            </div>
          ) : probe.status === 'none' ? (
            <p className="text-muted-foreground">
              No server answered on the usual ports. Start one and{' '}
              <button
                type="button"
                className="underline"
                onClick={() => setProbe({ status: 'idle', url: '', models: [] })}
              >
                try again
              </button>
              .
            </p>
          ) : (
            <p className="flex items-center gap-2 text-muted-foreground">
              <Spinner className="text-foreground" /> Looking for a local server…
            </p>
          ))}
        {provider === 'openai' && (
          <div className="grid gap-3">
            <label className="grid gap-1">
              <span className="text-xs text-muted-foreground">API key</span>
              <Input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="sk-…"
              />
            </label>
            <label className="grid gap-1">
              <span className="text-xs text-muted-foreground">Model</span>
              <Input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="gpt-5.4-mini"
              />
            </label>
          </div>
        )}
        {(provider === 'codex' || provider === 'grok') &&
          (signedIn ? (
            <p className="flex items-center gap-2">
              <CircleCheck className="size-4 text-muted-foreground" /> Signed in · credential{' '}
              <span className="font-mono text-xs">{signedIn}</span>
            </p>
          ) : (
            <div className="flex items-center justify-between gap-3">
              <p className="text-muted-foreground">
                {PROVIDERS.find((p) => p.id === provider)?.hint}
              </p>
              <Button variant="outline" onClick={signIn} disabled={busy}>
                {busy ? <Spinner /> : null} Sign in
              </Button>
            </div>
          ))}
      </div>
      {error && <p className="mt-3 text-sm text-destructive">{error}</p>}
      <Nav
        onBack={() => setStep('agent')}
        next={saveInference}
        nextLabel="Next"
        busy={busy}
        disabled={
          (provider === 'local' && probe.status !== 'found') ||
          (provider === 'openai' && !apiKey.trim()) ||
          ((provider === 'codex' || provider === 'grok') && !signedIn)
        }
      />
    </Frame>
  )
}
