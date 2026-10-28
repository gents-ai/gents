import { useEffect, useReducer, useState } from "react";
import { useStore } from "zustand";
import { ArrowRight, Server, Wifi } from "lucide-react";
import type {
  DesktopClientSnapshot,
  ManagedServerAuthorityInput,
  EnrollmentRequestView,
} from "@source-inc/gents-desktop-client";
import { DEFAULT_NODE_NAME } from "@source-inc/gents-desktop-fleet";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { Spinner } from "@gents/ui/components/spinner";
import {
  projectStartupLoadingStatus,
  startupSteps,
  type DesktopStartupPhase,
} from "../../../lib/loadingStatus";
import { SETUP_COMPLETE_DWELL_MS, StartupProgress } from "./StartupProgress";
import { engineerPatches } from "@/lib/engineer";
import { supportsLocalManagedServer } from "../../../lib/shellPlatform";
import { NodeAvatar } from "@/screens/AgentAvatar";
import { Mark } from "@/app/Mark";
import { setupErrorMessage } from "../../../lib/setupErrors";
import { ManagedRuntimeAuthorityPicker } from "@/components/ManagedRuntimeAuthority";
import { authoritiesEqual, authorityForSelection } from "@/lib/managedRuntimeAuthority";
import { useApp } from "@/app/AppContext";
import { useBootstrap, useEnrollmentRequests, useStartup } from "@/hooks/useClient";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { shortPath } from "@/lib/paths";
import { Field, Frame, Nav, Option, Title } from "./parts";
import { InferenceSetup } from "./InferenceSetup";
import { nodeSetUp } from "@/lib/firstRun";
import type { NodeView } from "../../../hooks/fleetStore";
import type { ProviderId } from "./inferenceSetupForm";

type Step = "welcome" | "starting" | "enrollment" | "inference";
type FailedPhase = Exclude<DesktopStartupPhase, "ready">;
type StartupDetail = "managedServer" | "configuration" | "client";

type Run = {
  step: Step;
  busy: boolean;
  error: string | null;
  phase: FailedPhase;
  details: Partial<Record<StartupDetail, string>>;
  startedDid: string | null;
  readyName: string | null;
  provisionedAt: number | null;
};

