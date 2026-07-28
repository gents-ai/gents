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
  InferenceProbeResult,
} from "@source-inc/gents-desktop-client";

import { ThemeToggle } from "../ThemeToggle";
import { BrandLockup } from "./BrandLockup";

export type FleetHostDashboardProps = Omit<
  FleetDashboardProps,
  "brand" | "headerLeadingActions" | "localRuntimeSetup" | "renderInferenceSetup"
> & {
  onInitLocalRuntime: (label?: string | null) => Promise<unknown>;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onProbeInferenceEndpoint: (endpoint: string) => Promise<InferenceProbeResult>;
  onCodexLogin: (agentDid: string) => Promise<CodexLoginResult>;
  onCancelCodexLogin: () => Promise<unknown>;
};

/**
 * Gents-specific fleet composition.
 *
 * Branding, theme controls, and runtime-admin capabilities stay in the host;
 * reusable discovery, pairing, health, and peer management live in the package.
 */
export function FleetHostDashboard({
  onInitLocalRuntime,
  onSaveBackendConfig,
  onSaveBehaviorConfig,
  onProbeInferenceEndpoint,
  onCodexLogin,
  onCancelCodexLogin,
  ...fleetProps
}: FleetHostDashboardProps) {
  return (
    <FleetDashboard
      {...fleetProps}
      brand={<BrandLockup />}
      headerLeadingActions={<ThemeToggle />}
      localRuntimeSetup={
        <LocalRuntimeConnect
          bootstrap={fleetProps.bootstrap}
          busy={fleetProps.addingPeer || fleetProps.starting || fleetProps.loading}
          copy={fleetProps.copy}
          onConnect={onInitLocalRuntime}
        />
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
        />
      )}
    />
  );
}
