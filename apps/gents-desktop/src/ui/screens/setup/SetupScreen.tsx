/* First run, from the Startup designs: choose where the agent lives,
   name it, watch it come online, then set up inference. Every step
   calls the bridge the way the desktop app does: initLocalStandardRuntime,
   requestStatusEnrollment, probeInferenceEndpoint, the provider logins
   and saveBackendConfig. */
import { useEffect, useState } from "react";
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
} from "lucide-react";
import type {
  BackendProviderKind,
  DesktopClientSnapshot,
  OpenAiWireApi,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import {
  projectStartupLoadingStatus,
  type DesktopStartupPhase,
  type LoadingStepState,
} from "../../../lib/loadingStatus";
import type { Shell } from "@/hooks/useShell";
import { backendIsConfigured, shouldRebindSetupDefault } from "@/lib/firstRun";
import { setupStewardPatches } from "@/lib/setupSteward";
import { isMobileTauriShell } from "../../../lib/shellPlatform";
import { AgentAvatar } from "@/screens/AgentAvatar";
import { applyTheme, themePreference } from "@/theme";
import { Mark } from "@/app/Mark";
import { openExternalUrl } from "../../../lib/externalLinks";
import { watchProviderLoginUrl, type OauthProvider } from "@/lib/providerLogin";

type Step = "welcome" | "remote" | "agent" | "starting" | "inference" | "ready";

const OPENAI_ENDPOINT = "https://api.openai.com/v1";
const OPENAI_DEFAULT_MODEL = "gpt-5.4-mini";
const OPENROUTER_ENDPOINT = "https://openrouter.ai/api/v1";
const OPENROUTER_DEFAULT_MODEL = "openai/gpt-4o-mini";
const LOCAL_DEFAULT_URL = "http://127.0.0.1:11434/v1";
const LOCAL_PROBE_URLS = ["http://127.0.0.1:8080/v1", LOCAL_DEFAULT_URL];
const GROK_ENDPOINT = "https://cli-chat-proxy.grok.com/v1";
const GROK_DEFAULT_MODEL = "grok-4.5";
const CLAUDE_ENDPOINT = "claude-cli://subscription";
const CLAUDE_DEFAULT_MODEL = "claude-sonnet-5";
const CODEX_ENDPOINT = "https://chatgpt.com/backend-api/codex";
const CODEX_DEFAULT_MODEL = "gpt-5.5";

const PROVIDERS = [
  {
    id: "openai",
    title: "OpenAI",
    hint: "Sign in with ChatGPT, or paste an API key.",
    icon: KeyRound,
    logo: "/logos/openai.svg",
  },
  {
    id: "anthropic",
    title: "Anthropic",
    hint: "Sign in with Claude Pro or Max.",
    icon: Sparkles,
    logo: "/logos/claude.svg",
  },
  {
    id: "grok",
    title: "Grok",
    hint: "Sign in with SuperGrok or X Premium+.",
    icon: Orbit,
    logo: "/logos/grok.svg",
  },
  {
    id: "local",
    title: "Local",
    hint: "Ollama, llama.cpp or any OpenAI-compatible server.",
    icon: Server,
    logo: "/logos/ollama.svg",
  },
  {
    id: "openrouter",
    title: "Open Router",
    hint: "One key for many hosted models.",
    icon: KeyRound,
    logo: "/logos/openrouter.svg",
  },
] as const;
type ProviderId = (typeof PROVIDERS)[number]["id"];

function Frame({ children }: { children: React.ReactNode }) {
  const [theme, setTheme] = useState(themePreference);
  const flip = () => {
    const next = theme === "dark" ? "light" : "dark";
    applyTheme(next);
    setTheme(next);
  };
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
          {theme === "dark" ? <Sun /> : <Moon />}
        </Button>
      </div>
    </ScrollArea>
  );
}

