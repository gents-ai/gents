import { listen } from "@tauri-apps/api/event";
import {
  FleetDashboard,
  type FleetDashboardProps,
} from "@source-inc/gents-desktop-fleet";
import {
  InferenceSetupWizard,
  LocalRuntimeConnect,
} from "@source-inc/gents-desktop-fleet/local-runtime";
import type {
  BackendSaveRequest,
  BehaviorSaveRequest,
  CodexLoginResult,
  CodexLoginUrl,
  GrokLoginResult,
  GrokLoginUrl,
  InferenceProbeResult,
  SyncHealthView,
} from "@source-inc/gents-desktop-client";

import { ThemeToggle } from "../ThemeToggle";
import { SyncHealthIndicator } from "../SyncHealthIndicator";
import { isMobileTauriShell } from "../../lib/shellPlatform";
import { BrandLockup } from "./BrandLockup";

export type FleetHostDashboardProps = Omit<
  FleetDashboardProps,
  "brand" | "headerLeadingActions" | "localRuntimeSetup" | "renderInferenceSetup"
> & {
  onInitLocalRuntime: (label?: string | null) => Promise<unknown>;
  onStartManagedServer?: (agentName: string) => Promise<unknown>;
  onCommitManagedServerAutoStart?: (agentName: string) => Promise<unknown>;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onProbeInferenceEndpoint: (endpoint: string) => Promise<InferenceProbeResult>;
  onCodexLogin: (agentDid: string) => Promise<CodexLoginResult>;
  onCancelCodexLogin: () => Promise<unknown>;
  onGrokLogin: (agentDid: string) => Promise<GrokLoginResult>;
  onCancelGrokLogin: () => Promise<unknown>;
  syncHealth?: SyncHealthView | null;
};

export function FleetHostDashboard({
  onInitLocalRuntime,
  onStartManagedServer,
  onCommitManagedServerAutoStart,
  onSaveBackendConfig,
  onSaveBehaviorConfig,
  onProbeInferenceEndpoint,
  onCodexLogin,
  onCancelCodexLogin,
  onGrokLogin,
  onCancelGrokLogin,
  syncHealth = null,
  ...fleetProps
}: FleetHostDashboardProps) {
  const supportsLocalRuntime = !isMobileTauriShell();
  const indicator = <SyncHealthIndicator syncHealth={syncHealth} />;
  return (
    <FleetDashboard
      {...fleetProps}
      syncHealth={syncHealth}
      brand={
        <div className="fleet-brand-row">
          <BrandLockup />
          {fleetProps.deployments.length === 0 ? indicator : null}
        </div>
      }
      copy={fleetProps.copy}
      headerLeadingActions={
        <>
          {fleetProps.deployments.length > 0 ? indicator : null}
          <ThemeToggle />
        </>
      }
      localRuntimeSetup={
        supportsLocalRuntime ? (
          <LocalRuntimeConnect
            bootstrap={fleetProps.bootstrap}
            busy={fleetProps.addingPeer || fleetProps.starting}
            loading={fleetProps.loading}
            copy={fleetProps.copy}
            onConnect={onInitLocalRuntime}
            onStartServer={onStartManagedServer}
            onCommitServerAutoStart={onCommitManagedServerAutoStart}
          />
        ) : undefined
      }
      renderInferenceSetup={(deployment, onClose) => (
        <InferenceSetupWizard
          deployment={deployment}
          onClose={onClose}
          onSaveBackendConfig={onSaveBackendConfig}
          onSaveBehaviorConfig={onSaveBehaviorConfig}
          onProbeInferenceEndpoint={onProbeInferenceEndpoint}
          onCodexLogin={onCodexLogin}
          onCancelCodexLogin={onCancelCodexLogin}
          onCodexLoginUrl={async (onUrl) =>
            listen<CodexLoginUrl>("desktop://codex-login-url", (event) =>
              onUrl(event.payload?.url ?? null),
            )
          }
          onGrokLogin={onGrokLogin}
          onCancelGrokLogin={onCancelGrokLogin}
          onGrokLoginUrl={async (onUrl) =>
            listen<GrokLoginUrl>("desktop://grok-login-url", (event) =>
              onUrl(event.payload?.url ?? null),
            )
          }
        />
      )}
    />
  );
}
