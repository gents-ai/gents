import { useEffect, useState } from "react";

import { Button } from "@gents/ui/components/button";

import { errorText } from "../lib/errorText";
import { projectStartupLoadingStatus, startupSteps } from "../lib/loadingStatus";
import { continueLabel } from "../lib/remoteNodes";
import { useApp } from "../ui/app/AppContext";
import { Mark } from "../ui/app/Mark";
import { useStartup } from "../ui/hooks/useClient";
import { StartupProgress } from "../ui/screens/setup/StartupProgress";

export function StartupScreen({
  configurationPending = false,
  initConfigStatus,
}: {
  configurationPending?: boolean;
  initConfigStatus?: "absent" | "configured" | "unreadable" | null;
}) {
  const { actions, lifecycle, stores } = useApp();
  const startup = useStartup();
  const { watchLocalServer } = actions;
  const localNode = stores.localServer.use.status();
  const [connecting, setConnecting] = useState(false);
  const [connectError, setConnectError] = useState<string | null>(null);
  const waitsOnLocalNode = configurationPending && initConfigStatus === "configured";
  useEffect(
    () => (waitsOnLocalNode ? watchLocalServer() : undefined),
    [waitsOnLocalNode, watchLocalServer],
  );
  /* a node set up outside this app serves without this desktop's enrollment,
     so its settings never arrive until the desktop is connected */
  const unconnected =
    waitsOnLocalNode &&
    (localNode?.state === "running" || localNode?.state === "external") &&
    !localNode?.pairingReady;
  const connect = async () => {
    setConnecting(true);
    setConnectError(null);
    try {
      await actions.enrollLocalServer();
      await actions.refreshSnapshot();
    } catch (cause) {
      setConnectError(errorText(cause));
    } finally {
      setConnecting(false);
    }
  };
  if (configurationPending && unconnected) {
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
              onClick={() => void actions.refreshSnapshot()}
            >
              Try again
            </Button>
          </div>
        </div>
      </section>
    );
  }
  if (configurationPending) {
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
              {initConfigStatus === "unreadable"
                ? "Unable to read saved node settings"
                : "Loading saved node settings"}
            </h1>
            <p aria-live="polite" className="text-sm text-muted-foreground">
              {initConfigStatus === "unreadable"
                ? "The saved node settings could not be read. Try again to reload them."
                : "Waiting for your saved settings and sessions."}
            </p>
          </div>
          <Button
            variant="outline"
            className="w-fit"
            onClick={() => void actions.refreshSnapshot()}
          >
            Try again
          </Button>
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
              {phase === "managed-server-error" &&
                skip("startup-continue-without-managed-server")}
            </>
          }
        />
      </div>
    </section>
  );
}