function Title({ children, note }: { children: React.ReactNode; note: string }) {
  return (
    <div className="mb-6">
      <h1 className="font-heading text-2xl font-medium text-heading">{children}</h1>
      <p className="mt-1.5 text-sm text-muted-foreground">{note}</p>
    </div>
  );
}

function Option({
  selected,
  onSelect,
  title,
  hint,
  icon: Icon,
  logo,
  testId,
}: {
  selected: boolean;
  onSelect: () => void;
  title: string;
  hint?: string;
  icon: typeof Server;
  logo?: string;
  testId?: string;
}) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      data-testid={testId}
      onClick={onSelect}
      className={cn(
        "flex w-full items-center gap-3 rounded-2xl border bg-raised px-4 py-3.5 text-left transition-shadow",
        selected
          ? "border-brand ring-1 ring-brand"
          : "border-border/60 hover:bg-accent",
      )}
    >
      {selected ? (
        <CircleCheck className="size-4 shrink-0 text-foreground" />
      ) : (
        <Circle className="size-4 shrink-0 text-muted-foreground" />
      )}
      <span className="min-w-0 flex-1">
        <span className="block text-sm font-medium">{title}</span>
        {hint && (
          <span className="block truncate text-xs text-muted-foreground">{hint}</span>
        )}
      </span>
      {logo ? (
        <img src={logo} alt="" className="size-5 shrink-0 object-contain dark:invert" />
      ) : (
        <Icon className="size-5 shrink-0 text-heading" />
      )}
    </button>
  );
}

function Nav({
  onBack,
  next,
  nextLabel = "Next",
  busy,
  disabled,
}: {
  onBack?: () => void;
  next?: () => void;
  nextLabel?: string;
  busy?: boolean;
  disabled?: boolean;
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
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="grid gap-1">
      <span className="text-xs text-muted-foreground">{label}</span>
      {children}
    </label>
  );
}

const stepIcon = (state: LoadingStepState | null) =>
  state === "complete" ? (
    <CircleCheck className="size-4 text-muted-foreground" />
  ) : state === "active" ? (
    <Spinner className="text-foreground" />
  ) : (
    <span className="size-1.5 rounded-full bg-border" />
  );

function defaultEndpoint(id: ProviderId) {
  switch (id) {
    case "openai":
      return OPENAI_ENDPOINT;
    case "openrouter":
      return OPENROUTER_ENDPOINT;
    case "local":
      return LOCAL_DEFAULT_URL;
    case "anthropic":
      return CLAUDE_ENDPOINT;
    case "grok":
      return GROK_ENDPOINT;
  }
}

function defaultModel(id: ProviderId) {
  switch (id) {
    case "openai":
      return OPENAI_DEFAULT_MODEL;
    case "openrouter":
      return OPENROUTER_DEFAULT_MODEL;
    case "anthropic":
      return CLAUDE_DEFAULT_MODEL;
    case "grok":
      return GROK_DEFAULT_MODEL;
    case "local":
      return "";
  }
}

