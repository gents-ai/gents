/* First run, from the Startup designs: choose where the Node lives,
   name it, watch it come online, then connect one inference provider
   (InferenceSetup). */
import { useEffect, useReducer, useState } from "react";
import { Server, Wifi } from "lucide-react";
import type {
  DesktopClientSnapshot,
  ManagedServerAuthorityInput,
} from "@source-inc/gents-desktop-client";
import { DEFAULT_NODE_NAME } from "@source-inc/gents-desktop-fleet";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { Spinner } from "@gents/ui/components/spinner";
import {
  projectStartupLoadingStatus,
  type DesktopStartupPhase,
} from "../../../lib/loadingStatus";
import { SETUP_COMPLETE_DWELL_MS, SetupProgress } from "./SetupProgress";
import { engineerPatches } from "@/lib/engineer";
import { supportsLocalManagedServer } from "../../../lib/shellPlatform";
import { NodeAvatar } from "@/screens/AgentAvatar";
import { Mark } from "@/app/Mark";
import { setupErrorMessage } from "../../../lib/setupErrors";
import { ManagedRuntimeAuthorityPicker } from "@/components/ManagedRuntimeAuthority";
import { authoritiesEqual, authorityForSelection } from "@/lib/managedRuntimeAuthority";
import { useApp } from "@/app/AppContext";
import { useBootstrap, useStartup } from "@/hooks/useClient";
import { Field, Frame, Nav, Option, Title } from "./parts";
import { InferenceSetup } from "./InferenceSetup";
import { nodeSetUp } from "@/lib/firstRun";
import type { NodeView } from "../../../hooks/fleetStore";
import type { ProviderId } from "./inferenceSetupForm";

type Step = "welcome" | "starting" | "inference";
type FailedPhase = Exclude<DesktopStartupPhase, "ready">;
type StartupDetail = "managedServer" | "configuration" | "client";

/* where first run is, and how far its provisioning has got */
type Run = {
  step: Step;
  /** a provisioning run is out */
  busy: boolean;
  error: string | null;
  phase: FailedPhase;
  details: Partial<Record<StartupDetail, string>>;
  /** when everything started; the provider step follows a moment later */
  provisionedAt: number | null;
};

type RunEvent =
  | { type: "begun"; phase: FailedPhase }
  | { type: "reached"; phase: FailedPhase; detail?: [StartupDetail, string] }
  | { type: "provisioned"; at: number; client: string }
  | { type: "failed"; phase: FailedPhase; error: string }
  | { type: "ended" }
  /** Try again after a failure: back to the welcome step without it */
  | { type: "retried" }
  /** back to the welcome step, keeping what was said */
  | { type: "wentBack" }
  | { type: "continued" };

function runReducer(run: Run, event: RunEvent): Run {
  switch (event.type) {
    case "begun":
      return {
        ...run,
        step: "starting",
        busy: true,
        error: null,
        phase: event.phase,
        details: {},
        provisionedAt: null,
      };
    case "reached":
      return {
        ...run,
        phase: event.phase,
        details: event.detail
          ? { ...run.details, [event.detail[0]]: event.detail[1] }
          : run.details,
      };
    case "provisioned":
      return {
        ...run,
        details: { ...run.details, client: event.client },
        provisionedAt: event.at,
      };
    case "failed":
      return { ...run, phase: event.phase, error: event.error };
    case "ended":
      return { ...run, busy: false };
    case "retried":
      return { ...run, step: "welcome", error: null };
    case "wentBack":
      return { ...run, step: "welcome" };
    case "continued":
      return { ...run, step: "inference", provisionedAt: null };
  }
}

