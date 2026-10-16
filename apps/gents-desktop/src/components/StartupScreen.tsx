import { Button } from "@gents/ui/components/button";

import { projectStartupLoadingStatus, startupSteps } from "../lib/loadingStatus";
import { useApp } from "../ui/app/AppContext";
import { Mark } from "../ui/app/Mark";
import { useStartup } from "../ui/hooks/useClient";
import { StartupProgress } from "../ui/screens/setup/StartupProgress";

/** Where startup has got to, until the client is ready: the step under
    way, the managed server's wait, and on failure the ways on. */
export function StartupScreen({
  configurationPending = false,
  initConfigStatus,
}: {
  configurationPending?: boolean;
  initConfigStatus?: "absent" | "configured" | "unreadable" | null;
}) {
  const { actions, lifecycle } = useApp();
  const startup = useStartup();
  if (configurationPending) {
    /* drawn like the startup progress around it: the mark, one title and
       one line, no eyebrow */
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
  const skip = (testId: string) => (
    <Button
      variant="outline"
      data-testid={testId}
      onClick={lifecycle.skipManagedServerWait}
    >
      Continue without the local node
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
              {phase === "managed-server-error" && (
                <>
                  {startup.canRestartManagedServer && (
                    <Button
                      variant="outline"
                      data-testid="startup-restart-managed-server"
                      onClick={() => void lifecycle.restartManagedServer()}
                    >
                      Restart node
                    </Button>
                  )}
                  {skip("startup-continue-without-managed-server")}
                </>
              )}
            </>
          }
        />
      </div>
    </section>
  );
}