export function SetupScreen({
  shell,
  onDone,
  initialStep = "welcome",
}: {
  shell: Shell;
  onDone: (snapshot: DesktopClientSnapshot) => void;
  initialStep?: Step;
}) {
  const [step, setStep] = useState<Step>(initialStep);
  const allowLocal = !isMobileTauriShell();
  const [where, setWhere] = useState<"local" | "remote">(
    allowLocal ? "local" : "remote",
  );
  const [address, setAddress] = useState("");
  const [name, setName] = useState("Forge");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [phase, setPhase] = useState<Exclude<DesktopStartupPhase, "ready">>(
    "checking-managed-server",
  );
  const [provider, setProvider] = useState<ProviderId>("openai");
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState(OPENAI_DEFAULT_MODEL);
  const [endpoint, setEndpoint] = useState(OPENAI_ENDPOINT);
  const [probe, setProbe] = useState<{
    status: "idle" | "probing" | "found" | "none";
    url: string;
    models: string[];
  }>({ status: "idle", url: "", models: [] });
  const [signedIn, setSignedIn] = useState<string | null>(null);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const api = shell.api;
  const root = shell.snapshot?.bootstrap.defaultAgentHome ?? "~/.gents";

  const finishProvisioning = async () => {
    const next = await api.fetchDesktopSnapshot();
    const nextDeployment = next.client?.deployments[0];
    const steward = nextDeployment ? setupStewardPatches(nextDeployment) : [];
    if (steward.length && nextDeployment) {
      await api.patchConfigComponents({
        agentDid: nextDeployment.agentDid,
        patches: steward,
      });
    }
    await shell.refreshSnapshot();
    setStep("inference");
  };

  useEffect(() => {
    if (step !== "inference" || provider !== "local" || probe.status !== "idle") return;
    void (async () => {
      await Promise.resolve();
      setProbe({ status: "probing", url: "", models: [] });
      const urls = endpoint.trim()
        ? [
            endpoint.trim(),
            ...LOCAL_PROBE_URLS.filter((url) => url !== endpoint.trim()),
          ]
        : LOCAL_PROBE_URLS;
      for (const url of urls) {
        const result = await api.probeInferenceEndpoint(url).catch(() => null);
        if (result?.reachable && result.models.length) {
          setProbe({ status: "found", url, models: result.models });
          setEndpoint(url);
          setModel((m) => m || result.models[0]!);
          return;
        }
      }
      setProbe({ status: "none", url: "", models: [] });
    })();
    // Probe the current endpoint plus the usual local ports once per idle.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [step, provider, probe.status, api]);

  const createAgent = async () => {
    const agentName = name.trim() || "Local Agent";
    setBusy(true);
    setError(null);
    setStep("starting");
    setPhase("checking-managed-server");
    try {
      if (api.startManagedServer) {
        await api.startManagedServer(agentName);
      }
      setPhase("loading-configuration");
      await shell.onInitLocalRuntime(agentName);
      setPhase("starting-client");
      if (api.commitManagedServerAutoStart) {
        await api.commitManagedServerAutoStart(agentName);
      }
      await finishProvisioning();
    } catch (e) {
      setPhase("managed-server-error");
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const enrol = async () => {
    setBusy(true);
    setError(null);
    setStep("starting");
    setPhase("starting-client");
    try {
      await api.requestStatusEnrollment(address.trim());
      await finishProvisioning();
    } catch (e) {
      setPhase("client-error");
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  const signIn = async () => {
    if (provider !== "openai" && provider !== "anthropic" && provider !== "grok") {
      return;
    }
    const oauthProvider: OauthProvider = provider;
    setBusy(true);
    setError(null);
    setAuthUrl(null);
    let unlisten = () => {};
    try {
      unlisten = await watchProviderLoginUrl(oauthProvider, setAuthUrl);
      const snapshot = await api.fetchDesktopSnapshot();
      const agentDid =
        shell.snapshot?.client?.deployments[0]?.agentDid ??
        snapshot.client?.deployments[0]?.agentDid;
      if (!agentDid) throw new Error("No agent to sign in");
      const result =
        oauthProvider === "openai"
          ? await api.codexLogin(agentDid)
          : oauthProvider === "anthropic"
            ? await api.claudeLogin(agentDid)
            : await api.grokLogin(agentDid);
      setSignedIn(result.credentialId);
      setAuthUrl(null);
      if (oauthProvider === "openai") setModel(CODEX_DEFAULT_MODEL);
      await persistInference({ signedInId: result.credentialId });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      unlisten();
      setBusy(false);
    }
  };

  const cancelSignIn = () => {
    if (provider === "openai") void api.cancelCodexLogin();
    else if (provider === "anthropic") void api.cancelClaudeLogin();
    else if (provider === "grok") void api.cancelGrokLogin();
  };
  const persistInference = async (opts?: { signedInId?: string }) => {
    const signed = opts?.signedInId ?? signedIn;
    const snapshot = await api.fetchDesktopSnapshot();
    const deployment =
      shell.snapshot?.client?.deployments[0] ?? snapshot.client?.deployments[0];
    if (!deployment) throw new Error("No agent to configure");
    const spec =
      provider === "openai" && signed && !apiKey.trim()
        ? {
            backendId: "openai",
            name: "ChatGPT",
            providerKind: "ChatGptCodex" as const,
            openaiWireApi: "responses" as const,
            endpoint: CODEX_ENDPOINT,
            apiKey: null as string | null,
            oauth: true,
            models: [model.trim() || CODEX_DEFAULT_MODEL],
          }
        : provider === "openai"
          ? {
              backendId: "openai",
              name: "OpenAI",
              providerKind: "OpenAiCompatible" as const,
              openaiWireApi: "responses" as const,
              endpoint: endpoint.trim() || OPENAI_ENDPOINT,
              apiKey: apiKey.trim(),
              oauth: false,
              models: [model.trim() || OPENAI_DEFAULT_MODEL],
            }
          : provider === "openrouter"
            ? {
                backendId: "openrouter",
                name: "Open Router",
                providerKind: "OpenRouter" as const,
                openaiWireApi: "chat_completions" as const,
                endpoint: endpoint.trim() || OPENROUTER_ENDPOINT,
                apiKey: apiKey.trim(),
                oauth: false,
                models: [model.trim() || OPENROUTER_DEFAULT_MODEL],
              }
            : provider === "anthropic"
              ? {
                  backendId: "anthropic",
                  name: "Anthropic",
                  providerKind: "ClaudeCliSubscription" as const,
                  openaiWireApi: null,
                  endpoint: CLAUDE_ENDPOINT,
                  apiKey: null as string | null,
                  oauth: true,
                  models: [model.trim() || CLAUDE_DEFAULT_MODEL],
                }
              : provider === "grok"
                ? {
                    backendId: "grok",
                    name: "Grok",
                    providerKind: "XaiGrokOAuth" as const,
                    openaiWireApi: "chat_completions" as const,
                    endpoint: GROK_ENDPOINT,
                    apiKey: null as string | null,
                    oauth: true,
                    models: [model.trim() || GROK_DEFAULT_MODEL],
                  }
                : {
                    backendId: "local",
                    name: "Local server",
                    providerKind: "OpenAiCompatible" as const,
                    openaiWireApi: "chat_completions" as const,
                    endpoint: endpoint.trim() || probe.url || LOCAL_DEFAULT_URL,
                    apiKey: apiKey.trim() || null,
                    oauth: false,
                    models: [model.trim() || probe.models[0] || "gents-7b"],
                  };
    const sameKind = deployment.inferenceBackends.find(
      (backend) => backend.providerKind === spec.providerKind,
    );
    const placeholder = deployment.inferenceBackends.find(
      (backend) => !backendIsConfigured(backend),
    );
    const addingExtra = deployment.inferenceBackends.some(
      (backend) =>
        backendIsConfigured(backend) && backend.providerKind !== spec.providerKind,
    );
    const backendId =
      sameKind?.backendId ??
      (!addingExtra ? placeholder?.backendId : undefined) ??
      spec.backendId;
    const existingProfile =
      deployment.inferenceProfiles.find((p) => p.backend_id === backendId) ??
      (!addingExtra ? deployment.inferenceProfiles[0] : undefined);
    const profileId = existingProfile?.profile_id ?? `profile-${backendId}`;
    await api.saveBackendConfig({
      document: {
        agent_did: deployment.agentDid,
        backend_id: backendId,
        name: spec.name,
        provider_kind: spec.providerKind as BackendProviderKind,
        openai_wire_api: spec.openaiWireApi as OpenAiWireApi | null,
        endpoint: spec.endpoint,
        auth: spec.oauth
          ? { kind: "principal_oauth" }
          : spec.apiKey
            ? { kind: "api_key", key: spec.apiKey }
            : { kind: "unauthenticated" },
        max_concurrent: 2,
        max_queue_depth: 8,
        enabled: true,
      },
    });
    await api.saveInferenceProfileConfig({
      document: {
        ...existingProfile,
        agent_did: deployment.agentDid,
        profile_id: profileId,
        display_name: spec.name,
        backend_id: backendId,
        model_name: spec.models[0] ?? "model",
      },
    });
    const defaultBehaviorId = deployment.agentPrincipal.defaultBehaviorId;
    // A healthy process may make init's generated local placeholder look
    // configured. The first provider still replaces that placeholder, while
    // adding another provider later must not change the user's chosen default.
    const shouldRebindDefault = shouldRebindSetupDefault(deployment, addingExtra);
    for (const b of deployment.behaviors) {
      if (
        !b.inferenceProfileId ||
        (b.behaviorId === defaultBehaviorId && shouldRebindDefault)
      ) {
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
        });
      }
    }
    const next = await api.fetchDesktopSnapshot();
    const nextDeployment = next.client?.deployments[0];
    const steward = nextDeployment ? setupStewardPatches(nextDeployment) : [];
    if (steward.length && nextDeployment) {
      await api.patchConfigComponents({
        agentDid: nextDeployment.agentDid,
        patches: steward,
      });
    }
    await shell.refreshSnapshot();
    return next;
  };

  const saveInference = async () => {
    setBusy(true);
    setError(null);
    try {
      await persistInference();
      setStep("ready");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const pickProvider = (id: ProviderId) => {
    if (busy) cancelSignIn();
    setProvider(id);
    setSignedIn(null);
    setAuthUrl(null);
    setApiKey("");
    setError(null);
    setModel(defaultModel(id));
    setEndpoint(defaultEndpoint(id));
    if (id === "local") setProbe({ status: "idle", url: "", models: [] });
  };

  const inferenceReady =
    provider === "openai"
      ? Boolean(signedIn || (apiKey.trim() && endpoint.trim()))
      : provider === "openrouter"
        ? Boolean(
            apiKey.trim() &&
            endpoint.trim() &&
            (model.trim() || OPENROUTER_DEFAULT_MODEL),
          )
        : provider === "local"
          ? Boolean((endpoint.trim() || probe.url) && (model.trim() || probe.models[0]))
          : Boolean(signedIn);

  if (step === "welcome") {
    return (
      <Frame>
        <Mark className="mb-6 h-6 text-ink" />
        <Title note="Gents runs agents whose every step is a document. Start one here, or connect to one that already runs.">
          Let’s get set up
        </Title>
        <div className="grid gap-3">
          {allowLocal && (
            <Option
              selected={where === "local"}
              onSelect={() => setWhere("local")}
              title="Local agent"
              hint="Create an agent on this Mac."
              icon={Server}
            />
          )}
          <Option
            selected={where === "remote"}
            onSelect={() => setWhere("remote")}
            title="Remote connect"
            hint="Join a Gents server someone else runs."
            icon={Wifi}
          />
        </div>
        <Nav next={() => setStep(where === "local" ? "agent" : "remote")} />
      </Frame>
    );
  }
  if (step === "remote") {
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
          onBack={() => setStep("welcome")}
          next={enrol}
          nextLabel="Request access"
          busy={busy}
          disabled={!address.trim()}
        />
      </Frame>
    );
  }
  if (step === "agent") {
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
          onBack={() => setStep("welcome")}
          next={createAgent}
          nextLabel="Start"
          busy={busy}
          disabled={!name.trim()}
        />
      </Frame>
    );
  }
  if (step === "starting") {
    const status = projectStartupLoadingStatus(phase, true);
    const steps: [string, LoadingStepState | null][] = [
      ["Restore hosted agent", status.managedServerState],
      ["Read saved connections", status.connectionState],
      ["Start secure client", status.clientState],
    ];
    const saying: Record<string, string> = {
      "checking-managed-server": "Waking the hosted agent…",
      "loading-configuration": "Teaching the gossip network some manners…",
      "starting-client": "Turning the secure client on…",
    };
    return (
      <Frame>
        <h1 className="font-heading text-2xl font-medium text-heading">
          {status.failed ? status.title : "Startup"}
        </h1>
        <p className="mt-3 flex items-center gap-2 text-sm text-muted-foreground">
          {status.failed ? null : <Spinner className="text-foreground" />}
          {saying[phase] ?? status.currentLabel}
        </p>
        <ol className="mt-6 grid gap-2">
          {steps.map(([label, state], i) =>
            state === "pending" ? null : (
              <li
                key={label}
                className="flex items-center gap-3 rounded-2xl border border-border/60 bg-raised px-4 py-3 text-sm animate-in fade-in-0 slide-in-from-bottom-1 duration-300 fill-mode-both"
              >
                <span className="font-mono text-[11px] text-muted-foreground">
                  0{i + 1}.
                </span>
                <span className="flex-1">{label}</span>
                {stepIcon(state)}
              </li>
            ),
          )}
        </ol>
        {error && (
          <div className="mt-4 grid gap-3">
            <p className="text-sm text-destructive">{error}</p>
            <Button
              variant="brand"
              onClick={() => {
                setError(null);
                setStep("agent");
              }}
            >
              Try again
            </Button>
          </div>
        )}
      </Frame>
    );
  }
  if (step === "ready") {
    const agentName =
      shell.selectedDeployment?.agentPrincipal.displayName ??
      (name.trim() || "your agent");
    const providerTitle =
      PROVIDERS.find((p) => p.id === provider)?.title ?? "inference";
    return (
      <Frame>
        <Mark className="mb-6 h-6 text-ink" />
        <Title
          note={`${providerTitle} is connected. The first message starts a conversation with ${agentName}.`}
        >
          You’re in
        </Title>
        <p className="text-sm text-muted-foreground">
          Send a message to begin. You can add another provider later from the agent’s
          inference settings.
        </p>
        {error && <p className="mt-3 text-sm text-destructive">{error}</p>}
        <Nav
          next={async () => {
            setBusy(true);
            try {
              await persistInference();
              const snapshot = await api.fetchDesktopSnapshot();
              onDone(snapshot);
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
              setBusy(false);
            }
          }}
          nextLabel="Start chatting"
          busy={busy}
        />
      </Frame>
    );
  }
  return (
    <Frame>
      <Title note="Pick a provider and enter its key, sign in, or point at a local endpoint. You can add others later.">
        Configure inference
      </Title>
      <div
        className="grid grid-cols-2 gap-3"
        role="radiogroup"
        aria-label="Inference provider"
      >
        {PROVIDERS.map((p) => (
          <Option
            key={p.id}
            selected={provider === p.id}
            onSelect={() => pickProvider(p.id)}
            title={p.title}
            hint={p.hint}
            icon={p.icon}
            logo={p.logo}
            testId={`setup-provider-${p.id}`}
          />
        ))}
      </div>
      <div className="mt-4 grid min-h-44 content-center rounded-2xl border border-border/60 bg-raised p-4 text-sm">
        {provider === "local" && (
          <div className="grid gap-3">
            <Field label="Endpoint">
              <Input
                value={endpoint}
                onChange={(e) => setEndpoint(e.target.value)}
                placeholder={LOCAL_DEFAULT_URL}
                className="font-mono"
              />
            </Field>
            {probe.status === "found" ? (
              <p className="text-xs text-muted-foreground">
                Found a server at{" "}
                <span className="font-mono text-foreground">{probe.url}</span>
              </p>
            ) : probe.status === "none" ? (
              <p className="text-xs text-muted-foreground">
                No server answered.{" "}
                <button
                  type="button"
                  className="underline"
                  onClick={() => setProbe({ status: "idle", url: "", models: [] })}
                >
                  Try again
                </button>
              </p>
            ) : (
              <p className="flex items-center gap-2 text-xs text-muted-foreground">
                <Spinner className="text-foreground" /> Looking for a local server…
              </p>
            )}
            {probe.status === "found" && probe.models.length > 0 ? (
              <Field label="Model">
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
              </Field>
            ) : (
              <Field label="Model">
                <Input
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                  placeholder="llama3.2"
                />
              </Field>
            )}
            <Field label="API key (optional)">
              <Input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="if the server requires one"
              />
            </Field>
          </div>
        )}
        {provider === "openrouter" && (
          <div className="grid gap-3">
            <Field label="API key">
              <Input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="sk-or-…"
                autoFocus
              />
            </Field>
            <Field label="Endpoint">
              <Input
                value={endpoint}
                onChange={(e) => setEndpoint(e.target.value)}
                placeholder={OPENROUTER_ENDPOINT}
                className="font-mono"
              />
            </Field>
            <Field label="Model">
              <Input
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder={OPENROUTER_DEFAULT_MODEL}
              />
            </Field>
          </div>
        )}
        {(provider === "openai" || provider === "anthropic" || provider === "grok") && (
          <div className="grid gap-3">
            {signedIn ? (
              <p className="flex items-center gap-2">
                <CircleCheck className="size-4 text-muted-foreground" /> Signed in ·
                credential <span className="font-mono text-xs">{signedIn}</span>
              </p>
            ) : (
              <div className="grid gap-3">
                <div className="flex items-center justify-between gap-3">
                  <p className="text-muted-foreground">
                    {provider === "openai"
                      ? "Sign in with ChatGPT (Plus, Pro, or Team)."
                      : PROVIDERS.find((p) => p.id === provider)?.hint}
                  </p>
                  <span className="flex shrink-0 items-center gap-2">
                    {busy ? (
                      <Button variant="outline" onClick={cancelSignIn}>
                        Cancel
                      </Button>
                    ) : null}
                    <Button variant="outline" onClick={signIn} disabled={busy}>
                      {busy ? <Spinner /> : null} {busy ? "Waiting…" : "Sign in"}
                    </Button>
                  </span>
                </div>
                {authUrl ? (
                  <p className="text-xs text-muted-foreground">
                    Browser didn’t open?{" "}
                    <button
                      type="button"
                      className="underline"
                      onClick={() => void openExternalUrl(authUrl)}
                    >
                      Open the sign-in page
                    </button>
                  </p>
                ) : null}
              </div>
            )}
            {provider === "openai" && !signedIn ? (
              <>
                <p className="text-xs text-muted-foreground">or paste an API key</p>
                <Field label="API key">
                  <Input
                    type="password"
                    value={apiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                    placeholder="sk-…"
                  />
                </Field>
                <Field label="Endpoint">
                  <Input
                    value={endpoint}
                    onChange={(e) => setEndpoint(e.target.value)}
                    placeholder={OPENAI_ENDPOINT}
                    className="font-mono"
                  />
                </Field>
                <Field label="Model">
                  <Input
                    value={model}
                    onChange={(e) => setModel(e.target.value)}
                    placeholder={OPENAI_DEFAULT_MODEL}
                  />
                </Field>
              </>
            ) : null}
          </div>
        )}
      </div>
      {error && <p className="mt-3 text-sm text-destructive">{error}</p>}
      <Nav
        onBack={initialStep === "inference" ? undefined : () => setStep("agent")}
        next={saveInference}
        nextLabel="Next"
        busy={busy}
        disabled={!inferenceReady}
      />
    </Frame>
  );
}
