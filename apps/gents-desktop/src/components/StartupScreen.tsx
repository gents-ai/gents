import { useEffect, useState } from "react";

import { Button } from "@gents/ui/components/button";

import { connectNodeState, connectScreen } from "../lib/connectScreen";
import { errorText } from "../lib/errorText";
import { projectStartupLoadingStatus, startupSteps } from "../lib/loadingStatus";
import type { StartupGate } from "../lib/startupGate";
import { continueLabel } from "../lib/remoteNodes";
import { useApp } from "../ui/app/AppContext";
import { Mark } from "../ui/app/Mark";
import { useStartup } from "../ui/hooks/useClient";
import { StartupProgress } from "../ui/screens/setup/StartupProgress";

/** `gate`: a ready app's screen that asks something of the person (see
    `projectStartupGate`); without one, the startup progress until ready. */
export function StartupScreen({ gate }: { gate?: Exclude<StartupGate, "shell"> }) {
  const { actions, lifecycle, stores } = useApp();
  const startup = useStartup();
  const { watchLocalServer } = actions;
  const localNode = stores.localServer.use.status();
  const [connecting, setConnecting] = useState(false);
  const [connectError, setConnectError] = useState<string | null>(null);
  const unconnected = gate === "not_connected";
  useEffect(
    () => (unconnected ? watchLocalServer() : undefined),
    [unconnected, watchLocalServer],
  );
  const refusal = localNode
    ? connectScreen(connectNodeState(localNode), localNode.pairingReady)[1]
    : null;
  const blocked =
    refusal === "node_not_running"
      ? "The node on this computer is not running. Start it in a terminal with `gents runtime server`, then choose Try again."
      : refusal
        ? (localNode?.error ?? "The node on this computer cannot be connected to yet.")
        : null;
  const reread = () =>
    Promise.all([actions.refreshSnapshot(), actions.refreshLocalServer()]);
  const retry = () => void reread();
  const connect = async () => {
    setConnecting(true);
    setConnectError(null);
    try {
      await actions.enrollLocalServer();
      await reread();
    } catch (cause) {
      if (!(await startup.incompatibleHome.adopt(cause))) {
        setConnectError(errorText(cause));
      }
    } finally {
      setConnecting(false);
    }
  };
  const home = startup.incompatibleHome;
  const startFresh = home.canFullReset && (
    <Button
      variant="outline"
      data-testid="startup-start-fresh"
      disabled={home.busy}
      onClick={() => void home.previewReset()}
    >
      Start fresh…
    </Button>
  );
  if (unconnected) {
    return (
      <section
        aria-labelledby="startup-title"
        className="viewport-frame grid place-items-center bg-background px-8 text-foreground"
        data-testid="startup-configuration-pending"
      >
        <div className="grid w-full max-w-md gap-6">
          <Mark className="h-5 w-7 text-ink" />
          <div className="grid gap-2">
            <h1
              id="startup-title"
              className="font-heading text-xl font-medium text-heading"
            >
              This desktop is not connected to your node
            </h1>
            <p aria-live="polite" className="text-sm text-muted-foreground">
              The node on this computer was set up outside this app, so the app cannot
              read its settings and sessions yet. Connect this desktop to the node with
              the node owner’s approval.
            </p>
            {blocked && (
              <p
                role="status"
                className="text-sm wrap-break-word text-muted-foreground"
                data-testid="startup-connect-blocked"
              >
                {blocked}
              </p>
            )}
            {connectError && (
              <p
                role="alert"
                className="text-sm wrap-break-word text-destructive"
                data-testid="startup-connect-error"
              >
                Couldn’t connect this desktop: {connectError}
              </p>
            )}
          </div>
          <div className="flex gap-2">
            <Button
              variant="brand"
              disabled={connecting}
              data-testid="startup-connect-desktop"
              onClick={() => void connect()}
            >
              {connecting ? "Connecting this desktop…" : "Connect this desktop"}
            </Button>
            <Button
              variant="outline"
              disabled={connecting}
              data-testid="startup-connect-retry"
              onClick={retry}
            >
              Try again
            </Button>
          </div>
        </div>
      </section>
    );
  }
  if (gate === "unreadable") {
    return (
      <section
        aria-labelledby="startup-title"
        className="viewport-frame grid place-items-center bg-background px-8 text-foreground"
        data-testid="startup-configuration-pending"
      >
        <div className="grid w-full max-w-md gap-6">
          <Mark className="h-5 w-7 text-ink" />
          <div className="grid gap-2">
            <h1
              id="startup-title"
              className="font-heading text-xl font-medium text-heading"
            >
              Unable to read saved node settings
            </h1>
            <p aria-live="polite" className="text-sm text-muted-foreground">
              The saved node settings could not be read. Try again to reload them.
            </p>
          </div>
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => void actions.refreshSnapshot()}>
              Try again
            </Button>
            {startFresh}
          </div>
        </div>
      </section>
    );
  }
  if (startup.phase === "ready") return null;
  const { phase, managedServerWait } = startup;
  const status = projectStartupLoadingStatus(phase, lifecycle.supportsManagedServer);
  const wait =
    managedServerWait && phase === "checking-managed-server" ? managedServerWait : null;
  const label = continueLabel(startup.repairPeers);
  const skip = (testId: string) =>
    label && (
      <Button
        variant="outline"
        data-testid={testId}
        onClick={lifecycle.skipManagedServerWait}
      >
        {label}
      </Button>
    );

  return (
    <section
      aria-labelledby="startup-title"
      className="viewport-frame grid place-items-center bg-background px-8 text-foreground"
      data-testid="startup-screen"
    >
      <div className="w-full max-w-md">
        <StartupProgress
          title={status.title}
          titleId="startup-title"
          steps={startupSteps(status)}
          failedLabel={status.currentLabel}
          wait={wait}
          waitTestId="startup-managed-server-wait"
          loginItemsTestId="startup-open-login-items"
          onOpenLoginItems={
            actions.localServerOffers.loginItems
              ? actions.openLocalServerLoginItems
              : undefined
          }
          waitActions={skip("startup-skip-managed-server-wait")}
          error={
            status.failed ? (startup.error ?? "Gents could not finish starting.") : null
          }
          diagnosticsHint={startup.diagnosticsHint}
          actions={
            <>
              <Button
                variant="brand"
                data-testid="startup-retry"
                onClick={() => void lifecycle.retryStartup()}
              >
                Try again
              </Button>
              {status.failed && startFresh}
              {phase === "managed-server-error" &&
                skip("startup-continue-without-managed-server")}
            </>
          }
        />
      </div>
    </section>
  );
}
