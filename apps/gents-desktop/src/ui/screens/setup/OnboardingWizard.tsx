/* First run, from the Startup designs: choose where the agent lives,
   name it, watch it come online, then connect one inference provider
   (InferenceSetup). */
import { useEffect, useState } from "react";
import { Server, Wifi } from "lucide-react";
import type {
  DesktopClientSnapshot,
  ManagedServerAuthorityInput,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { Spinner } from "@gents/ui/components/spinner";
import {
  projectStartupLoadingStatus,
  type DesktopStartupPhase,
} from "../../../lib/loadingStatus";
import {
  observeManagedServerOperation,
  type ManagedServerWait,
} from "../../../lib/managedServerStartup";
import { SETUP_COMPLETE_DWELL_MS, SetupProgress } from "./SetupProgress";
import { setupStewardPatches } from "@/lib/setupSteward";
import { supportsLocalManagedServer } from "../../../lib/shellPlatform";
import { AgentAvatar } from "@/screens/AgentAvatar";
import { Mark } from "@/app/Mark";
import { setupErrorMessage } from "@/lib/providerLogin";
import { waitForManagedRuntimePairing } from "@/lib/managedRuntimeReadiness";
import { ManagedRuntimeAuthorityPicker } from "@/components/ManagedRuntimeAuthority";
import { authoritiesEqual, authorityForSelection } from "@/lib/managedRuntimeAuthority";
import { useApp } from "@/app/AppContext";
import { useBootstrap, useStartup } from "@/hooks/useClient";
import { Field, Frame, Nav, Option, Title } from "./parts";
import { InferenceSetup } from "./InferenceSetup";
import type { ProviderId } from "./inferenceSetupForm";

type Step = "welcome" | "starting" | "inference";

function shortDid(did: string | null) {
  if (!did) return "a new agent identity";
  return did.length > 24 ? `${did.slice(0, 14)}…${did.slice(-6)}` : did;
}

export function ceilingFromInit(
  value?: string | null,
): ManagedServerAuthorityInput["toolCeiling"] {
  switch (value?.trim().toLowerCase()) {
    case "meta-only":
    case "metaonly":
    case "meta_only":
      return "meta-only";
    case "readonly":
      return "readonly";
    default:
      return "readwrite";
  }
}

export function OnboardingWizard({
  onDone,
  initialStep = "welcome",
  agentDid,
  onCancel,
  provider,
}: {
  onDone: (snapshot: DesktopClientSnapshot) => void;
  initialStep?: Step;
  agentDid?: string;
  onCancel?: () => void;
  provider?: ProviderId;
}) {
  const bootstrap = useBootstrap();
  const {
    api,
    actions: { initLocalRuntime, refreshSnapshot, changeConfig },
  } = useApp();
  const { diagnosticsHint } = useStartup();
  const { incompatibleHome } = useStartup();
  const [step, setStep] = useState<Step>(initialStep);
  const allowLocal = supportsLocalManagedServer();
  const [where, setWhere] = useState<"local" | "remote">(
    allowLocal ? "local" : "remote",
  );
  const [address, setAddress] = useState("");
  const existingHome = Boolean(
    bootstrap?.agentHomeExists && bootstrap?.initAgentDid?.trim(),
  );
  const [name, setName] = useState(bootstrap?.initAgentName?.trim() || "Forge");
  /* An initialized home keeps its identity: provisioning never renames it,
     so its name is shown, not asked for. */
  const existingName = existingHome ? bootstrap?.initAgentName?.trim() || null : null;
  const agentName = existingName ?? name;
  const [homeRoot, setHomeRoot] = useState<string | null>(
    api.managedServerStatus ? null : (bootstrap?.initToolRoot ?? null),
  );
  const [toolCeiling, setToolCeiling] = useState<
    ManagedServerAuthorityInput["toolCeiling"]
  >(() => ceilingFromInit(bootstrap?.initToolCeiling));
  const [selectedDirectory, setSelectedDirectory] = useState<string | null | undefined>(
    bootstrap?.initToolRoot ?? undefined,
  );
  const [authorityError, setAuthorityError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [phase, setPhase] = useState<Exclude<DesktopStartupPhase, "ready">>(
    "checking-managed-server",
  );
  const [startupDetails, setStartupDetails] = useState<
    Partial<Record<"managedServer" | "configuration" | "client", string>>
  >({});
  const [managedWait, setManagedWait] = useState<ManagedServerWait | null>(null);
  const [provisionedAt, setProvisionedAt] = useState<number | null>(null);
  useEffect(() => {
    if (provisionedAt === null) return;
    const timer = window.setTimeout(
      () => setStep("inference"),
      Math.max(0, provisionedAt + SETUP_COMPLETE_DWELL_MS - Date.now()),
    );
    return () => window.clearTimeout(timer);
  }, [provisionedAt]);
  const root = bootstrap?.defaultAgentHome ?? "~/.gents";
  const toolRoot = selectedDirectory === undefined ? homeRoot : selectedDirectory;
  const authority = authorityForSelection(toolCeiling, toolRoot);

  useEffect(() => {
    if (step !== "welcome" || !allowLocal || !api.managedServerStatus || homeRoot)
      return;
    const pending = api.managedServerStatus();
    if (!pending) return;
    void pending
      .then((status) => {
        if (status.suggestedToolRoot) setHomeRoot(status.suggestedToolRoot);
        else setAuthorityError("The user home directory is unavailable.");
      })
      .catch((cause) =>
        setAuthorityError(cause instanceof Error ? cause.message : String(cause)),
      );
  }, [api, homeRoot, step, allowLocal]);

  const finishProvisioning = async () => {
    const next = await api.fetchDesktopSnapshot();
    const nextDeployment = next.client?.deployments[0];
    const steward = nextDeployment ? setupStewardPatches(nextDeployment) : [];
    if (steward.length && nextDeployment) {
      await changeConfig("patchConfigComponents", {
        agentDid: nextDeployment.agentDid,
        patches: steward,
      });
    } else {
      await refreshSnapshot();
    }
    setStartupDetails((current) => ({
      ...current,
      client: nextDeployment
        ? `Connected securely to ${nextDeployment.label}`
        : "Secure client started",
    }));
    setProvisionedAt(Date.now());
  };

  const createAgent = async () => {
    const requestedName = agentName.trim();
    if (!requestedName) return;
    if (!authority) {
      setAuthorityError(
        homeRoot
          ? "Choose and validate an existing directory."
          : "The user home directory is still being resolved.",
      );
      setStep("welcome");
      return;
    }
    setBusy(true);
    setError(null);
    setStep("starting");
    setPhase("checking-managed-server");
    setStartupDetails({});
    setProvisionedAt(null);
    let failedPhase: Exclude<DesktopStartupPhase, "ready"> = "managed-server-error";
    try {
      if (api.startManagedServer) {
        const startManagedServer = api.startManagedServer;
        const status = await observeManagedServerOperation(
          api,
          () => startManagedServer(requestedName, authority),
          setManagedWait,
        );
        if (status.agentName && status.agentName !== requestedName) {
          /* Welcome then shows the existing agent's name. */
          void refreshSnapshot();
          throw new Error(
            `This computer already has a local agent named ${status.agentName}, so ${requestedName} was not created. Go back to continue with ${status.agentName}.`,
          );
        }
        const confirmed: ManagedServerAuthorityInput | null =
          status.effectiveToolCeiling
            ? {
                toolCeiling: status.effectiveToolCeiling,
                toolRoot: status.effectiveToolRoot,
              }
            : null;
        if (!confirmed || !authoritiesEqual(confirmed, authority)) {
          throw new Error(
            "The managed runtime started with different authority than the reviewed settings.",
          );
        }
        setStartupDetails({
          managedServer: `${requestedName} is running as ${shortDid(status.agentDid)}, with its identity and data in ${root}`,
        });
      }
      setPhase("loading-configuration");
      failedPhase = "configuration-error";
      await initLocalRuntime(requestedName);
      setStartupDetails((current) => ({
        ...current,
        configuration: `Saved the local connection to ${requestedName}`,
      }));
      setPhase("starting-client");
      failedPhase = "client-error";
      if (api.commitManagedServerAutoStart) {
        await api.commitManagedServerAutoStart(requestedName);
      }
      await waitForManagedRuntimePairing(api);
      await finishProvisioning();
    } catch (e) {
      setPhase(failedPhase);
      setError(setupErrorMessage(e));
      await incompatibleHome?.adopt(e);
    } finally {
      setBusy(false);
    }
  };
  const enrol = async () => {
    setBusy(true);
    setError(null);
    setStep("starting");
    setPhase("starting-client");
    setStartupDetails({});
    setProvisionedAt(null);
    try {
      await api.requestStatusEnrollment(address.trim());
      await finishProvisioning();
    } catch (e) {
      setPhase("client-error");
      setError(setupErrorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  if (step === "inference")
    return (
      <InferenceSetup
        purpose="onboarding"
        checkRuntime={initialStep === "inference"}
        onDone={onDone}
        agentDid={agentDid}
        onCancel={onCancel}
        provider={provider}
        onBack={initialStep === "inference" ? undefined : () => setStep("welcome")}
      />
    );
  if (step === "welcome") {
    return (
      <Frame>
        <Mark className="mb-6 h-6 text-ink" />
        <Title note="Gents keeps a record of every step an agent takes, so you can see what it did and it can pick up where it left off. Start an agent here, or connect to one that already runs.">
          Let’s get set up
        </Title>
        <div className="grid gap-3">
          {allowLocal && (
            <Option
              selected={where === "local"}
              onSelect={() => setWhere("local")}
              title="Local agent"
              hint={
                existingHome
                  ? "Continue the agent already on this computer."
                  : "Create an agent that runs independently in the background."
              }
              icon={Server}
            >
              <div className="flex items-end gap-3">
                <AgentAvatar name={agentName} className="mb-1 size-8 shrink-0" />
                <div className="min-w-0 flex-1">
                  <Field label="Agent name">
                    <Input
                      value={agentName}
                      onChange={(event) => setName(event.target.value)}
                      readOnly={existingName !== null}
                      aria-readonly={existingName !== null}
                    />
                  </Field>
                </div>
              </div>
              {homeRoot ? (
                <ManagedRuntimeAuthorityPicker
                  home={homeRoot}
                  toolCeiling={toolCeiling}
                  toolRoot={toolRoot}
                  onCeilingChange={setToolCeiling}
                  onRootChange={setSelectedDirectory}
                  validateRoot={api.validateManagedServerRoot}
                  error={authorityError}
                  onError={setAuthorityError}
                />
              ) : (
                <div className="grid gap-2 text-sm text-muted-foreground">
                  {authorityError ? (
                    <>
                      <p role="alert" className="text-destructive">
                        {authorityError}
                      </p>
                      <Button
                        variant="outline"
                        onClick={() => {
                          setAuthorityError(null);
                          void api
                            .managedServerStatus?.()
                            .then((status) => {
                              if (status.suggestedToolRoot)
                                setHomeRoot(status.suggestedToolRoot);
                              else
                                setAuthorityError(
                                  "The user home directory is unavailable.",
                                );
                            })
                            .catch((cause) => setAuthorityError(String(cause)));
                        }}
                      >
                        Try again
                      </Button>
                    </>
                  ) : (
                    <p className="flex items-center gap-2">
                      <Spinner /> Resolving your home directory…
                    </p>
                  )}
                </div>
              )}
              <p className="break-all text-xs text-muted-foreground">
                Agent data: <span className="font-mono">{root}</span>
              </p>
              {existingHome ? (
                <p className="text-xs text-muted-foreground">
                  Found an existing Gents home
                  {bootstrap?.initAgentName ? ` for ${bootstrap?.initAgentName}` : ""}.
                  Next keeps that identity, native service, and reviewed host authority.
                </p>
              ) : null}
              <p className="text-xs text-muted-foreground">
                Your operating system manages the agent as a background service. Closing
                this window or choosing Quit Desktop leaves the agent running; use the
                menu bar’s Stop Agent command to stop it. Start-at-login remains a
                separate operating-system preference.
              </p>
            </Option>
          )}
          <Option
            selected={where === "remote"}
            onSelect={() => setWhere("remote")}
            title="Remote connect"
            hint="Join a Gents server someone else runs."
            icon={Wifi}
          >
            <Field label="Server address">
              <Input
                type="url"
                value={address}
                onChange={(event) => setAddress(event.target.value)}
                placeholder="https://gents.example.net:8787"
              />
            </Field>
            <p className="text-xs text-muted-foreground">
              Use the server’s status address. Its administrator approves your access
              request.
            </p>
          </Option>
        </div>
        {error ? (
          <p role="alert" className="mt-3 text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <Nav
          next={where === "local" ? createAgent : enrol}
          nextLabel={where === "local" ? "Next" : "Request access"}
          busy={busy}
          disabled={
            where === "local"
              ? !agentName.trim() || !authority || !homeRoot
              : !address.trim()
          }
        />
      </Frame>
    );
  }
  if (step === "starting") {
    const status = projectStartupLoadingStatus(phase, true);
    const done = provisionedAt !== null;
    const saying: Record<string, string> = {
      "checking-managed-server": "Starting your local agent…",
      "loading-configuration": "Loading agent configuration…",
      "starting-client": "Starting the secure client…",
    };
    return (
      <Frame>
        <SetupProgress
          title={status.failed ? status.title : done ? "Ready" : "Starting"}
          label={done ? "Everything started." : (saying[phase] ?? status.currentLabel)}
          failed={status.failed}
          done={done}
          steps={[
            {
              label: where === "local" ? "Start local agent" : "Connect to server",
              state: done ? "complete" : status.managedServerState,
              detail: startupDetails.managedServer ?? null,
            },
            {
              label: "Load configuration",
              state: done ? "complete" : status.connectionState,
              detail: startupDetails.configuration ?? null,
            },
            {
              label: "Start secure client",
              state: done ? "complete" : status.clientState,
              detail: startupDetails.client ?? null,
            },
          ]}
          wait={managedWait}
          error={error}
          onRetry={() => {
            setError(null);
            setStep("welcome");
          }}
          onContinue={() => {
            setProvisionedAt(null);
            setStep("inference");
          }}
          onOpenLoginItems={api.openManagedServerLoginItems}
          diagnosticsHint={diagnosticsHint ?? bootstrap?.diagnosticsHint}
        />
      </Frame>
    );
  }
  return null;
}