function shortDid(did: string | null) {
  if (!did) return "a new node identity";
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
  nodeDid,
  onCancel,
  provider,
}: {
  onDone: (snapshot: DesktopClientSnapshot) => void;
  initialStep?: Step;
  nodeDid?: string;
  onCancel?: () => void;
  provider?: ProviderId;
}) {
  const bootstrap = useBootstrap();
  const { stores, actions } = useApp();
  const { initLocalRuntime, refreshSnapshot, changeConfig } = actions;
  const { diagnosticsHint } = useStartup();
  const { incompatibleHome } = useStartup();
  const [run, dispatch] = useReducer(runReducer, {
    step: initialStep,
    busy: false,
    error: null,
    phase: "checking-managed-server",
    details: {},
    provisionedAt: null,
  });
  const { step, busy, error, phase, details: startupDetails, provisionedAt } = run;
  const allowLocal = supportsLocalManagedServer();
  const [where, setWhere] = useState<"local" | "remote">(
    allowLocal ? "local" : "remote",
  );
  const [address, setAddress] = useState("");
  const existingHome = Boolean(
    bootstrap?.nodeHomeExists && bootstrap?.initNodeDid?.trim(),
  );
  const [name, setName] = useState(
    bootstrap?.initNodeName?.trim() || DEFAULT_NODE_NAME,
  );
  /* An initialized home keeps its identity: provisioning never renames it,
     so its name is shown, not asked for. */
  const existingName = existingHome ? bootstrap?.initNodeName?.trim() || null : null;
  const nodeName = existingName ?? name;
  const [homeRoot, setHomeRoot] = useState<string | null>(
    actions.localServerOffers.status ? null : (bootstrap?.initToolRoot ?? null),
  );
  const [toolCeiling, setToolCeiling] = useState<
    ManagedServerAuthorityInput["toolCeiling"]
  >(() => ceilingFromInit(bootstrap?.initToolCeiling));
  const [selectedDirectory, setSelectedDirectory] = useState<string | null | undefined>(
    bootstrap?.initToolRoot ?? undefined,
  );
  const [authorityError, setAuthorityError] = useState<string | null>(null);
  const managedWait = stores.localServer.use.wait();
  /* the folder the service suggests as the tool root, or why it cannot */
  const readSuggestedRoot = () =>
    actions.refreshLocalServer().then((status) => {
      if (status?.suggestedToolRoot) setHomeRoot(status.suggestedToolRoot);
      else
        setAuthorityError(
          status
            ? "The user home directory is unavailable."
            : (stores.localServer.getState().readFailure ??
                "The user home directory is unavailable."),
        );
    });
  useEffect(() => {
    if (provisionedAt === null) return;
    const timer = window.setTimeout(
      () => dispatch({ type: "continued" }),
      Math.max(0, provisionedAt + SETUP_COMPLETE_DWELL_MS - Date.now()),
    );
    return () => window.clearTimeout(timer);
  }, [provisionedAt]);
  const root = bootstrap?.defaultNodeHome ?? "~/.gents";
  const toolRoot = selectedDirectory === undefined ? homeRoot : selectedDirectory;
  const authority = authorityForSelection(toolCeiling, toolRoot);

  useEffect(() => {
    if (
      step !== "welcome" ||
      !allowLocal ||
      !actions.localServerOffers.status ||
      homeRoot
    )
      return;
    void readSuggestedRoot();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [actions, homeRoot, step, allowLocal]);

  /* the node just set up, as the next read lists it: the local node's own
     node, which a paired remote node may be listed before; an enrolment's
     node is not listed until its server approves, so it takes the first */
  const finishProvisioning = async (
    setUp: (snapshot: DesktopClientSnapshot) => NodeView | null | undefined,
  ) => {
    const next = await actions.readSnapshot();
    const nextDeployment = setUp(next);
    const engineer = nextDeployment ? engineerPatches(nextDeployment) : [];
    if (engineer.length && nextDeployment) {
      await changeConfig("patchConfigComponents", {
        nodeDid: nextDeployment.nodeDid,
        patches: engineer,
      });
    } else {
      await refreshSnapshot();
    }
    dispatch({
      type: "provisioned",
      at: Date.now(),
      client: nextDeployment
        ? `Connected securely to ${nextDeployment.label}`
        : "Secure client started",
    });
  };

  const createNode = async () => {
    const requestedName = nodeName.trim();
    if (!requestedName) return;
    if (!authority) {
      setAuthorityError(
        homeRoot
          ? "Choose and validate an existing directory."
          : "The user home directory is still being resolved.",
      );
      dispatch({ type: "wentBack" });
      return;
    }
    dispatch({ type: "begun", phase: "checking-managed-server" });
    let failedPhase: FailedPhase = "managed-server-error";
    try {
      if (actions.localServerOffers.start) {
        const status = await actions.startLocalServer(requestedName, authority);
        if (status.nodeName && status.nodeName !== requestedName) {
          /* Welcome then shows the existing Node's name. */
          void refreshSnapshot();
          throw new Error(
            `This computer already has a local node named ${status.nodeName}, so ${requestedName} was not created. Go back to continue with ${status.nodeName}.`,
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
        dispatch({
          type: "reached",
          phase: "checking-managed-server",
          detail: [
            "managedServer",
            `${requestedName} is running as ${shortDid(status.nodeDid)}, with its identity and data in ${root}`,
          ],
        });
      }
      dispatch({ type: "reached", phase: "loading-configuration" });
      failedPhase = "configuration-error";
      await initLocalRuntime(requestedName);
      dispatch({
        type: "reached",
        phase: "starting-client",
        detail: ["configuration", `Saved the local connection to ${requestedName}`],
      });
      failedPhase = "client-error";
      await actions.commitLocalServerAutoStart(requestedName);
      await actions.awaitLocalServerPairing();
      await finishProvisioning(nodeSetUp);
    } catch (e) {
      dispatch({ type: "failed", phase: failedPhase, error: setupErrorMessage(e) });
      await incompatibleHome?.adopt(e);
    } finally {
      dispatch({ type: "ended" });
    }
  };
  const enrol = async () => {
    dispatch({ type: "begun", phase: "starting-client" });
    try {
      await actions.requestStatusEnrollment(address.trim());
      await finishProvisioning((snapshot) => snapshot.client?.deployments[0]);
    } catch (e) {
      dispatch({ type: "failed", phase: "client-error", error: setupErrorMessage(e) });
    } finally {
      dispatch({ type: "ended" });
    }
  };
  if (step === "inference")
    return (
      <InferenceSetup
        purpose="onboarding"
        checkRuntime={initialStep === "inference"}
        onDone={onDone}
        nodeDid={nodeDid}
        onCancel={onCancel}
        provider={provider}
        onBack={
          initialStep === "inference" ? undefined : () => dispatch({ type: "wentBack" })
        }
      />
    );
  if (step === "welcome") {
    return (
      <Frame>
        <Mark className="mb-6 h-6 text-ink" />
        <Title note="Gents keeps a record of every step an agent takes, so you can see what it did and it can pick up where it left off. Start a node here, or connect to one that already runs.">
          Let’s get set up
        </Title>
        <div className="grid gap-3">
          {allowLocal && (
            <Option
              selected={where === "local"}
              onSelect={() => setWhere("local")}
              title="Local node"
              hint={
                existingHome
                  ? "Continue the node already on this computer."
                  : "Create a node that runs independently in the background."
              }
              icon={Server}
            >
              <div className="flex items-end gap-3">
                <NodeAvatar name={nodeName} className="mb-1 size-8 shrink-0" />
                <div className="min-w-0 flex-1">
                  <Field label="Node name">
                    <Input
                      value={nodeName}
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
                  validateRoot={actions.validateLocalServerRoot}
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
                          void readSuggestedRoot();
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
                Node data: <span className="font-mono">{root}</span>
              </p>
              {existingHome ? (
                <p className="text-xs text-muted-foreground">
                  Found an existing Gents home
                  {bootstrap?.initNodeName ? ` for ${bootstrap?.initNodeName}` : ""}.
                  Next keeps that identity, native service, and reviewed host authority.
                </p>
              ) : null}
              <p className="text-xs text-muted-foreground">
                Your operating system manages the node as a background service. Closing
                this window or choosing Quit Desktop leaves the node running; use the
                menu bar’s Stop Node command to stop it. Start-at-login remains a
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
          next={where === "local" ? createNode : enrol}
          nextLabel={where === "local" ? "Next" : "Request access"}
          busy={busy}
          disabled={
            where === "local"
              ? !nodeName.trim() || !authority || !homeRoot
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
      "checking-managed-server": "Starting your local node…",
      "loading-configuration": "Loading node configuration…",
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
              label: where === "local" ? "Start local node" : "Connect to server",
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
          onRetry={() => dispatch({ type: "retried" })}
          onContinue={() => dispatch({ type: "continued" })}
          onOpenLoginItems={
            actions.localServerOffers.loginItems
              ? actions.openLocalServerLoginItems
              : undefined
          }
          diagnosticsHint={diagnosticsHint ?? bootstrap?.diagnosticsHint}
        />
      </Frame>
    );
  }
  return null;
}