type RunEvent =
  | { type: "begun"; phase: FailedPhase }
  | {
      type: "reached";
      phase: FailedPhase;
      detail?: [StartupDetail, string];
      startedDid?: string | null;
    }
  | { type: "provisioned"; at: number; client: string; name: string | null }
  | { type: "failed"; phase: FailedPhase; error: string }
  | { type: "ended" }
  | { type: "retried" }
  | { type: "wentBack" }
  | { type: "continued" }
  | { type: "requested" }
  | { type: "requesting" };

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
        startedDid: null,
        readyName: null,
        provisionedAt: null,
      };
    case "reached":
      return {
        ...run,
        phase: event.phase,
        details: event.detail
          ? { ...run.details, [event.detail[0]]: event.detail[1] }
          : run.details,
        startedDid: event.startedDid === undefined ? run.startedDid : event.startedDid,
      };
    case "provisioned":
      return {
        ...run,
        details: { ...run.details, client: event.client },
        readyName: event.name,
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
    case "requesting":
      return { ...run, busy: true, error: null };
    case "requested":
      return { ...run, step: "enrollment", busy: false, error: null };
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
  onDone: (snapshot: DesktopClientSnapshot, nodeDid?: string) => void;
  initialStep?: Step;
  nodeDid?: string;
  onCancel?: () => void;
  provider?: ProviderId;
}) {
  const bootstrap = useBootstrap();
  const home = useHomeFolder();
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
    startedDid: null,
    readyName: null,
    provisionedAt: null,
  });
  const {
    step,
    busy,
    error,
    phase,
    details: startupDetails,
    startedDid,
    readyName,
    provisionedAt,
  } = run;
  const allowLocal = supportsLocalManagedServer();
  const [where, setWhere] = useState<"local" | "remote">(
    allowLocal ? "local" : "remote",
  );
  const [address, setAddress] = useState("");
  const enrollmentRequests = useEnrollmentRequests();
  const snapshot = useStore(stores.client, (state) => state.snapshot);
  const [enrollment, setEnrollment] = useState<EnrollmentRequestView | null>(null);
  const existingHome = Boolean(
    bootstrap?.nodeHomeExists && bootstrap?.initNodeDid?.trim(),
  );
  const [name, setName] = useState(
    bootstrap?.initNodeName?.trim() || DEFAULT_NODE_NAME,
  );
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
  const [readyShownAt, setReadyShownAt] = useState<number | null>(null);
  useEffect(() => {
    if (provisionedAt === null) setReadyShownAt(null);
  }, [provisionedAt]);
  useEffect(() => {
    if (readyShownAt === null) return;
    const timer = window.setTimeout(
      () => dispatch({ type: "continued" }),
      Math.max(0, readyShownAt + SETUP_COMPLETE_DWELL_MS - Date.now()),
    );
    return () => window.clearTimeout(timer);
  }, [readyShownAt]);
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
      name: nextDeployment?.label ?? null,
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
          detail: ["managedServer", `${requestedName} is running`],
          startedDid: status.nodeDid,
        });
      }
      dispatch({ type: "reached", phase: "loading-configuration" });
      failedPhase = "configuration-error";
      await initLocalRuntime(requestedName);
      dispatch({
        type: "reached",
        phase: "starting-client",
        detail: ["configuration", `Saved the connection to ${requestedName}`],
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
    dispatch({ type: "requesting" });
    try {
      const request = await actions.requestStatusEnrollment(address.trim());
      setEnrollment(request);
      dispatch({ type: "requested" });
    } catch (e) {
      dispatch({ type: "failed", phase: "client-error", error: setupErrorMessage(e) });
    } finally {
      dispatch({ type: "ended" });
    }
  };
  if (step === "enrollment" && enrollment) {
    const active = enrollmentRequests?.find(
      (request) => request.requestId === enrollment.requestId,
    );
    const target = snapshot?.client?.deployments.find(
      (deployment) => deployment.nodeDid === enrollment.ownerNode,
    );
    const ready = Boolean(target?.chatSafe);
    const observed = enrollmentRequests != null;
    const label = target?.label ?? enrollment.serverLabel ?? "your node";
    return (
      <Frame>
        <div role="status" aria-live="polite">
          <Title
            note={
              ready
                ? `Your secure connection to ${label} is ready.`
                : active?.state === "approved"
                  ? "Approval received. Finishing your secure connection…"
                  : !observed
                    ? "Approval status is unavailable. Refresh to check again."
                    : active
                      ? "Ask the node’s administrator to approve this device before the request expires. You can return to this request while it is active."
                      : "This request is no longer listed. If the connection does not become ready, request access again."
            }
          >
            {ready
              ? "Connected to your node"
              : !observed
                ? "Checking access"
                : active?.state === "approved"
                  ? "Connecting to your node"
                  : "Waiting for access"}
          </Title>
        </div>
        {!observed && (
          <Button variant="outline" onClick={() => void refreshSnapshot()}>
            Refresh status
          </Button>
        )}
        <div className="grid min-w-0 gap-3 text-sm">
          <p className="break-words">{label}</p>
          {!ready && (
            <p className="text-muted-foreground">
              Request expires:{" "}
              <time dateTime={enrollment.expiresAt}>
                {new Date(enrollment.expiresAt).toLocaleString()}
              </time>
            </p>
          )}
          <details className="min-w-0 text-muted-foreground">
            <summary className="cursor-pointer">Connection details</summary>
            <dl className="mt-2 grid gap-2 break-all">
              <div>
                <dt>Node</dt>
                <dd>{enrollment.ownerNode}</dd>
              </div>
              <div>
                <dt>Request</dt>
                <dd>{enrollment.requestId}</dd>
              </div>
            </dl>
          </details>
        </div>
        <Nav
          onBack={() => {
            setEnrollment(null);
            dispatch({ type: "wentBack" });
          }}
          next={() => {
            const current = stores.client.getState().snapshot;
            if (
              current?.client?.deployments.some(
                (deployment) =>
                  deployment.nodeDid === enrollment.ownerNode && deployment.chatSafe,
              )
            )
              onDone(current, enrollment.ownerNode);
          }}
          nextLabel="Continue"
          disabled={!ready}
        />
      </Frame>
    );
  }
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
        <Mark className="mb-6 h-5 w-7 text-ink" />
        <Title
          note={
            allowLocal
              ? "Start a node on this computer, or connect to a node that already runs."
              : "Connect this phone to Gents running on your computer or server. Your agents and their history stay on that node."
          }
        >
          {allowLocal ? "Let’s get set up" : "Connect to your node"}
        </Title>
        {!allowLocal &&
          snapshot?.client?.deployments
            .filter((node) => node.chatSafe)
            .map((node) => (
              <Button
                key={node.nodeDid}
                className="mb-3 w-full"
                variant="outline"
                onClick={() => onDone(snapshot, node.nodeDid)}
              >
                <span className="truncate">Continue to {node.label}</span>
              </Button>
            ))}
        {!allowLocal && enrollmentRequests && enrollmentRequests.length > 0 && (
          <div className="mb-6 grid min-w-0 gap-2">
            <p className="text-sm text-muted-foreground">
              Saved access requests. If the administrator cannot find your request,
              enter the node’s status address below to send it again.
            </p>
            {enrollmentRequests.map((request) => (
              <Button
                key={request.requestId}
                className="min-w-0 w-full"
                variant="outline"
                onClick={() => {
                  setEnrollment(request);
                  dispatch({ type: "requested" });
                }}
              >
                <span className="truncate">
                  View request to {request.serverLabel ?? request.ownerNode}
                </span>
              </Button>
            ))}
          </div>
        )}
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
                  <Field
                    label="Node name"
                    htmlFor="setup-node-name"
                    info={
                      <>
                        Its identity and data live in{" "}
                        <span className="font-mono break-all">{root}</span>. Your
                        operating system runs it as a background service: closing this
                        window or quitting Desktop leaves it running, and the menu bar’s
                        Stop Node stops it. Starting at login is a separate setting.
                      </>
                    }
                  >
                    <Input
                      id="setup-node-name"
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
                Node data:{" "}
                <span className="font-mono" title={root}>
                  {shortPath(root, { home })}
                </span>
              </p>
              {existingHome ? (
                <p className="text-xs text-muted-foreground">
                  Found an existing Gents home
                  {bootstrap?.initNodeName ? ` for ${bootstrap?.initNodeName}` : ""}.
                  Next keeps that identity, native service, and reviewed host authority.
                </p>
              ) : null}
            </Option>
          )}
          <Option
            selected={where === "remote"}
            onSelect={() => setWhere("remote")}
            title="Remote connect"
            hint="Connect to an existing Gents node."
            icon={Wifi}
          >
            <form
              className="grid min-w-0 gap-3"
              onSubmit={(event) => {
                event.preventDefault();
                if (!busy && address.trim()) void enrol();
              }}
            >
              <Field label="Server address">
                <Input
                  type="url"
                  enterKeyHint="go"
                  autoCapitalize="none"
                  autoCorrect="off"
                  value={address}
                  onChange={(event) => setAddress(event.target.value)}
                  placeholder="https://gents.example.net:8787"
                />
              </Field>
              <p className="text-xs text-muted-foreground">
                Enter the node’s status address, reachable from this device. The node’s
                administrator must approve this device’s access request.
              </p>
            </form>
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
    const detail = {
      node: startupDetails.managedServer,
      connections: startupDetails.configuration,
      client: startupDetails.client,
    };
    return (
      <Frame>
        <StartupProgress
          title={status.title}
          doneTitle="Gents is ready"
          summary={
            readyName
              ? {
                  title:
                    where === "local"
                      ? `${readyName} is running`
                      : `Connected to ${readyName}`,
                  facts: [
                    ...(startedDid
                      ? [
                          {
                            label: "Identity",
                            value: shortDid(startedDid),
                            full: startedDid,
                          },
                        ]
                      : []),
                    ...(where === "local" ? [{ label: "Data", value: root }] : []),
                  ],
                }
              : undefined
          }
          steps={startupSteps(status, { remote: where !== "local" }).map((s) => ({
            ...s,
            state: done ? "complete" : s.state,
            detail: detail[s.key] ?? null,
          }))}
          failedLabel={status.currentLabel}
          done={done}
          wait={managedWait}
          waitTestId="setup-managed-server-wait"
          loginItemsTestId="setup-open-login-items"
          onOpenLoginItems={
            actions.localServerOffers.loginItems
              ? actions.openLocalServerLoginItems
              : undefined
          }
          error={error}
          diagnosticsHint={diagnosticsHint ?? bootstrap?.diagnosticsHint}
          actions={
            <Button variant="brand" onClick={() => dispatch({ type: "retried" })}>
              Try again
            </Button>
          }
          onFinished={() => setReadyShownAt(Date.now())}
          footer={
            <Button
              data-testid="setup-continue"
              variant="brand"
              onClick={() => dispatch({ type: "continued" })}
            >
              Continue <ArrowRight />
            </Button>
          }
        />
      </Frame>
    );
  }
  return null;
}
