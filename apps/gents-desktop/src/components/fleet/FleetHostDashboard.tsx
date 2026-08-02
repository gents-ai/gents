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
  onGrokLogin: (agentDid: string) => Promise<GrokLoginResult>;
  onCancelGrokLogin: () => Promise<unknown>;
};

export function FleetHostDashboard({
  onInitLocalRuntime,
  onSaveBackendConfig,
  onSaveBehaviorConfig,
  onProbeInferenceEndpoint,
  onCodexLogin,
  onCancelCodexLogin,
  onGrokLogin,
  onCancelGrokLogin,
  ...fleetProps
}: FleetHostDashboardProps) {
  return (
    <FleetDashboard
      {...fleetProps}
      brand={<BrandLockup />}
      copy={{
        ...fleetProps.copy,
        pairingQrHint: fleetProps.copy?.pairingQrHint ?? (
          <>
            Point the camera at the QR code printed by{" "}
            <code>gents p2p pairings invite --bearer --qr</code>.
          </>
        ),
      }}
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
